use tauri::{AppHandle, WebviewWindow};

use crate::utils::dir::in_private_folder;

pub mod config;
pub mod lnk;
pub mod registry;
pub mod uninstall;

#[tauri::command]
pub async fn launch_and_exit(path: String, app: AppHandle) {
    let _ = open::that(path);
    app.exit(0);
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DirState {
    Unwritable,
    Writable,
    Private,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SelectDirRes {
    pub path: String,
    pub state: DirState,
    pub empty: bool,
    pub upgrade: bool,
}

#[tauri::command]
pub async fn select_dir(
    path: String,
    exe_name: String,
    silent: bool,
    window: WebviewWindow,
) -> Option<SelectDirRes> {
    let pathstr = if silent {
        path.clone()
    } else {
        let res = rfd::AsyncFileDialog::new()
            .set_directory(path)
            .set_can_create_directories(true)
            .set_parent(&window)
            .pick_folder()
            .await;
        res.as_ref()?;
        let res = res.unwrap();
        res.path().to_str().map(|s| s.to_string())?
    };
    let mut empty = true;
    let mut upgrade = false;
    let path = std::path::Path::new(&pathstr);
    let mut state = DirState::Writable;
    if path.is_file() {
        return None;
    }
    if path.exists() {
        // check writeable by direct open the directory
        let handle = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .create_new(true)
            .open(path)
            .await;
        if handle.is_err() {
            state = DirState::Unwritable;
        }
        drop(handle);
        let exe_path = path.join(exe_name);
        if exe_path.exists() {
            upgrade = true;
            empty = false;
        } else {
            let entries = tokio::fs::read_dir(path).await;
            if entries.is_ok() {
                let mut entries = entries.unwrap();
                if let Ok(Some(_entry)) = entries.next_entry().await {
                    empty = false;
                }
            }
        }
    } else {
        // get parent dir
        let parent = path.parent();
        parent?;
        let parent = parent.unwrap();
        let handle = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .create_new(true)
            .open(parent)
            .await;
        if handle.is_err() {
            state = DirState::Unwritable;
        }
    }
    if in_private_folder(path) {
        state = DirState::Private;
    }
    Some(SelectDirRes {
        path: pathstr,
        state,
        empty,
        upgrade,
    })
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

#[tauri::command]
pub async fn confirm_dialog(title: String, message: String, window: WebviewWindow) -> bool {
    let ret = rfd::MessageDialog::new()
        .set_title(&title)
        .set_description(&message)
        .set_level(rfd::MessageLevel::Info)
        .set_parent(&window)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    
        matches!(ret, rfd::MessageDialogResult::Yes)
}
