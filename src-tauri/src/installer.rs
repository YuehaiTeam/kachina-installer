use crate::local::{get_config_from_embedded, get_embedded, Embedded};
use serde_json::Value;
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Path};
use tauri::{AppHandle, WebviewWindow};
use windows::Win32::UI::Shell::{SHGetFolderPathW, CSIDL_COMMON_PROGRAMS, CSIDL_DESKTOPDIRECTORY};

#[tauri::command]
pub async fn launch_and_exit(path: String, app: AppHandle) {
    let _ = open::that(path);
    app.exit(0);
}

#[derive(serde::Serialize)]
pub struct InstallerConfig {
    pub install_path: String,
    pub install_path_exists: bool,
    pub is_uninstall: bool,
    pub embedded_files: Option<Vec<Embedded>>,
    pub embedded_config: Option<Value>,
    pub enbedded_metadata: Option<Value>,
    pub exe_path: String,
}

pub async fn get_config_pre(
    uninstall_name: String,
    exe_path: &Path,
    install_path: &Path,
    install_path_exists: bool,
) -> Result<InstallerConfig, String> {
    let is_uninstall = exe_path.file_name().unwrap().to_string_lossy() == uninstall_name;
    let exe_path = exe_path.to_string_lossy().to_string();
    let mut embedded_files = None;
    let mut embedded_config = None;
    let mut enbedded_metadata = None;
    if let Ok(embedded_files_res) = get_embedded().await {
        if let Ok(res) = get_config_from_embedded(&embedded_files_res).await {
            embedded_config = res.0;
            enbedded_metadata = res.1;
        }

        embedded_files = Some(embedded_files_res);
    }
    Ok(InstallerConfig {
        install_path: install_path.to_string_lossy().to_string(),
        install_path_exists,
        is_uninstall,
        embedded_files,
        embedded_config,
        enbedded_metadata,
        exe_path,
    })
}

#[tauri::command]
pub async fn get_installer_config(
    exe_name: String,
    reg_name: String,
    program_files_path: String,
    uninstall_name: String,
) -> Result<InstallerConfig, String> {
    // check if current dir has exeName
    let exe_path = std::env::current_exe();
    if exe_path.is_err() {
        return Err(format!(
            "Failed to get current exe path: {:?}",
            exe_path.err()
        ));
    }
    let exe_path = exe_path.unwrap();
    let exe_dir = exe_path.parent();
    if exe_dir.is_none() {
        return Err("Failed to get exe dir".to_string());
    }
    let exe_dir = exe_dir.unwrap();
    let exe_path = exe_dir.join(exe_name.clone());
    if exe_path.exists() {
        return get_config_pre(uninstall_name, &exe_path, exe_dir, true).await;
    }
    let exe_parent_dir = exe_dir.parent();
    if exe_parent_dir.is_none() {
        return Err("Failed to get exe parent dir".to_string());
    }
    let exe_parent_dir = exe_parent_dir.unwrap();
    let exe_path = exe_parent_dir.join(exe_name.clone());
    if exe_path.exists() {
        return get_config_pre(uninstall_name, &exe_path, exe_parent_dir, true).await;
    }
    let key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    ));
    if key.is_ok() {
        let key = key.unwrap();
        let path: String = key
            .get_value("InstallLocation")
            .map_err(|e| e.to_string())?;
        let path = Path::new(&path);
        let exe_path = Path::new(&path).join(exe_name.clone());
        if exe_path.exists() {
            return get_config_pre(uninstall_name, &exe_path, path, true).await;
        }
        let sub_exe_path = Path::new(&path)
            .join(reg_name.clone())
            .join(exe_name.clone());
        if sub_exe_path.exists() {
            let sub_exe_dir = Path::new(&path).join(reg_name.clone());
            return get_config_pre(uninstall_name, &exe_path, &sub_exe_dir, true).await;
        }
    }
    let program_files = std::env::var("ProgramFiles");
    if program_files.is_err() {
        return Err(format!(
            "Failed to get ProgramFiles: {:?}",
            program_files.err()
        ));
    }
    let program_files = program_files.unwrap();
    let program_files_real_path = Path::new(&program_files).join(program_files_path.clone());
    let program_files_exe_path = program_files_real_path.join(exe_name.clone());
    get_config_pre(
        uninstall_name,
        &exe_path,
        &program_files_real_path,
        program_files_exe_path.exists(),
    )
    .await
}

#[tauri::command]
pub async fn create_lnk(target: String, lnk: String) -> Result<(), String> {
    let target = Path::new(&target);
    let lnk = Path::new(&lnk);
    let sl = mslnk::ShellLink::new(target)
        .map_err(|e| format!("Failed to create shell link: {:?}", e))?;
    sl.create_lnk(lnk)
        .map_err(|e| format!("Failed to create lnk: {:?}", e))?;
    Ok(())
}

