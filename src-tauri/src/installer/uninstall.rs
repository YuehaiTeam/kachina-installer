use anyhow::Context;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::fs::staging::{staging_root, Staging};
use crate::utils::error::TAResult;
use crate::utils::process;

lazy_static::lazy_static!(
    /// Staging directory to remove after this process exits. It holds the
    /// running executable under `old\` (self-update or self-uninstall), which
    /// can be renamed but not deleted while it runs.
    static ref DELETE_SELF_ON_EXIT_PATH: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
);

/// The only writer of the exit-time cleanup path. Called once the swap that
/// moved the running executable has fully succeeded.
pub fn schedule_delete_on_exit(staging_root: &str) {
    DELETE_SELF_ON_EXIT_PATH
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .replace(staging_root.to_string());
}

#[cfg(test)]
pub fn delete_on_exit_path() -> Option<String> {
    DELETE_SELF_ON_EXIT_PATH
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(test)]
pub fn clear_delete_on_exit() {
    DELETE_SELF_ON_EXIT_PATH
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .take();
}

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

pub async fn clear_empty_dirs(key: String) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        let path = Path::new(&key);
        run_clear_empty_dirs(path)?;
        delete_dir_if_empty(path)?;
        Ok(())
    })
    .await
    .context("CLEAR_EMPTY_DIR_ERR")?
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct RunUninstallArgs {
    pub source: String,
    pub files: Vec<String>,
    pub user_data_path: Vec<String>,
    pub extra_uninstall_path: Vec<String>,
    pub reg_name: String,
    pub uninstall_name: String,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default)]
pub struct UninstallOutcome {
    pub errors: Vec<String>,
    /// Staging root the running uninstaller was parked in (`old\<name>`); the
    /// session schedules it for deletion at exit.
    pub self_moved_to: Option<String>,
}

pub async fn run_uninstall_with_args(args: RunUninstallArgs) -> TAResult<UninstallOutcome> {
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

/// Park the running executable under the staging directory's `old\` so the
/// install directory can be emptied. Same volume is guaranteed by
/// `staging_root`; a drive-root install has nowhere to go and is refused.
async fn park_self(exe_path: &Path, source: &str, uninstall_name: &str) -> anyhow::Result<String> {
    let root = staging_root(source)?;
    let staging = Staging::at(&root);
    staging.ensure_layout()?;
    let parked = staging.old_path(uninstall_name);
    let _ = tokio::fs::remove_file(&parked).await;
    tokio::fs::rename(exe_path, &parked)
        .await
        .context("SELF_UNINSTALL_ERR")?;
    Ok(root.to_string_lossy().to_string())
}

pub async fn run_uninstall(
    source: String,
    files: Vec<String>,
    user_data_path: Vec<String>,
    extra_uninstall_path: Vec<String>,
    reg_name: String,
    uninstall_name: String,
) -> TAResult<UninstallOutcome> {
    let exe_path = std::env::current_exe().context("GET_EXE_PATH_ERR")?;
    let self_moved_to = if exe_path.starts_with(&source) {
        Some(park_self(&exe_path, &source, &uninstall_name).await?)
    } else {
        None
    };

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
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to remove user data file {}: {:?}", pathstr, e)
                    })
                    .context("RM_USERDATA_ERR")?;
            } else {
                tokio::fs::remove_dir_all(path)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to remove user data folder {}: {:?}", pathstr, e)
                    })
                    .context("RM_USERDATA_ERR")?;
            }
        }
    }

    // recursively delete empty folders
    clear_empty_dirs(source).await?;

    // delete registry - try both HKLM and HKCU since installation could have used either
    let reg_path = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{reg_name}");
    let _ = windows_registry::LOCAL_MACHINE.remove_tree(&reg_path);
    let _ = windows_registry::CURRENT_USER.remove_tree(&reg_path);

    Ok(UninstallOutcome {
        errors: res,
        self_moved_to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[tokio::test]
    async fn park_self_moves_running_image_into_staging_old() {
        let base = crate::fs::staging::scratch_file(&format!("kachina-uninst-{}", uuid::Uuid::new_v4()));
        let install = base.join("app");
        std::fs::create_dir_all(&install).unwrap();
        let exe = install.join("uninst.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        // a running image: readable, renamable, not deletable
        let _running = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x4)
            .open(&exe)
            .unwrap();
        let root = park_self(&exe, &install.to_string_lossy(), "uninst.exe")
            .await
            .unwrap();
        assert!(!exe.exists());
        let parked = Staging::at(&root).old_path("uninst.exe");
        assert_eq!(std::fs::read(&parked).unwrap(), b"MZ");
        assert_eq!(delete_on_exit_path(), None, "park does not schedule by itself");
        schedule_delete_on_exit(&root);
        assert_eq!(delete_on_exit_path().as_deref(), Some(root.as_str()));
        clear_delete_on_exit();
        drop(_running);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&base);
    }
}

