// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod capabilities;
pub mod cli;
pub mod dfs;
pub mod fs;
pub mod host;
pub mod installer;
pub mod ipc;
pub mod local;
pub mod module;
pub mod session;
pub mod thirdparty;
pub mod utils;
use cli::arg::{Command, InstallArgs};
use sentry_tracing::EventFilter;
use std::{sync::atomic::AtomicBool, time::Duration};
use tracing_subscriber::prelude::*;
use utils::sentry::sentry_init;

pub(crate) fn windows_text_scale_factor() -> f64 {
    // Read TextScaleFactor from registry: HKEY_CURRENT_USER\Software\Microsoft\Accessibility\TextScaleFactor
    // The registry value is a DWORD representing percentage (e.g., 100 = 100%, 125 = 125%)
    windows_registry::CURRENT_USER
        .options()
        .read()
        .open("Software\\Microsoft\\Accessibility")
        .and_then(|key| key.get_u32("TextScaleFactor"))
        .ok()
        .map(|scale| scale as f64 / 100.0)
        .filter(|&scale| scale.is_finite() && scale > 0.0)
        .unwrap_or(1.0)
}

lazy_static::lazy_static! {
    /// Raw HTTP client without middleware (for internal use)
    pub(crate) static ref RAW_CLIENT: reqwest::Client = {
        reqwest::Client::builder()
            .user_agent(capabilities::ua_string()) // overwritten per-request by DynamicUaMiddleware
            .gzip(true)
            .zstd(true)
            .read_timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    };

    /// HTTP client for API calls — carries real-time dynamic UA
    pub static ref API_CLIENT: reqwest_middleware::ClientWithMiddleware = {
        reqwest_middleware::ClientBuilder::new(RAW_CLIENT.clone())
            .with(capabilities::DynamicUaMiddleware::new())
            .build()
    };

    /// HTTP client for downloads (supports H3/QUIC via middleware)
    pub static ref DOWNLOAD_CLIENT: reqwest_middleware::ClientWithMiddleware = {
        let h3_ok = capabilities::is_h3_available();

        let mut builder = reqwest_middleware::ClientBuilder::new(RAW_CLIENT.clone())
            .with(capabilities::DynamicUaMiddleware::new());

        if h3_ok {
            match capabilities::H3FallbackMiddleware::new(Duration::from_secs(60)) {
                Ok(h3mw) => {
                    builder = builder.with(h3mw);
                    tracing::info!("[H3] H3FallbackMiddleware enabled");
                }
                Err(e) => {
                    tracing::warn!("[H3] Middleware init failed: {:#}, disabling", e);
                    capabilities::disable_h3();
                }
            }
        }

        // Shared SSH connection pool for both SSH tunnel and SFTP middlewares
        let ssh_pool = std::sync::Arc::new(
            capabilities::ssh::SshPoolInner::new(Duration::from_secs(300)),
        );

        // SSH tunnel middleware — routes ssh+http:// URLs through SSH direct-tcpip channels
        builder = builder.with(capabilities::ssh::SshMiddleware::with_pool(ssh_pool.clone()));
        tracing::info!("[SSH] SshMiddleware enabled");

        // SFTP download middleware — routes sftp:// URLs through SSH SFTP subsystem
        builder = builder.with(capabilities::sftp::SftpMiddleware::new(ssh_pool));
        tracing::info!("[SFTP] SftpMiddleware enabled");

        builder.build()
    };

    /// Legacy alias - will be removed after migration
    pub static ref REQUEST_CLIENT: &'static reqwest_middleware::ClientWithMiddleware = &*API_CLIENT;
    pub static ref APP_BOOT_SIGNAL: AtomicBool = AtomicBool::new(false);
}

fn main() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };

    let command = cli::parse();
    let _guard = sentry_init(matches!(command, Command::HeadlessUac(_)));
    utils::sentry::sentry_set_info();
    let sentry_layer = sentry_tracing::layer().event_filter(|md| match *md.level() {
        tracing::Level::TRACE => EventFilter::Ignore,
        tracing::Level::DEBUG => EventFilter::Ignore,
        _ => EventFilter::Breadcrumb,
    });
    let info_filter = utils::sentry::InfoFilter {};

    // Create log file in temp directory, ignore failures
    let temp_dir = std::env::temp_dir();
    let log_file = temp_dir.join("KachinaInstaller.log");

    let console_layer = tracing_subscriber::fmt::layer().with_filter(utils::sentry::InfoFilter {});

    let registry = tracing_subscriber::registry()
        .with(sentry_layer)
        .with(console_layer);

    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file)
            .with_ansi(false)
            .with_filter(info_filter);
        registry.with(file_layer).init();
    } else {
        registry.init();
    }

    // Initialize H3/QUIC probe early — before any client is created
    capabilities::init();

    // command is not  Command::Install, can be anything
    match command {
        Command::HeadlessUac(args) => {
            sentry::add_breadcrumb(sentry::Breadcrumb {
                category: Some("app".into()),
                message: Some("KachinaInstaller started as UAC Thread".into()),
                level: sentry::Level::Info,
                ..Default::default()
            });
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(ipc::manager::uac_ipc_main(args));
        }
        Command::InstallWebview2 => {
            sentry::add_breadcrumb(sentry::Breadcrumb {
                category: Some("app".into()),
                message: Some("KachinaInstaller started as Webview2 Installer".into()),
                level: sentry::Level::Info,
                ..Default::default()
            });
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    if module::wv2::install_webview2().await.is_ok() {
                        host_main(InstallArgs::default(), None);
                    }
                });
        }
        Command::NativeUi(args) => {
            sentry::add_breadcrumb(sentry::Breadcrumb {
                category: Some("app".into()),
                message: Some("KachinaInstaller started (native-ui)".into()),
                level: sentry::Level::Info,
                ..Default::default()
            });
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    native_entry(args).await;
                });
        }
        Command::Install(install) => {
            sentry::add_breadcrumb(sentry::Breadcrumb {
                category: Some("app".into()),
                message: Some(if install.silent {
                    "KachinaInstaller started (silent)".into()
                } else {
                    "KachinaInstaller started".into()
                }),
                level: sentry::Level::Info,
                ..Default::default()
            });
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    if install.silent {
                        if let Err(err) = session::run::silent_main(install).await {
                            tracing::error!("silent install failed: {err:#}");
                            std::process::exit(1);
                        }
                    } else {
                        gui_entry(install).await;
                    }
                });
        }
    }
}

async fn gui_entry(args: InstallArgs) {
    if host::webview_version().is_ok() {
        host_main(args, None);
        return;
    }
    native_entry(args).await;
}

async fn native_entry(args: InstallArgs) {
    match host::native::run(args).await {
        Ok(host::native::NativeOutcome::Exit) | Ok(host::native::NativeOutcome::Again) => {}
        Ok(host::native::NativeOutcome::Web { args, preset }) => host_main(args, Some(preset)),
        Err(err) => {
            tracing::error!("native ui failed: {err:#}");
            rfd::MessageDialog::new()
                .set_title("错误")
                .set_description(format!("{err}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::exit(1);
        }
    }
}

fn host_main(args: InstallArgs, preset: Option<session::types::SessionInput>) {
    if let Err(err) = host::run(args, preset) {
        tracing::error!("gui host failed: {err:#}");
        rfd::MessageDialog::new()
            .set_title("错误")
            .set_description(format!("窗口初始化失败: {err}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
}
