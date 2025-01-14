use crate::fs::{delete_dir_if_empty, rm_list, run_clear_empty_dirs};
use std::{os::windows::process::CommandExt, path::Path};
use tokio::io::AsyncWriteExt;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

lazy_static::lazy_static!(
    static ref DELETE_SELF_ON_EXIT_PATH: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
);

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
