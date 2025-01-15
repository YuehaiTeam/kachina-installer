// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod cli;
pub mod dfs;
pub mod fs;
pub mod installer;
pub mod ipc;
pub mod local;
pub mod utils;

use clap::Parser;
use cli::arg::{Command, InstallArgs};
use installer::uninstall::delete_self_on_exit;
use std::time::Duration;
use tauri::{window::Color, Manager, WindowEvent};
use tauri_utils::{config::WindowEffectsConfig, WindowEffect};

lazy_static::lazy_static! {
    pub static ref REQUEST_CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent(format!("KachinaInstaller/{}", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .zstd(true)
        .read_timeout(Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
}

fn main() {
    let mut has_console = false;
    let is_help = std::env::args().any(|arg| arg == "-h" || arg == "--help");
    if is_help {
        get_console();
        has_console = true;
    }
    let cli = cli::Cli::parse();
    let mut command = cli.command();
    let wv2ver = tauri::webview_version();
    if wv2ver.is_err() {
        command = Command::InstallWebview2;
    }
    // command is not  Command::Install, can be anything
    match command {
        Command::Install(install) => {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(tauri_main(install));
        }
        Command::HeadlessUac(args) => {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(ipc::manager::uac_ipc_main(args));
        }
        cli => {
            if !has_console {
                get_console();
            }
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(cli_main(cli));
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
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            // things which can be run directly
            dfs::get_dfs,
            dfs::get_dfs_metadata,
            installer::launch_and_exit,
            installer::config::get_installer_config,
            installer::lnk::get_dirs,
            installer::registry::read_uninstall_metadata,
            installer::select_dir,
            installer::error_dialog,
            // things which may need uac
            fs::deep_readdir_with_metadata,
            fs::is_dir_empty,
            fs::ensure_dir,
            installer::lnk::create_lnk,
            installer::uninstall::create_uninstaller,
            installer::registry::write_registry,
            installer::uninstall::run_uninstall,
            // new mamaned operation
            ipc::manager::managed_operation,
        ])
        .manage(args)
        .manage(ipc::manager::ManagedElevate::new())
        .setup(move |app| {
            let main_window = app.get_webview_window("main").unwrap();
            #[cfg(debug_assertions)]
            {
                let window = app.get_webview_window("main");
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
                let _ = match dark_light::detect()? {
                    dark_light::Mode::Dark => {
                        main_window.set_background_color(Some(Color(0, 0, 0, 255)))
                    }
                    _ => main_window.set_background_color(Some(Color(255, 255, 255, 255))),
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

async fn cli_main(cli: Command) {
    match cli {
        Command::InstallWebview2 => cli::install_webview2().await,
        _ => {}
    }
}

pub fn get_console() {
    // try attach parent console
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    let attach_res = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    if attach_res.is_err() {
        // no parent console, alloc new console
        use windows::Win32::System::Console::AllocConsole;
        let _ = unsafe { AllocConsole() };
    }
}