fn get_start_menu_directory() -> String {
    let mut path: [u16; 260] = [0; 260];
    unsafe {
        let _ = SHGetFolderPathW(None, CSIDL_COMMON_PROGRAMS as i32, None, 0, &mut path);
    }
    OsString::from_wide(&path)
        .to_string_lossy()
        .as_ref()
        .trim_end_matches('\0')
        .to_string()
}

fn get_desktop_directory() -> String {
    use windows::Win32::UI::Shell::SHGetFolderPathW;
    let mut path: [u16; 260] = [0; 260];
    unsafe {
        let _ = SHGetFolderPathW(None, CSIDL_DESKTOPDIRECTORY as i32, None, 0, &mut path);
    }
    OsString::from_wide(&path)
        .to_string_lossy()
        .as_ref()
        .trim_end_matches('\0')
        .to_string()
}

#[tauri::command]
pub async fn get_dirs() -> Option<(String, String)> {
    Some((get_start_menu_directory(), get_desktop_directory()))
}

#[tauri::command]
pub async fn create_uninstaller(
    source: String,
    uninstaller_name: String,
    updater_name: String,
) -> Result<(), String> {
    let source = Path::new(&source);
    let uninstaller_path = source.join(uninstaller_name);
    let updater_path = source.join(updater_name);
    let current_exe_path = std::env::current_exe();
    if current_exe_path.is_err() {
        return Err(format!(
            "Failed to get current exe path: {:?}",
            current_exe_path.err()
        ));
    }
    let current_exe_path = current_exe_path.unwrap();
    let updater_is_self = true; // tbd, so always trust the updater is newer
    if updater_path.exists() && updater_is_self {
        let res = tokio::fs::copy(&current_exe_path, &updater_path).await;
        if res.is_err() {
            return Err(format!("Failed to create updater: {:?}", res.err()));
        }
    } else {
        // else, overwrite uninstaller and updater
        let res = tokio::fs::copy(&current_exe_path, &uninstaller_path).await;
        if res.is_err() {
            return Err(format!("Failed to create uninstaller: {:?}", res.err()));
        }
        let res = tokio::fs::copy(&current_exe_path, &updater_path).await;
        if res.is_err() {
            return Err(format!("Failed to create updater: {:?}", res.err()));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn write_registry(
    reg_name: String,
    name: String,
    version: String,
    exe: String,
    source: String,
    uninstaller: String,
    metadata: String,
    size: u64,
    publisher: String,
) -> Result<(), String> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        format!(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
            reg_name
        ),
        winreg::enums::KEY_READ | winreg::enums::KEY_WRITE | winreg::enums::KEY_QUERY_VALUE,
    );
    let key = if let Ok(key) = key {
        key
    } else {
        let create = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .create_subkey(format!(
                "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
                reg_name
            ))
            .map_err(|e| format!("Failed to create subkey: {:?}", e))?;
        create.0
    };

    key.set_value("DisplayName", &name)
        .map_err(|e| format!("Failed to set DisplayName: {:?}", e))?;
    key.set_value("DisplayVersion", &version)
        .map_err(|e| format!("Failed to set DisplayVersion: {:?}", e))?;
    key.set_value("UninstallString", &uninstaller)
        .map_err(|e| format!("Failed to set UninstallString: {:?}", e))?;
    key.set_value("InstallLocation", &source)
        .map_err(|e| format!("Failed to set InstallLocation: {:?}", e))?;
    key.set_value("DisplayIcon", &exe)
        .map_err(|e| format!("Failed to set DisplayIcon: {:?}", e))?;
    key.set_value("Publisher", &publisher)
        .map_err(|e| format!("Failed to set Publisher: {:?}", e))?;
    key.set_value("EstimatedSize", &size)
        .map_err(|e| format!("Failed to set EstimatedSize: {:?}", e))?;
    key.set_value("NoModify", &1u32)
        .map_err(|e| format!("Failed to set NoModify: {:?}", e))?;
    key.set_value("NoRepair", &1u32)
        .map_err(|e| format!("Failed to set NoRepair: {:?}", e))?;
    key.set_value("InstallerMeta", &metadata)
        .map_err(|e| format!("Failed to set UninstallData: {:?}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn read_uninstall_metadata(reg_name: String) -> Result<Value, String> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    ));
    if key.is_err() {
        return Err("Failed to open subkey".to_string());
    }
    let key = key.unwrap();
    let metadata: String = key.get_value("InstallerMeta").map_err(|e| e.to_string())?;
    let metadata: Value = serde_json::from_str(&metadata).map_err(|e| e.to_string())?;
    Ok(metadata)
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
