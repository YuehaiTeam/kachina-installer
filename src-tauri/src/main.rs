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
use std::{sync::atomic::AtomicBool, time::Duration};

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

        // SSH tunnel middleware — routes ssh+http:// URLs through SSH direct-tcpip channels
        builder = builder.with(capabilities::ssh::SshMiddleware);
        tracing::info!("[SSH] SshMiddleware enabled");

        // SFTP download middleware — routes sftp:// URLs through SSH SFTP subsystem
        builder = builder.with(capabilities::sftp::SftpMiddleware);
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
    // 崩溃提示进程只弹框，不初始化遥测与网络探测
    if let Command::CrashDialog { event_id } = &command {
        crash_dialog(event_id.as_deref());
        return;
    }
    utils::sentry::init(matches!(command, Command::HeadlessUac(_)));
    utils::sentry::set_app_info();
    let show_crash_dialog = match &command {
        Command::Install(a) => !a.silent,
        _ => true,
    };
    utils::sentry::install_panic_hook(show_crash_dialog);

    // 日志：控制台 + %TEMP%\KachinaInstaller.log + Sentry 面包屑，INFO 级全局过滤
    utils::log::init(&std::env::temp_dir().join("KachinaInstaller.log"));

    // Initialize H3/QUIC probe early — before any client is created
    capabilities::init();

    // command is not  Command::Install, can be anything
    match command {
        Command::HeadlessUac(args) => {
            utils::sentry::add_breadcrumb(
                "app",
                "info",
                "KachinaInstaller started as UAC Thread".into(),
            );
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(ipc::manager::uac_ipc_main(args));
        }
        Command::InstallWebview2 => {
            utils::sentry::add_breadcrumb(
                "app",
                "info",
                "KachinaInstaller started as Webview2 Installer".into(),
            );
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    if module::wv2::install_webview2().await.is_ok() {
                        host_main(InstallArgs::default(), None, None);
                    }
                    utils::sentry::flush(Duration::from_secs(3));
                });
        }
        Command::NativeUi(args) => {
            utils::sentry::add_breadcrumb(
                "app",
                "info",
                "KachinaInstaller started (native-ui)".into(),
            );
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    native_entry(args).await;
                    utils::sentry::flush(Duration::from_secs(3));
                });
        }
        Command::CrashDialog { .. } => unreachable!("handled before telemetry init"),
        Command::Install(install) => {
            utils::sentry::add_breadcrumb(
                "app",
                "info",
                if install.silent {
                    "KachinaInstaller started (silent)".into()
                } else {
                    "KachinaInstaller started".into()
                },
            );
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    if install.silent {
                        if let Err(err) = session::run::silent_main(install).await {
                            tracing::error!("silent install failed: {}", utils::code::log_line(&err));
                            if utils::code::should_report_error(&err) {
                                let event_id = utils::sentry::capture_anyhow(&err);
                                tracing::error!("reported as event {event_id}");
                            }
                            utils::sentry::flush(Duration::from_secs(3));
                            std::process::exit(1);
                        }
                    } else {
                        gui_entry(install).await;
                    }
                    utils::sentry::flush(Duration::from_secs(3));
                });
        }
    }
}

fn crash_dialog(event_id: Option<&str>) {
    use crate::utils::i18n::t;
    use crate::utils::taskdialog::{task_dialog, CommandLink, TaskDialogRequest};
    let unknown = t("dialog.unknown_event", &[]);
    task_dialog(
        TaskDialogRequest {
            title: t("dialog.error", &[]),
            content: t("dialog.crash", &[("event_id", event_id.unwrap_or(&unknown))]),
            expanded: None,
            footer: None,
            buttons: vec![CommandLink {
                id: windows::Win32::UI::WindowsAndMessaging::IDOK.0,
                text: t("dialog.ok", &[]),
            }],
        },
        windows::Win32::Foundation::HWND::default(),
    );
}

async fn gui_entry(args: InstallArgs) {
    if host::webview_version().is_ok() {
        let gui = session::commands::prepare_gui(args.clone(), None).await;
        host_main(args, None, Some(gui));
        return;
    }
    native_entry(args).await;
}

async fn native_entry(args: InstallArgs) {
    match host::native::run(args).await {
        Ok(host::native::NativeOutcome::Exit) | Ok(host::native::NativeOutcome::Again { .. }) => {}
        Ok(host::native::NativeOutcome::Web { args, preset }) => {
                let gui = session::commands::prepare_gui(args.clone(), Some(preset.clone())).await;
                host_main(args, Some(preset), Some(gui));
            }
        Err(err) => {
            tracing::error!("native ui failed: {err:#}");
            fatal_error(&err);
        }
    }
}

fn host_main(
    args: InstallArgs,
    preset: Option<session::types::SessionInput>,
    gui: Option<std::sync::Arc<session::commands::GuiRuntime>>,
) {
    if let Err(err) = host::run(args, preset, gui) {
        tracing::error!("gui host failed: {err:#}");
        fatal_error(&err);
    }
}

/// Host-level failure with no renderer alive: report, show the default error
/// dialog with the event id, exit 1.
fn fatal_error(err: &anyhow::Error) -> ! {
    let event_id = if utils::code::should_report_error(err) {
        Some(utils::sentry::capture_anyhow(err))
    } else {
        None
    };
    if let Some(mut coded) = utils::code::coded_from_error(err) {
        coded.event_id = event_id;
        utils::taskdialog::show_error_coded(&coded, windows::Win32::Foundation::HWND::default());
    }
    utils::sentry::flush(Duration::from_secs(3));
    std::process::exit(1);
}
