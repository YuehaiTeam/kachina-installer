use crate::utils::uac::check_elevated;
use serde_json::Value;

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
    let hive = if elevated {
        windows_registry::LOCAL_MACHINE
    } else {
        windows_registry::CURRENT_USER
    };

    let key_path = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    );

    let key = hive
        .create(&key_path)
        .map_err(|e| format!("Failed to create/open registry key: {:?}", e))?;

    key.set_string("DisplayName", &name)
        .map_err(|e| format!("Failed to set DisplayName: {:?}", e))?;
    key.set_string("DisplayVersion", &version)
        .map_err(|e| format!("Failed to set DisplayVersion: {:?}", e))?;
    key.set_string("UninstallString", &uninstaller)
        .map_err(|e| format!("Failed to set UninstallString: {:?}", e))?;
    key.set_string("InstallLocation", &source)
        .map_err(|e| format!("Failed to set InstallLocation: {:?}", e))?;
    key.set_string("DisplayIcon", &exe)
        .map_err(|e| format!("Failed to set DisplayIcon: {:?}", e))?;
    key.set_string("Publisher", &publisher)
        .map_err(|e| format!("Failed to set Publisher: {:?}", e))?;
    key.set_u32("EstimatedSize", (size as u32) / 1024)
        .map_err(|e| format!("Failed to set EstimatedSize: {:?}", e))?;
    key.set_u32("NoModify", 1u32)
        .map_err(|e| format!("Failed to set NoModify: {:?}", e))?;
    key.set_u32("NoRepair", 1u32)
        .map_err(|e| format!("Failed to set NoRepair: {:?}", e))?;
    key.set_string("InstallerMeta", &metadata)
        .map_err(|e| format!("Failed to set UninstallData: {:?}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn read_uninstall_metadata(reg_name: String) -> Result<Value, String> {
    let key_path = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    );

    // First try HKLM, if not exist, try HKCU
    let key = windows_registry::LOCAL_MACHINE
        .options()
        .read()
        .open(&key_path)
        .or_else(|_| {
            windows_registry::CURRENT_USER
                .options()
                .read()
                .open(&key_path)
        })
        .map_err(|e| format!("Failed to open registry key: {:?}", e))?;

    let metadata: String = key
        .get_string("InstallerMeta")
        .map_err(|e| format!("Failed to read InstallerMeta: {:?}", e))?;

    let metadata: Value = serde_json::from_str(&metadata).map_err(|e| e.to_string())?;
    Ok(metadata)
}
