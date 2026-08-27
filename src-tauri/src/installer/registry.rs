use crate::utils::{
    error::{IntoTAResult, TAResult},
    uac::check_elevated,
};
use anyhow::{Context, Result};
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

pub async fn write_registry_with_params(params: WriteRegistryParams) -> TAResult<()> {
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
) -> TAResult<()> {
    write_registry_raw(
        reg_name,
        name,
        version,
        exe,
        source,
        uninstaller,
        metadata,
        size,
        publisher,
    )
    .await
    .into_ta_result()
}
pub async fn write_registry_raw(
    reg_name: String,
    name: String,
    version: String,
    exe: String,
    source: String,
    uninstaller: String,
    metadata: String,
    size: u64,
    publisher: String,
) -> Result<()> {
    let elevated = check_elevated().unwrap_or(false);

    let key_path = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{reg_name}");

    // If the key already exists (e.g. installed by an earlier version), update it
    // in place so updates never leave a stale or duplicate entry in the other hive.
    let hive = if windows_registry::LOCAL_MACHINE
        .options()
        .read()
        .open(&key_path)
        .is_ok()
    {
        windows_registry::LOCAL_MACHINE
    } else if windows_registry::CURRENT_USER
        .options()
        .read()
        .open(&key_path)
        .is_ok()
    {
        windows_registry::CURRENT_USER
    } else if elevated {
        windows_registry::LOCAL_MACHINE
    } else {
        windows_registry::CURRENT_USER
    };

    let key = hive.create(&key_path).context("OPEN_REG_ERR")?;
    {
        key.set_string("DisplayName", &name)?;
        key.set_string("DisplayVersion", &version)?;
        key.set_string("UninstallString", &uninstaller)?;
        key.set_string("InstallLocation", &source)?;
        key.set_string("DisplayIcon", &exe)?;
        key.set_string("Publisher", &publisher)?;
        key.set_u32("EstimatedSize", (size as u32) / 1024)?;
        key.set_u32("NoModify", 1u32)?;
        key.set_u32("NoRepair", 1u32)?;
        key.set_string("InstallerMeta", &metadata)?;
        Ok::<(), anyhow::Error>(())
    }
    .context("WRITE_REG_ERR")
}

pub async fn read_uninstall_metadata(reg_name: String) -> TAResult<Value> {
    read_uninstall_metadata_raw(&reg_name, None).into_ta_result()
}

pub fn read_uninstall_metadata_raw(reg_name: &str, install_path: Option<&str>) -> Result<Value> {
    let key_path = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{reg_name}");
    let hives = [
        windows_registry::LOCAL_MACHINE,
        windows_registry::CURRENT_USER,
    ];
    let mut fallback = None;
    for hive in hives {
        let Ok(key) = hive.options().read().open(&key_path) else {
            continue;
        };
        let Ok(metadata) = key.get_string("InstallerMeta") else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&metadata) else {
            continue;
        };
        if let Some(want) = install_path {
            let location = key.get_string("InstallLocation").unwrap_or_default();
            if !location.is_empty()
                && location
                    .trim_end_matches(['\\', '/'])
                    .eq_ignore_ascii_case(want.trim_end_matches(['\\', '/']))
            {
                return Ok(parsed);
            }
        }
        if fallback.is_none() {
            fallback = Some(parsed);
        }
    }
    fallback.context("GET_INSTALLMETA_ERR")
}
