use std::path::Path;
use tauri::AppHandle;

#[tauri::command]
pub async fn launch_and_exit(path: String, app: AppHandle) {
    let _ = tauri_plugin_opener::open_path(path, None::<&str>);
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
    // check if registry installion path exists
    // HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\BetterGI
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
