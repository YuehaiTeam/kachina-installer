// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod cli;
pub mod dfs;
pub mod fs;
pub mod installer;
pub mod ipc;
pub mod local;
pub mod module;
pub mod thirdparty;
pub mod utils;

use clap::Parser;
use cli::arg::{Command, InstallArgs};
use installer::uninstall::delete_self_on_exit;
use sentry_tracing::EventFilter;
use std::time::Duration;
use tauri::{window::Color, Manager, WindowEvent};
use tauri_utils::{config::WindowEffectsConfig, WindowEffect};
use tracing_subscriber::prelude::*;
use utils::sentry::sentry_init;

lazy_static::lazy_static! {
    pub static ref REQUEST_CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent(ua_string())
        .gzip(true)
        .zstd(true)
        .read_timeout(Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
}

fn ua_string() -> String {
    let winver = nt_version::get();
    let cpu_cores = num_cpus::get();
    let wv2ver = tauri::webview_version();
    let wv2ver = if let Ok(ver) = wv2ver {
        ver
    } else {
        "Unknown".to_string()
    };
    format!(
        "KachinaInstaller/{} Webview2/{} Windows/{}.{}.{} Threads/{}",
        env!("CARGO_PKG_VERSION"),
        wv2ver,
        winver.0,
        winver.1,
        winver.2 & 0xffff,
        cpu_cores
    )
}

fn main() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };

    let cli = cli::Cli::parse();
    let mut command = cli.command();
    let wv2ver = tauri::webview_version();
    if wv2ver.is_err() {
        command = Command::InstallWebview2;
    }
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
    // command is not  Command::Install, can be anything
    match command {
        Command::Install(install) => {
            sentry::add_breadcrumb(sentry::Breadcrumb {
                category: Some("app".into()),
                message: Some("KachinaInstaller started".into()),
                level: sentry::Level::Info,
                ..Default::default()
            });
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(tauri_main(install));
        }
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
                .block_on(module::wv2::install_webview2());
        }
    }
}

async fn tauri_main(args: InstallArgs) {
    tauri::async_runtime::set(tokio::runtime::Handle::current());
    let (major, minor, build) = nt_version::get();
    let build = (build & 0xffff) as u16;
    let is_lower_than_win10 = major < 10;
    if is_lower_than_win10 {
        rfd::MessageDialog::new()
            .set_title("错误")
            .set_description("不支持的操作系统版本")
            .show();
        return;
    }
    // use 22000 as the build number of Windows 11
    let is_win11 = major == 10 && minor == 0 && build >= 22000;
    let is_win11_ = is_win11;

    // set cwd to temp dir
    let temp_dir = std::env::temp_dir();
    let res = std::env::set_current_dir(&temp_dir);
    if res.is_err() {
        rfd::MessageDialog::new()
            .set_title("错误")
            .set_description("无法访问临时文件夹")
            .show();
        return;
    }
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            // things which can be run directly
            fs::is_dir_empty,
            dfs::get_dfs,
            dfs::get_http_with_range,
            dfs::http_get_request,
            // DFS2 commands
            dfs::get_dfs2_metadata,
            dfs::create_dfs2_session,
            dfs::get_dfs2_chunk_url,
            dfs::get_dfs2_batch_chunk_urls,
            dfs::end_dfs2_session,
            dfs::solve_dfs2_challenge,
            installer::log,
            installer::warn,
            installer::error,
            installer::launch,
            installer::launch_and_exit,
            installer::config::get_installer_config,
            installer::lnk::get_dirs,
            installer::registry::read_uninstall_metadata,
            installer::select_dir,
            installer::error_dialog,
            installer::confirm_dialog,
            installer::get_exe_version,
            // wincred
            utils::wincred::wincred_write,
            utils::wincred::wincred_read,
            utils::wincred::wincred_delete,
            // mirrorc
            thirdparty::mirrorc::get_mirrorc_status,
            // new mamaned operation
            ipc::manager::managed_operation,
        ])
        .manage(args)
        .manage(ipc::manager::ManagedElevate::new())
        .setup(move |app| {
            // sleep 5s to check if window is alive
            tokio::spawn({
                let app_handle = app.app_handle().clone();
                async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let window = app_handle.get_webview_window("main");
                    // if window is not visible, there may be a js error or a webview2 fault
                    // throw a dialog and exit
                    let mut fault = false;
                    if let Some(window) = &window {
                        if let Ok(visible) = window.is_visible() {
                            if !visible {
                                fault = true;
                            }
                        } else {
                            fault = true;
                        }
                    } else {
                        fault = true;
                    }
                    if fault {
                        rfd::MessageDialog::new()
                            .set_title("Kachina Installer")
                            .set_description("Initialization failed due to webview2 fault")
                            .set_level(rfd::MessageLevel::Error)
                            .show();
                        tracing::error!("Webview2 fault detected");
                        std::process::exit(1);
                    } else {
                        tracing::info!("Webview2 is alive");
                    }
                }
            });
            let temp_dir_for_data = temp_dir.join("KachinaInstaller");
            let mut main_window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title(" ")
            .resizable(false)
            .maximizable(false)
            .transparent(true)
            .inner_size(520.0, 250.0)
            .center();
            if !cfg!(debug_assertions) {
                main_window = main_window.data_directory(temp_dir_for_data).visible(false);
            }
            let main_window = main_window.build().unwrap();
            #[cfg(debug_assertions)]
            {
                let window = tauri::Manager::get_webview_window(app, "main");
                if let Some(window) = window {
                    window.open_devtools();
                }
            }
            if is_win11 {
                let _ = main_window.set_effects(Some(WindowEffectsConfig {
                    effects: vec![WindowEffect::Mica],
                    ..Default::default()
                }));
            } else {
                // if mica is not available, just use solid background.
                let _ = if utils::gui::is_dark_mode().unwrap_or(false) {
                    main_window.set_background_color(Some(Color(0, 0, 0, 255)))
                } else {
                    main_window.set_background_color(Some(Color(255, 255, 255, 255)))
                };
            }
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let WindowEvent::ThemeChanged(theme) = event {
                if !is_win11_ {
                    match theme {
                        tauri::Theme::Dark => {
                            let _ = window.set_background_color(Some(Color(0, 0, 0, 255)));
                        }
                        tauri::Theme::Light => {
                            let _ = window.set_background_color(Some(Color(255, 255, 255, 255)));
                        }
                        _ => {}
                    }
                }
            }
            if let WindowEvent::CloseRequested { .. } = event {
                delete_self_on_exit();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
