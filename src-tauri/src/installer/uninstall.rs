use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

lazy_static::lazy_static!(
    static ref DELETE_SELF_ON_EXIT_PATH: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
);

pub fn run_clear_empty_dirs(path: &Path) -> Result<(), std::io::Error> {
    let entries = std::fs::read_dir(path)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            run_clear_empty_dirs(&path)?;
            let entries = std::fs::read_dir(&path)?;
            if entries.count() == 0 {
                std::fs::remove_dir(&path)?;
            }
        }
    }
    Ok(())
}

pub fn delete_dir_if_empty(path: &Path) -> Result<(), std::io::Error> {
    let entries = std::fs::read_dir(path)?;
    if entries.count() == 0 {
        std::fs::remove_dir(path)?;
    }
    Ok(())
}

pub async fn rm_list(key: Vec<PathBuf>) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    for path in key {
        set.spawn(tokio::task::spawn_blocking(move || {
            let path = Path::new(&path);
            if path.exists() {
                let res = std::fs::remove_file(path);
                if res.is_err() {
                    return Err(format!("Failed to remove file: {:?}", res.err()));
                }
            }
            Ok(())
        }));
    }
    let res = set.join_all().await;
    let errs: Vec<String> = res
        .into_iter()
        .filter_map(|r| r.err())
        .map(|e| e.to_string())
        .collect();
    errs
}

pub async fn clear_empty_dirs(key: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = Path::new(&key);
        run_clear_empty_dirs(path).map_err(|e| format!("Failed to clear empty dirs: {:?}", e))?;
        delete_dir_if_empty(path).map_err(|e| format!("Failed to clear empty dirs: {:?}", e))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("Failed to clear empty dirs: {:?}", e))?
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct RunUninstallArgs {
    source: String,
    files: Vec<String>,
    user_data_path: Vec<String>,
    extra_uninstall_path: Vec<String>,
    reg_name: String,
    uninstall_name: String,
}
pub async fn run_uninstall_with_args(args: RunUninstallArgs) -> Result<Vec<String>, String> {
    run_uninstall(
        args.source,
        args.files,
        args.user_data_path,
        args.extra_uninstall_path,
        args.reg_name,
        args.uninstall_name,
    )
    .await
}

#[tauri::command]
pub async fn run_uninstall(
    source: String,
    files: Vec<String>,
    user_data_path: Vec<String>,
    extra_uninstall_path: Vec<String>,
    reg_name: String,
    uninstall_name: String,
) -> Result<Vec<String>, String> {
    let exe_path = std::env::current_exe();
    if exe_path.is_err() {
        return Err(format!(
            "Failed to get current exe path: {:?}",
            exe_path.err()
        ));
    }
    let exe_path = exe_path.unwrap();
    // check if exe_path is in source
    if DELETE_SELF_ON_EXIT_PATH.read().unwrap().is_none() && exe_path.starts_with(&source) {
        let tmp_dir = std::env::temp_dir();
        let mut tmp_uninstaller_path = tmp_dir.join(format!(
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
        // write delete_on_exit value
        DELETE_SELF_ON_EXIT_PATH
            .write()
            .unwrap()
            .replace(tmp_uninstaller_path.to_string_lossy().to_string());
    }

    let mut delete_list = files
        .iter()
        .map(|f| Path::new(source.as_str()).join(f))
        .filter(|f| f.exists() && *f != exe_path)
        .collect::<Vec<_>>();
    if !exe_path.starts_with(&source) {
        // external uninstaller
        delete_list.push(Path::new(source.as_str()).join(uninstall_name));
    }
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

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct CreateUninstallerArgs {
    source: String,
    uninstaller_name: String,
    updater_name: String,
}
pub async fn create_uninstaller_with_args(args: CreateUninstallerArgs) -> Result<(), String> {
    create_uninstaller(args.source, args.uninstaller_name, args.updater_name).await
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
    let updater_is_self = current_exe_path == updater_path;
    if !updater_is_self {
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
        // drop
        drop(output);
        // open again with rw
        clear_index_mark(&uninstaller_path).await?;
        // find
        let res = tokio::fs::copy(&uninstaller_path, &updater_path).await;
        if res.is_err() {
            return Err(format!("Failed to create updater: {:?}", res.err()));
        }
    } else {
        // try modify updater, if fail, silently ignore
        let _ = clear_index_mark(&updater_path).await;
    }
    Ok(())
}
pub async fn clear_index_mark(path: &PathBuf) -> Result<(), String> {
    // open again with rw
    let mut output_file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .await
        .map_err(|e| format!("Failed to modify updater: {:?}", e))?;
    // read first 256 bytes to buffer
    let mut buffer = [0u8; 256];
    let read = output_file.read(&mut buffer).await;
    if let Err(e) = read {
        return Err(format!("Failed to read updater: {:?}", e));
    }
    // check ! and K
    let mark_pos = buffer.windows(2).position(|w| w == b"!K".as_ref());
    if let Some(mark_pos) = mark_pos {
        // check if equals !KachinaInstaller!
        let mark_str = "!KachinaInstaller!";
        let mark_real = String::from_utf8_lossy(&buffer[mark_pos..mark_pos + mark_str.len()]);
        if mark_real == mark_str {
            let index_start = mark_pos + mark_str.len();
            // PE header replaced with index. Remove it.
            // write 5*4 bytes of 0 after index_start
            let res = output_file
                .seek(tokio::io::SeekFrom::Start(index_start as u64))
                .await;
            if let Err(e) = res {
                return Err(format!("Failed to seek uninstaller: {:?}", e));
            }
            let zero = [0u8; 5 * 4];
            let res = output_file.write(&zero).await;
            if let Err(e) = res {
                return Err(format!("Failed to write uninstaller: {:?}", e));
            }
        }
    }
    // close file
    drop(output_file);
    Ok(())
}