pub fn delete_self_on_exit() {
    let path = DELETE_SELF_ON_EXIT_PATH.read().unwrap();
    if path.is_none() {
        return;
    }
    let path = path.as_ref().unwrap();
    // 子进程独立于本进程存活；ping 拖延约 1 秒等本进程退出后再删整个暂存目录。
    let _ = process::spawn(
        "cmd",
        &[
            "/C",
            "ping",
            "127.0.0.1",
            "-n",
            "2",
            "&",
            "rmdir",
            "/s",
            "/q",
            path.as_str(),
        ],
        true,
    );
}

/// Stage the installer image (uninstaller / updater) under `new\` as phase-one
/// products. `copy_from` names an existing file to duplicate (the updater the
/// metadata shipped); otherwise the running executable's `base + config` is
/// written and its packed index mark cleared.
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct StageSelfImageArgs {
    pub install_dir: String,
    pub new_dir: String,
    pub hash_algorithm: String,
    pub names: Vec<String>,
    pub copy_from: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, PartialEq, Eq)]
pub struct StagedImage {
    pub rel: String,
    pub hash: String,
    /// Hash of the file currently in the install directory, if any.
    pub old: Option<String>,
    /// The install directory already holds identical bytes; the staged copy
    /// was removed and no unit is needed.
    pub unchanged: bool,
}

pub async fn stage_self_image(args: StageSelfImageArgs) -> TAResult<Vec<StagedImage>> {
    let new_dir = Path::new(&args.new_dir);
    let install = Path::new(&args.install_dir);
    let mut out = Vec::new();
    for name in &args.names {
        let staged = crate::fs::staging::join_rel(new_dir, name);
        if let Some(parent) = staged.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("CREATE_DIR_ERR")?;
        }
        match &args.copy_from {
            Some(src) => {
                tokio::fs::copy(src, &staged)
                    .await
                    .context("CREATE_UPDATER_ERR")?;
            }
            None => {
                let mut image = crate::local::get_base_with_config().await?;
                let file = tokio::fs::File::create(&staged)
                    .await
                    .context("CREATE_UNINSTALLER_ERR")?;
                let mut writer = tokio::io::BufWriter::new(file);
                tokio::io::copy(&mut image, &mut writer)
                    .await
                    .context("CREATE_UNINSTALLER_ERR")?;
                writer.flush().await.context("CREATE_UNINSTALLER_ERR")?;
                drop(writer);
                clear_index_mark(&staged).await?;
            }
        }
        crate::fs::sync_staged_file(&staged).await?;
        let hash = crate::utils::hash::run_hash(&args.hash_algorithm, &staged.to_string_lossy()).await?;
        let existing = crate::fs::staging::join_rel(install, name);
        let old = if existing.is_file() {
            crate::utils::hash::run_hash(&args.hash_algorithm, &existing.to_string_lossy())
                .await
                .ok()
        } else {
            None
        };
        let unchanged = old.as_deref() == Some(hash.as_str());
        if unchanged {
            let _ = tokio::fs::remove_file(&staged).await;
        }
        out.push(StagedImage {
            rel: name.replace('\\', "/"),
            hash,
            old,
            unchanged,
        });
    }
    Ok(out)
}
pub async fn clear_index_mark(path: &PathBuf) -> anyhow::Result<()> {
    // open again with rw
    let mut output_file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .await
        .context("SELF_UPDATE_ERR")?;
    // read first 256 bytes to buffer
    let mut buffer = [0u8; 256];
    output_file
        .read_exact(&mut buffer)
        .await
        .context("SELF_UPDATE_ERR")?;

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
            output_file
                .seek(tokio::io::SeekFrom::Start(index_start as u64))
                .await
                .context("SELF_UPDATE_ERR")?;
            let zero = [0u8; 5 * 4];
            output_file
                .write_all(&zero)
                .await
                .context("SELF_UPDATE_ERR")?;
        }
    }
    // close file
    output_file.flush().await.context("SELF_UPDATE_ERR")?;
    output_file.sync_all().await.context("SELF_UPDATE_ERR")?;
    drop(output_file);
    Ok(())
}
