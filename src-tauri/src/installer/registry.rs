use crate::utils::uac::check_elevated;
use serde_json::Value;

// 如果有权限，写HKLM，否则写HKCU

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
pub struct WriteRegistryParams {
    pub reg_name: String,
    pub name: String,
    pub version: String,
    pub exe: String,
    pub source: String,
    pub uninstaller: String,
    pub metadata: String,
    pub size: u64,
    pub publisher: String,
}

pub async fn write_registry_with_params(params: WriteRegistryParams) -> Result<(), String> {
    write_registry(
        params.reg_name,
        params.name,
        params.version,
        params.exe,
        params.source,
        params.uninstaller,
        params.metadata,
        params.size,
        params.publisher,
    )
    .await
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
    let elevated = check_elevated().unwrap_or(false);
    let regbase = if elevated {
        winreg::enums::HKEY_LOCAL_MACHINE
    } else {
        winreg::enums::HKEY_CURRENT_USER
    };
    let key = winreg::RegKey::predef(regbase).open_subkey_with_flags(
        format!(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
            reg_name
        ),
        winreg::enums::KEY_READ | winreg::enums::KEY_WRITE | winreg::enums::KEY_QUERY_VALUE,
    );
    let key = if let Ok(key) = key {
        key
    } else {
        let create = winreg::RegKey::predef(regbase)
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
    // First try HKLM, if not exist, try HKCU
    let mut key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    ));
    if key.is_err() {
        key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(format!(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
            reg_name
        ));
    }
    if key.is_err() {
        return Err("Failed to open registry key".to_string());
    }
    let key = key.unwrap();
    let metadata: String = key.get_value("InstallerMeta").map_err(|e| e.to_string())?;
    let metadata: Value = serde_json::from_str(&metadata).map_err(|e| e.to_string())?;
    Ok(metadata)
}
