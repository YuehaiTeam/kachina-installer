use tauri::{AppHandle, WebviewWindow};
use windows::Win32::{
    Foundation::{CloseHandle, WAIT_FAILED, WAIT_TIMEOUT},
    System::Diagnostics::ToolHelp::PROCESSENTRY32W,
};

use crate::utils::dir::in_private_folder;

pub mod config;
pub mod lnk;
pub mod registry;
pub mod runtimes;
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
pub async fn kill_process(pid: u32) -> Result<(), String> {
    let ret = tokio::task::spawn_blocking(move || {
        // use the windows crate
        let handle = unsafe {
            windows::Win32::System::Threading::OpenProcess(
                windows::Win32::System::Threading::PROCESS_TERMINATE,
                false,
                pid,
            )
        };
        if let Err(e) = handle {
            return Err(format!("Failed to open process: {:?}", e));
        }
        let handle = handle.unwrap();
        let ret = unsafe { windows::Win32::System::Threading::TerminateProcess(handle, 1) };
        if let Err(e) = ret {
            return Err(format!("Failed to terminate process: {:?}", e));
        }
        // wait for the process to exit, timeout 10s
        let ret = unsafe { windows::Win32::System::Threading::WaitForSingleObject(handle, 10000) };
        match ret {
            WAIT_FAILED => {
                return Err(format!("Failed to wait for process: {:?}", ret));
            }
            WAIT_TIMEOUT => {
                return Err("Process did not exit in time".to_string());
            }
            _ => {}
        };
        let _ = unsafe { CloseHandle(handle) };
        return Ok(());
    })
    .await;
    if let Err(e) = ret {
        return Err(format!("Failed to kill process: {:?}", e));
    }
    ret.unwrap()
}

fn get_process_path(pid: u32) -> Option<String> {
    // QueryFullProcessImageName
    let handle = unsafe {
        windows::Win32::System::Threading::OpenProcess(
            windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
    };
    if handle.is_err() {
        return None;
    }
    let handle = handle.unwrap();
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let ret = unsafe {
        windows::Win32::System::Threading::QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    if ret.is_err() {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    Some(path)
}

#[tauri::command]
pub async fn find_process_by_name(name: String) -> Result<Vec<(u32, String)>, String> {
    let mut processes = Vec::new();
    unsafe {
        let snapshot = windows::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot(
            windows::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS,
            0,
        );
        if let Err(e) = snapshot {
            return Err(format!("Failed to create snapshot: {:?}", e));
        }
        let snapshot = snapshot.unwrap();
        if snapshot.is_invalid() {
            return Err("Failed to create snapshot: invalid handle".to_string());
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;

        if windows::Win32::System::Diagnostics::ToolHelp::Process32FirstW(snapshot, &mut entry)
            .is_ok()
        {
            loop {
                let current_name = String::from_utf16_lossy(&entry.szExeFile)
                    .trim_end_matches('\0')
                    .to_lowercase();
                if current_name == name.to_lowercase() {
                    if let Some(path) = get_process_path(entry.th32ProcessID) {
                        processes.push((entry.th32ProcessID, path));
                    } else {
                        processes.push((entry.th32ProcessID, "".to_string()));
                    }
                }

                if windows::Win32::System::Diagnostics::ToolHelp::Process32NextW(
                    snapshot, &mut entry,
                )
                .is_err()
                {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(processes)
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

#[tauri::command]
pub fn log(data: String) {
    println!("{}", data);
}
