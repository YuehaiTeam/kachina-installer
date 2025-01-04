use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Path};
use tauri::AppHandle;
use windows::Win32::UI::Shell::{SHGetFolderPathW, CSIDL_COMMON_PROGRAMS, CSIDL_DESKTOPDIRECTORY};

#[tauri::command]
pub async fn launch_and_exit(path: String, app: AppHandle) {
    let _ = open::that(path);
    app.exit(0);
}

#[tauri::command]
pub async fn get_install_source(
    exe_name: String,
    reg_name: String,
    program_files_path: String,
) -> Result<(String, bool), String> {
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
        return Ok((exe_dir.to_string_lossy().to_string(), true));
    }
    let key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    ));
    if key.is_ok() {
        let key = key.unwrap();
        let path = key.get_value("InstallLocation");
        if path.is_ok() {
            let path = path.unwrap();
            let exe_path = Path::new(&path).join(exe_name.clone());
            if exe_path.exists() {
                return Ok((path, true));
            }
            let sub_exe_path = Path::new(&path)
                .join(reg_name.clone())
                .join(exe_name.clone());
            if sub_exe_path.exists() {
                return Ok((format!("{}\\{}", path, reg_name), true));
            }
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
    Ok((
        program_files_real_path.to_string_lossy().to_string(),
        program_files_exe_path.exists(),
    ))
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
pub async fn create_uninstaller(path: String) -> Result<(), String> {
    // if 'updater.exe' is in path, and updater is self, copy updater to path
    let uninstaller_path = Path::new(&path);
    let parent_path = uninstaller_path.parent();
    if parent_path.is_none() {
        return Err("Failed to get parent path".to_string());
    }
    let parent_path = parent_path.unwrap();
    let updater_path = parent_path.join("update.exe");
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
    let key = if key.is_err() {
        let create = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .create_subkey(format!(
                "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
                reg_name
            ))
            .map_err(|e| format!("Failed to create subkey: {:?}", e))?;
        create.0
    } else {
        key.unwrap()
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
