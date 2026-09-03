use windows::Win32::{
    Foundation::{CloseHandle, WAIT_FAILED, WAIT_TIMEOUT},
    System::Diagnostics::ToolHelp::PROCESSENTRY32W,
};

use crate::utils::dir::in_private_folder;
use anyhow::{Context, Result};

pub mod config;
pub mod lnk;
pub mod registry;
pub mod runtimes;
pub mod uninstall;

pub async fn launch(path: String) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = crate::utils::process::shell_open(&path) {
            tracing::warn!("launch {path} failed: {e}");
        }
    })
    .await;
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

/// What the installer knows about a target directory. Single source for the
/// GUI/native `UiState.path`, `Settings.elevate` / `is_update`, and the
/// directory picker; `None` when the path is an existing file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirProbe {
    pub exists: bool,
    pub empty: bool,
    /// The project's exe is already there.
    pub upgrade: bool,
    pub writable: bool,
    pub private: bool,
}

impl DirProbe {
    pub fn state(&self) -> DirState {
        if !self.writable {
            DirState::Unwritable
        } else if self.private {
            DirState::Private
        } else {
            DirState::Writable
        }
    }
}

/// Writability is tested by creating and removing a probe file in the directory,
/// or in the nearest existing ancestor when the directory does not exist yet.
pub fn probe_dir(path: &std::path::Path, exe_name: &str) -> Option<DirProbe> {
    if path.is_file() {
        return None;
    }
    let exists = path.is_dir();
    let (empty, upgrade) = if exists {
        let upgrade = !exe_name.is_empty() && path.join(exe_name).is_file();
        let empty = !upgrade
            && std::fs::read_dir(path)
                .map(|mut it| it.next().is_none())
                .unwrap_or(true);
        (empty, upgrade)
    } else {
        (true, false)
    };
    let writable = path
        .ancestors()
        .find(|p| p.is_dir())
        .is_some_and(can_create_probe_file);
    Some(DirProbe {
        exists,
        empty,
        upgrade,
        writable,
        private: in_private_folder(path),
    })
}

fn can_create_probe_file(dir: &std::path::Path) -> bool {
    let p = dir.join(format!(".kachina-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
    {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&p);
            true
        }
        Err(_) => false,
    }
}

pub async fn inspect_dir(pathstr: String, exe_name: String) -> Option<SelectDirRes> {
    let probe = probe_dir(std::path::Path::new(&pathstr), &exe_name)?;
    Some(SelectDirRes {
        path: pathstr,
        state: probe.state(),
        empty: probe.empty,
        upgrade: probe.upgrade,
    })
}

pub async fn kill_process(pid: u32) -> Result<()> {
    let ret = tokio::task::spawn_blocking(move || {
        // use the windows crate
        let handle = unsafe {
            windows::Win32::System::Threading::OpenProcess(
                windows::Win32::System::Threading::PROCESS_TERMINATE
                    | windows::Win32::System::Threading::PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }
        .context("OPEN_PROCESS_ERR")?;
        let ret = unsafe { windows::Win32::System::Threading::TerminateProcess(handle, 1) }
            .context("KILL_PROCESS_ERR");
        if ret.is_err() {
            let _ = unsafe { CloseHandle(handle) };
            return ret;
        }
        // wait for the process to exit, timeout 10s
        let ret = unsafe { windows::Win32::System::Threading::WaitForSingleObject(handle, 10000) };
        match ret {
            WAIT_FAILED => {
                let oserr = windows::core::Error::from_thread();
                return Err(anyhow::anyhow!(oserr).context("WAIT_PROCESS_ERR"));
            }
            WAIT_TIMEOUT => {
                return Err(
                    anyhow::anyhow!("Process did not exit in time").context("KILL_PROCESS_TIMEOUT")
                );
            }
            _ => {}
        };
        let _ = unsafe { CloseHandle(handle) };
        Ok(())
    })
    .await;
    if let Err(e) = ret {
        return Err(anyhow::Error::new(e).context("KILL_PROCESS_ERR"));
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

pub async fn find_process_by_name(name: String) -> Result<Vec<(u32, String)>> {
    let mut processes = Vec::new();
    unsafe {
        let snapshot = windows::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot(
            windows::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS,
            0,
        )
        .context("FIND_PROCESS_ERR")?;
        if snapshot.is_invalid() {
            return Err(anyhow::anyhow!("Failed to create snapshot: invalid handle")
                .context("FIND_PROCESS_ERR"));
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

pub async fn error_dialog(title: String, message: String, parent: crate::host::HwndParent) {
    let ok = crate::utils::i18n::t("dialog.ok", &[]);
    tokio::task::spawn_blocking(move || {
        crate::utils::taskdialog::task_dialog(
            crate::utils::taskdialog::TaskDialogRequest {
                title,
                content: message,
                expanded: None,
                footer: None,
                buttons: vec![crate::utils::taskdialog::CommandLink {
                    id: windows::Win32::UI::WindowsAndMessaging::IDOK.0,
                    text: ok,
                }],
            },
            parent.hwnd(),
        );
    })
    .await
    .ok();
}

pub async fn confirm_dialog(
    title: String,
    message: String,
    parent: crate::host::HwndParent,
) -> bool {
    let yes = crate::utils::i18n::t("dialog.yes", &[]);
    let no = crate::utils::i18n::t("dialog.no", &[]);
    let clicked = tokio::task::spawn_blocking(move || {
        crate::utils::taskdialog::task_dialog(
            crate::utils::taskdialog::TaskDialogRequest {
                title,
                content: message,
                expanded: None,
                footer: None,
                buttons: vec![
                    crate::utils::taskdialog::CommandLink {
                        id: windows::Win32::UI::WindowsAndMessaging::IDYES.0,
                        text: yes,
                    },
                    crate::utils::taskdialog::CommandLink {
                        id: windows::Win32::UI::WindowsAndMessaging::IDNO.0,
                        text: no,
                    },
                ],
            },
            parent.hwnd(),
        )
    })
    .await
    .unwrap_or(0);
    clicked == windows::Win32::UI::WindowsAndMessaging::IDYES.0
}

pub async fn pick_install_path(
    current: &str,
    exe_name: &str,
    app_name: &str,
    parent: crate::host::HwndParent,
) -> Option<String> {
    let path = crate::utils::folderdialog::pick_folder(current.to_string(), parent).await?;
    let seldir = inspect_dir(path, exe_name.to_string()).await?;
    apply_path_choice(seldir, app_name, parent).await
}

pub async fn apply_path_choice(
    seldir: SelectDirRes,
    app_name: &str,
    parent: crate::host::HwndParent,
) -> Option<String> {
    if !seldir.empty && !seldir.upgrade {
        let is_drive_root = {
            let n = seldir.path.replace('\\', "/");
            n.len() == 3 && n.as_bytes().get(1) == Some(&b':') && n.ends_with('/')
        };
        let nest = is_drive_root
            || confirm_dialog(
                crate::utils::i18n::t("dialog.prompt", &[]),
                crate::utils::i18n::t("ready.dir_not_empty", &[]),
                parent,
            )
            .await;
        if nest {
            return Some(format!(
                "{}\\{app_name}",
                seldir.path.trim_end_matches(['\\', '/'])
            ));
        }
    }
    Some(seldir.path)
}


pub fn log(data: String) {
    tracing::info!("{}", data);
}

pub fn warn(data: String) {
    tracing::warn!("{}", data);
}

pub fn error(data: String) {
    tracing::error!("{}", data);
}
