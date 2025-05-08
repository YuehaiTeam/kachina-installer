use crate::{
    cli::arg::InstallArgs,
    local::{get_config_from_embedded, get_embedded, Embedded},
    utils::{
        error::{return_ta_result, TAResult},
        uac::check_elevated,
    },
};
use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use tauri::State;

#[derive(Serialize, Debug, Clone)]
pub struct InstallerConfig {
    pub install_path: String,
    pub install_path_exists: bool,
    pub install_path_source: &'static str,
    pub is_uninstall: bool,
    pub embedded_files: Option<Vec<Embedded>>,
    pub embedded_index: Option<Vec<Embedded>>,
    pub embedded_config: Option<Value>,
    pub enbedded_metadata: Option<Value>,
    pub exe_path: String,
    pub args: crate::cli::arg::InstallArgs,
    pub elevated: bool,
}

pub async fn get_config_pre(
    exe_path_path: &Path,
    args: InstallArgs,
    scan_exe: bool,
) -> anyhow::Result<InstallerConfig> {
    let exe_path = exe_path_path.to_string_lossy().to_string();
    let mut embedded_files = None;
    let mut embedded_config = None;
    let mut enbedded_metadata = None;
    let mut embedded_index = None;
    if scan_exe {
        if let Ok(embedded_files_res) = get_embedded().await {
            if let Ok(res) = get_config_from_embedded(&embedded_files_res).await {
                embedded_config = res.0;
                enbedded_metadata = res.1;
                embedded_index = res.2;
            }
            embedded_files = Some(embedded_files_res);
        }
        #[cfg(debug_assertions)]
        {
            if embedded_config.is_none() {
                let exe_dir = exe_path_path.parent();
                if exe_dir.is_none() {
                    return Err(anyhow::anyhow!("Failed to get exe dir").context("GET_EXE_DIR_ERR"));
                }
                let exe_dir = exe_dir.unwrap();
                let config_json = exe_dir.join(".config.json");
                if config_json.exists() {
                    let config = tokio::fs::read(&config_json)
                        .await
                        .context("DEBUG_READ_CONFIG_ERR")?;
                    embedded_config =
                        Some(serde_json::from_slice(&config).context("DEBUG_READ_CONFIG_ERR")?);
                }
            }
        }
    }
    Ok(InstallerConfig {
        install_path: "".to_string(),
        install_path_exists: false,
        install_path_source: "",
        is_uninstall: false,
        embedded_files,
        embedded_index,
        embedded_config,
        enbedded_metadata,
        exe_path,
        args,
        elevated: check_elevated().unwrap_or(false),
    })
}

impl InstallerConfig {
    pub fn fill(
        mut self,
        install_path: &Path,
        install_path_exists: bool,
        install_path_source: &'static str,
    ) -> InstallerConfig {
        self.install_path = install_path.to_string_lossy().to_string();
        self.install_path_exists = install_path_exists;
        self.install_path_source = install_path_source;
        self
    }
}

#[tauri::command]
pub async fn get_installer_config(
    args: State<'_, InstallArgs>,
    scan_exe: bool,
) -> TAResult<InstallerConfig> {
    // check if current dir has exeName
    let exe_path = std::env::current_exe().context("GET_EXE_PATH_ERR")?;
    let mut config = get_config_pre(&exe_path, args.inner().clone(), scan_exe).await?;
    let mut uninstall_name = "uninst.exe";
    let mut exe_name = "main.exe";
    let mut program_files_path = "KachinaInstaller";
    let mut reg_name = "KachinaInstaller";
    if let Some(config) = config.embedded_config.as_ref() {
        uninstall_name = config["uninstallName"].as_str().unwrap_or("uninst.exe");
        exe_name = config["exeName"].as_str().unwrap_or("main.exe");
        program_files_path = config["programFilesPath"]
            .as_str()
            .unwrap_or("KachinaInstaller");
        reg_name = config["regName"].as_str().unwrap_or("KachinaInstaller");
    }
    let is_uninstall = exe_path.file_name().unwrap().to_string_lossy() == uninstall_name;
    config.is_uninstall = is_uninstall;
    let exe_dir = exe_path.parent();
    if exe_dir.is_none() {
        return return_ta_result("Failed to get exe dir".to_string(), "GET_EXE_PATH_ERR");
    }
    let exe_dir = exe_dir.unwrap();
    let exe_path = exe_dir.join(exe_name);
    if exe_path.exists() {
        return Ok(config.fill(exe_dir, true, "CURRENT_DIR"));
    }
    let exe_parent_dir = exe_dir.parent();
    if let Some(exe_parent_dir) = exe_parent_dir {
        let exe_path = exe_parent_dir.join(exe_name);
        if exe_path.exists() {
            return Ok(config.fill(exe_parent_dir, true, "PARENT_DIR"));
        }
    }
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
        });
    if key.is_ok() {
        let key = key.unwrap();
        let path: String = key
            .get_string("InstallLocation")
            .context("READ_REG_ERR")?;
        let path = Path::new(&path);
        let exe_path = Path::new(&path).join(exe_name);
        if exe_path.exists() {
            return Ok(config.fill(path, true, "REG"));
        }
        let sub_exe_path = Path::new(&path).join(reg_name).join(exe_name);
        if sub_exe_path.exists() {
            let sub_exe_dir = Path::new(&path).join(reg_name);
            return Ok(config.fill(&sub_exe_dir, true, "REG_FOLDED"));
        }
    }
    let program_files = std::env::var("ProgramFiles").context("GET_KNOWNFOLDER_ERR")?;
    let program_files_real_path = Path::new(&program_files).join(program_files_path);
    let program_files_exe_path = program_files_real_path.join(exe_name);
    Ok(config.fill(
        &program_files_real_path,
        program_files_exe_path.exists(),
        "DEFAULT",
    ))
}
