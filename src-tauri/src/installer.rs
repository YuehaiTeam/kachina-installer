use crate::{
    cli::InstallArgs,
    fs::{delete_dir_if_empty, rm_list, run_clear_empty_dirs},
    local::{get_config_from_embedded, get_embedded, Embedded},
};
use serde_json::Value;
use std::{
    ffi::OsString,
    os::windows::{ffi::OsStringExt, process::CommandExt},
    path::Path,
};
use tauri::{AppHandle, State, WebviewWindow};
use tokio::io::AsyncWriteExt;
use windows::Win32::{
    System::Threading::CREATE_NO_WINDOW,
    UI::Shell::{SHGetFolderPathW, CSIDL_COMMON_PROGRAMS, CSIDL_DESKTOPDIRECTORY},
};

lazy_static::lazy_static!(
    static ref DELETE_SELF_ON_EXIT_PATH: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
);

#[tauri::command]
pub async fn launch_and_exit(path: String, app: AppHandle) {
    let _ = open::that(path);
    app.exit(0);
}

#[derive(serde::Serialize)]
pub struct InstallerConfig {
    pub install_path: String,
    pub install_path_exists: bool,
    pub install_path_source: &'static str,
    pub is_uninstall: bool,
    pub embedded_files: Option<Vec<Embedded>>,
    pub embedded_config: Option<Value>,
    pub enbedded_metadata: Option<Value>,
    pub exe_path: String,
    pub args: crate::cli::InstallArgs,
}

