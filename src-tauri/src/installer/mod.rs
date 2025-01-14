use tauri::{AppHandle, WebviewWindow};

pub mod config;
pub mod lnk;
pub mod registry;
pub mod uninstall;

#[tauri::command]
pub async fn launch_and_exit(path: String, app: AppHandle) {
    let _ = open::that(path);
    app.exit(0);
}

#[tauri::command]
pub async fn select_dir(path: String) -> Option<String> {
    let res = rfd::AsyncFileDialog::new()
        .set_directory(path)
        .set_can_create_directories(true)
        .pick_folder()
        .await;
    res.as_ref()?;
    let res = res.unwrap();
    res.path().to_str().map(|s| s.to_string())
}

#[tauri::command]
pub async fn error_dialog(title: String, message: String, window: WebviewWindow) {
    rfd::MessageDialog::new()
        .set_title(&title)
        .set_description(&message)
        .set_level(rfd::MessageLevel::Error)
        .set_parent(&window)
        .show();
}
