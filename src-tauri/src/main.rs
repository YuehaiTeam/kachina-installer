// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod dfs;
pub mod fs;
pub mod installer;
pub mod static_obj;

use tauri::Manager;
use tracing_subscriber::EnvFilter;
fn main() {
    let wv2ver = tauri::webview_version();
    if wv2ver.is_err() || std::env::args_os().any(|a| &a == "--install-webview2") {
        // try attach parent console
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let attach_res = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        if attach_res.is_err() {
            // no parent console, alloc new console
            use windows::Win32::System::Console::AllocConsole;
            let _ = unsafe { AllocConsole() };
        }
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(cli_main());
        return;
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            fs::decompress,
            fs::md5_file,
            fs::download_and_decompress,
            fs::deep_readdir_with_metadata,
            dfs::get_dfs,
            dfs::get_dfs_metadata,
            installer::launch_and_exit,
            installer::get_install_source,
        ])
        .setup(move |app| {
            let main_window = app.get_window("main").unwrap();
            #[cfg(debug_assertions)]
            {
                let _window = app.get_webview_window("main");
                if _window.is_some() {
                    _window.unwrap().open_devtools();
                }
            }

            if !is_win11 {
                let _ = main_window.set_effects(Some(tauri::utils::config::WindowEffectsConfig {
                    effects: vec![tauri::window::Effect::Acrylic],
                    ..Default::default()
                }));
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

async fn cli_main() {
    println!("安装程序缺少必要的运行环境");
    println!("当前系统未安装 WebView2 运行时，正在安装...");
    let wv2_installer_blob = include_bytes!("../../utils/MicrosoftEdgeWebview2Setup.exe");
    // extract the installer to a temp file
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