pub async fn get_config_pre(
    exe_path_path: &Path,
    args: InstallArgs,
) -> Result<InstallerConfig, String> {
    let exe_path = exe_path_path.to_string_lossy().to_string();
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
    #[cfg(debug_assertions)]
    {
        if embedded_config.is_none() {
            let exe_dir = exe_path_path.parent();
            if exe_dir.is_none() {
                return Err("Failed to get exe dir".to_string());
            }
            let exe_dir = exe_dir.unwrap();
            let config_json = exe_dir.join(".config.json");
            if config_json.exists() {
                let config = tokio::fs::read(&config_json)
                    .await
                    .map_err(|e| e.to_string())?;
                embedded_config = Some(serde_json::from_slice(&config).map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(InstallerConfig {
        install_path: "".to_string(),
        install_path_exists: false,
        install_path_source: "",
        is_uninstall: false,
        embedded_files,
        embedded_config,
        enbedded_metadata,
        exe_path,
        args,
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
pub async fn get_installer_config(args: State<'_, InstallArgs>) -> Result<InstallerConfig, String> {
    // check if current dir has exeName
    let exe_path = std::env::current_exe();
    if exe_path.is_err() {
        return Err(format!(
            "Failed to get current exe path: {:?}",
            exe_path.err()
        ));
    }
    let exe_path = exe_path.unwrap();
    let mut config = get_config_pre(&exe_path, args.inner().clone()).await?;
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
        return Err("Failed to get exe dir".to_string());
    }
    let exe_dir = exe_dir.unwrap();
    let exe_path = exe_dir.join(exe_name);
    if exe_path.exists() {
        return Ok(config.fill(exe_dir, true, "CURRENT_DIR"));
    }
    let exe_parent_dir = exe_dir.parent();
    if exe_parent_dir.is_none() {
        return Err("Failed to get exe parent dir".to_string());
    }
    let exe_parent_dir = exe_parent_dir.unwrap();
    let exe_path = exe_parent_dir.join(exe_name);
    if exe_path.exists() {
        return Ok(config.fill(exe_parent_dir, true, "PARENT_DIR"));
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
    let program_files = std::env::var("ProgramFiles");
    if program_files.is_err() {
        return Err(format!(
            "Failed to get ProgramFiles: {:?}",
            program_files.err()
        ));
    }
    let program_files = program_files.unwrap();
    let program_files_real_path = Path::new(&program_files).join(program_files_path);
    let program_files_exe_path = program_files_real_path.join(exe_name);
    Ok(config.fill(
        &program_files_real_path,
        program_files_exe_path.exists(),
        "DEFAULT",
    ))
}

#[tauri::command]
pub async fn create_lnk(target: String, lnk: String) -> Result<(), String> {
    let target = Path::new(&target);
    let lnk = Path::new(&lnk);
    let lnk_dir = lnk.parent();
    if lnk_dir.is_none() {
        return Err("Failed to get lnk parent dir".to_string());
    }
    let lnk_dir = lnk_dir.unwrap();
    tokio::fs::create_dir_all(lnk_dir)
        .await
        .map_err(|e| format!("Failed to create lnk dir: {:?}", e))?;
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
        let mut self_configured_mmap = crate::local::get_base_with_config().await?;
        let output_file = tokio::fs::File::create(&uninstaller_path)
            .await
            .map_err(|e| format!("Failed to create uninstaller: {:?}", e))?;
        let mut output = tokio::io::BufWriter::new(output_file);
        tokio::io::copy(&mut self_configured_mmap, &mut output)
            .await
            .map_err(|e| format!("Failed to write uninstaller: {:?}", e))?;
        // flush
        output
            .flush()
            .await
            .map_err(|e| format!("Failed to flush: {:?}", e))?;
        let res = tokio::fs::copy(&uninstaller_path, &updater_path).await;
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

#[tauri::command]
pub async fn run_uninstall(
    source: String,
    files: Vec<String>,
    user_data_path: Vec<String>,
    extra_uninstall_path: Vec<String>,
    reg_name: String,
) -> Result<Vec<String>, String> {
    let exe_path = std::env::current_exe();
    if exe_path.is_err() {
        return Err(format!(
            "Failed to get current exe path: {:?}",
            exe_path.err()
        ));
    }
    let exe_path = exe_path.unwrap();
    let mut tmp_uninstaller_path = exe_path.clone();
    // check if exe_path is in source
    if DELETE_SELF_ON_EXIT_PATH.read().unwrap().is_none() && exe_path.starts_with(&source) {
        let tmp_dir = std::env::temp_dir();
        tmp_uninstaller_path = tmp_dir.join(format!(
            "kachina.uninst.{}.exe",
            chrono::Utc::now().timestamp()
        ));
        // try to move current exe to tmp_uninstaller_path
        let res = tokio::fs::rename(&exe_path, &tmp_uninstaller_path).await;
        if res.is_err() {
            // move fail, maybe exe and tempdir is not in the same partition
            // try move to parent dir
            let source_parent = Path::new(&source).parent();
            if let Some(source_parent) = source_parent {
                tmp_uninstaller_path = source_parent.join(format!(
                    "kachina.uninst.{}.exe",
                    chrono::Utc::now().timestamp()
                ));
                let res = tokio::fs::rename(&exe_path, &tmp_uninstaller_path).await;
                if res.is_err() {
                    return Err(format!("Failed to move exe to parent dir: {:?}", res.err()));
                }
            } else {
                return Err("Insecure uninstall: installer is in root dir".to_string());
            }
        }
    }
    // write delete_on_exit value
    DELETE_SELF_ON_EXIT_PATH
        .write()
        .unwrap()
        .replace(tmp_uninstaller_path.to_string_lossy().to_string());

    // change cwd to %temp%
    let temp_dir = std::env::temp_dir();
    std::env::set_current_dir(&temp_dir).map_err(|e| format!("Failed to set cwd: {:?}", e))?;
    let delete_list = files
        .iter()
        .map(|f| Path::new(source.as_str()).join(f))
        .filter(|f| f.exists() && *f != exe_path)
        .collect::<Vec<_>>();
    let res = rm_list(delete_list).await;

    // delete user data
    // merge user_data_path and extra_uninstall_path
    let to_be_delete = [&user_data_path[..], &extra_uninstall_path[..]].concat();
    for pathstr in to_be_delete.iter() {
        let path = Path::new(pathstr);
        if path.exists() {
            // check if is file or dir
            if path.is_file() {
                tokio::fs::remove_file(path)
                    .await
                    .map_err(|e| format!("Failed to remove user data file {}: {:?}", pathstr, e))?;
            } else {
                tokio::fs::remove_dir_all(path).await.map_err(|e| {
                    format!("Failed to remove user data folder {}: {:?}", pathstr, e)
                })?;
            }
        }
    }

    // recursively delete empty folders
    let source_path = source.clone();
    tokio::task::spawn_blocking(move || {
        let path = Path::new(&source_path);
        run_clear_empty_dirs(path).map_err(|e| format!("Failed to clear empty dirs: {:?}", e))?;
        delete_dir_if_empty(path).map_err(|e| format!("Failed to clear empty dirs: {:?}", e))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("Failed to clear empty dirs: {:?}", e))??;

    // delete registry
    let _ = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).delete_subkey_all(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        reg_name
    ));

    Ok(res)
}

pub fn delete_self_on_exit() {
    let path = DELETE_SELF_ON_EXIT_PATH.read().unwrap();
    if path.is_none() {
        return;
    }
    let path = path.as_ref().unwrap();
    // run the cmd file with window hidden
    #[allow(clippy::zombie_processes)]
    let _ = std::process::Command::new("cmd")
        .arg("/C")
        .arg("ping")
        .arg("127.0.0.1")
        .arg("-n")
        .arg("2")
        .arg("&")
        .arg("del")
        .arg("/f")
        .arg("/q")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .unwrap();
}
