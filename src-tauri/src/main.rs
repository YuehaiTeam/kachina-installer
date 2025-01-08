// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod cli;
pub mod dfs;
pub mod fs;
pub mod installer;
pub mod local;
pub mod pack;
pub mod progressed_read;
pub mod static_obj;

use clap::Parser;
use cli::{Command, InstallArgs};
use installer::delete_self_on_exit;
use static_obj::REQUEST_CLIENT;
use tauri::{Manager, WindowEvent};
use tauri_utils::config::WindowEffectsConfig;
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
            tauri_main(install);
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

fn tauri_main(args: InstallArgs) {
    let (major, minor, build) = nt_version::get();
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
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            fs::run_hpatch,
            fs::local_decompress,
            fs::download_and_decompress,
            fs::download_and_decompress_and_hpatch,
            fs::deep_readdir_with_metadata,
            fs::is_dir_empty,
            fs::ensure_dir,
            fs::clear_empty_dirs,
            dfs::get_dfs,
            dfs::get_dfs_metadata,
            installer::launch_and_exit,
            installer::get_installer_config,
            installer::create_lnk,
            installer::get_dirs,
            installer::create_uninstaller,
            installer::write_registry,
            installer::read_uninstall_metadata,
            installer::select_dir,
            installer::error_dialog,
            installer::run_uninstall
        ])
        .manage(args)
        .setup(move |app| {
            let main_window = app.get_webview_window("main").unwrap();
            #[cfg(debug_assertions)]
            {
                let window = app.get_webview_window("main");
                if let Some(window) = window {
                    window.open_devtools();
                }
            }

            if !is_win11 {
                let _ = main_window.set_effects(Some(WindowEffectsConfig {
                    effects: vec![tauri::window::Effect::Acrylic],
                    ..Default::default()
                }));
            }
            Ok(())
        })
        .on_window_event(|_app, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                delete_self_on_exit();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

async fn cli_main(cli: Command) {
    match cli {
        Command::InstallWebview2 => install_webview2().await,
        Command::Pack(args) => pack::pack_cli(args).await,
        _ => {}
    }
}

async fn install_webview2() {
    println!("安装程序缺少必要的运行环境");
    println!("当前系统未安装 WebView2 运行时，正在下载并安装...");
    // use reqwest to download the installer
    let res = REQUEST_CLIENT
        .get("https://go.microsoft.com/fwlink/p/?LinkId=2124703")
        .send()
        .await
        .expect("failed to download WebView2 installer");
    let wv2_installer_blob = res
        .bytes()
        .await
        .expect("failed to download WebView2 installer");
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir
        .as_path()
        .join("kachina.MicrosoftEdgeWebview2Setup.exe");
    tokio::fs::write(&installer_path, wv2_installer_blob)
        .await
        .expect("failed to write installer to temp dir");
    // run the installer
    let status = tokio::process::Command::new(installer_path.clone())
        .arg("/install")
        .status()
        .await
        .expect("failed to run installer");
    let _ = tokio::fs::remove_file(installer_path).await;
    if status.success() {
        println!("WebView2 运行时安装成功");
        println!("正在重新启动安装程序...");
        // exec self and detatch
        let _ = tokio::process::Command::new(std::env::current_exe().unwrap()).spawn();
        // delete the installer
    } else {
        println!("WebView2 运行时安装失败");
        println!("按任意键退出...");
        let _ = tokio::io::AsyncReadExt::read(&mut tokio::io::stdin(), &mut [0u8]).await;
        std::process::exit(0);
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
