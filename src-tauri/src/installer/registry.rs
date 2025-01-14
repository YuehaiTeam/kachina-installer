use serde_json::Value;

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
