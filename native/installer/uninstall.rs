use anyhow::Context;
use std::io;
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

use crate::fs::staging::{staging_root, Staging};
use crate::fs::{staged_write_error, write_staged_verified};
use crate::utils::code::{Coded, FILE_IO_FAILED};
use crate::utils::error::TAResult;
use crate::utils::process;
use crate::verified_write::Algorithm;

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
        set.spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Failed to remove file {}: {e}", path.display())),
        });
    }
    let mut errs = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => errs.push(e),
            Err(e) => errs.push(e.to_string()),
        }
    }
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
    pub uninstall_name: String,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default)]
pub struct UninstallOutcome {
    /// Paths that could not be removed. Removal continues past each failure.
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
        args.uninstall_name,
    )
    .await
}

/// Park the running executable under the staging directory's `old\` so the
/// install directory can be emptied. Same volume is guaranteed by
/// `staging_root`; a drive-root install has nowhere to go and is refused.
async fn park_self(exe_path: &Path, source: &str, uninstall_name: &str) -> anyhow::Result<String> {
    let root = staging_root(source, false)?;
    let staging = Staging::at(&root);
    staging.ensure_layout()?;
    let parked = staging.old_path(uninstall_name);
    let _ = tokio::fs::remove_file(&parked).await;
    tokio::fs::rename(exe_path, &parked)
        .await
        .context("SELF_UNINSTALL_ERR")?;
    Ok(root.to_string_lossy().to_string())
}

/// Remove each existing file or directory tree; missing paths are skipped and
/// a failure does not stop the rest. Returns one message per failed path.
pub async fn remove_paths(paths: &[String]) -> Vec<String> {
    let mut errs = Vec::new();
    for pathstr in paths {
        let path = Path::new(pathstr);
        if !path.exists() {
            continue;
        }
        let removed = if path.is_file() {
            tokio::fs::remove_file(path).await
        } else {
            tokio::fs::remove_dir_all(path).await
        };
        if let Err(e) = removed {
            errs.push(format!("Failed to remove {pathstr}: {e}"));
        }
    }
    errs
}

pub async fn run_uninstall(
    source: String,
    files: Vec<String>,
    user_data_path: Vec<String>,
    extra_uninstall_path: Vec<String>,
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
    let mut errors = rm_list(delete_list).await;
    errors.extend(remove_paths(&[&user_data_path[..], &extra_uninstall_path[..]].concat()).await);
    if let Err(e) = clear_empty_dirs(source).await {
        errors.push(format!("{e:#}"));
    }

    Ok(UninstallOutcome {
        errors,
        self_moved_to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[tokio::test]
    async fn park_self_moves_running_image_into_staging_old() {
        let base =
            crate::fs::staging::scratch_file(&format!("kachina-uninst-{}", uuid::Uuid::new_v4()));
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
        assert_eq!(
            delete_on_exit_path(),
            None,
            "park does not schedule by itself"
        );
        schedule_delete_on_exit(&root);
        assert_eq!(delete_on_exit_path().as_deref(), Some(root.as_str()));
        clear_delete_on_exit();
        drop(_running);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn rm_list_missing_is_ok_locked_is_err() {
        let dir = crate::fs::staging::scratch_file(&format!("kachina-rm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("gone.txt");
        assert!(rm_list(vec![missing]).await.is_empty());

        let ok = dir.join("ok.txt");
        std::fs::write(&ok, b"x").unwrap();
        assert!(rm_list(vec![ok.clone()]).await.is_empty());
        assert!(!ok.exists());

        let locked = dir.join("locked.txt");
        std::fs::write(&locked, b"x").unwrap();
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(&locked)
            .unwrap();
        let errs = rm_list(vec![locked.clone()]).await;
        assert!(!errs.is_empty(), "{errs:?}");
        assert!(locked.exists());
        drop(_hold);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn uninstall_continues_past_a_locked_file() {
        let base =
            crate::fs::staging::scratch_file(&format!("kachina-uninst-{}", uuid::Uuid::new_v4()));
        let install = base.join("app");
        let extra = base.join("extra");
        std::fs::create_dir_all(install.join("sub")).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        for name in ["locked.txt", "sub/a.txt", "b.txt"] {
            std::fs::write(install.join(name), b"x").unwrap();
        }
        std::fs::write(extra.join("c.txt"), b"x").unwrap();
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(install.join("locked.txt"))
            .unwrap();

        let outcome = run_uninstall(
            install.to_string_lossy().into_owned(),
            vec!["locked.txt".into(), "sub/a.txt".into(), "b.txt".into()],
            Vec::new(),
            vec![extra.to_string_lossy().into_owned()],
            "uninst.exe".into(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
        assert!(
            outcome.errors[0].contains("locked.txt"),
            "{:?}",
            outcome.errors
        );
        assert!(install.join("locked.txt").exists());
        assert!(!install.join("b.txt").exists());
        assert!(
            !install.join("sub").exists(),
            "emptied folders are still cleared"
        );
        assert!(!extra.exists(), "later paths are still removed");
        drop(_hold);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The installer's own image is staged without its layout header; a copied
    /// file keeps every byte, layout header included.
    #[test]
    fn only_the_own_image_loses_its_layout_header() {
        let dir =
            crate::fs::staging::scratch_file(&format!("kachina-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut image: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
        image[0x4E..0x4E + 18].copy_from_slice(crate::pack_format::LAYOUT_MARKER);
        image[0x4E + 18..0x4E + 38].fill(0x5A);
        let mut cleared = image.clone();
        crate::pack_format::clear_layout(&mut cleared);

        let own = dir.join("uninstall.exe");
        write_image(ImageSource::Own(image.clone()), &own, Algorithm::Md5).unwrap();
        assert_eq!(std::fs::read(&own).unwrap(), cleared);

        let source = dir.join("source.exe");
        std::fs::write(&source, &image).unwrap();
        let copy = dir.join("updater.exe");
        write_image(ImageSource::File(source), &copy, Algorithm::Xxh).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), image);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_cleanup_deletes_ampersand_path_and_spares_sibling() {
        let root =
            crate::fs::staging::scratch_file(&format!("kachina-clean-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let staged = root.join("应用 & Games.kachina-staged");
        std::fs::create_dir_all(staged.join("old")).unwrap();
        std::fs::write(staged.join("old").join("a.txt"), b"x").unwrap();
        let sentinel = root.join("keep.txt");
        std::fs::write(&sentinel, b"y").unwrap();

        let child = spawn_staging_cleanup(&staged.to_string_lossy()).unwrap();
        child.wait_blocking().unwrap();

        assert!(
            !staged.exists(),
            "staged dir should be gone: {}",
            staged.display()
        );
        assert!(sentinel.exists(), "sibling sentinel must stay");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn staging_cleanup_retries_until_lock_released() {
        let root =
            crate::fs::staging::scratch_file(&format!("kachina-clean-{}", uuid::Uuid::new_v4()));
        let staged = root.join("Apps&Games.kachina-staged");
        std::fs::create_dir_all(&staged).unwrap();
        let locked = staged.join("held.txt");
        std::fs::write(&locked, b"x").unwrap();
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(&locked)
            .unwrap();

        let child = spawn_staging_cleanup(&staged.to_string_lossy()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(staged.exists(), "rmdir must wait while the file is held");
        drop(hold);
        child.wait_blocking().unwrap();
        assert!(!staged.exists(), "dir goes away after the lock is dropped");
        let _ = std::fs::remove_dir_all(&root);
    }
}

pub fn delete_self_on_exit() {
    // 幂等：WebView 消息循环（WM_CLOSE / WM_QUIT）与 main 的最终收尾都会调用，
    // 只清理一次。
    let Some(path) = DELETE_SELF_ON_EXIT_PATH
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    else {
        return;
    };
    let _ = spawn_staging_cleanup(&path);
}

/// `ping -n 2` ≈ 1s; 20 tries cap the wait at about 20s. The path is only in
/// `%KACHINA_CLEANUP%`, so `&` in the directory name is not cmd syntax.
const CLEANUP_CMD: &str = r#"for /L %i in (1,1,20) do @if exist "%KACHINA_CLEANUP%" (rmdir /s /q "%KACHINA_CLEANUP%" & ping 127.0.0.1 -n 2 >nul)"#;

fn spawn_staging_cleanup(path: &str) -> io::Result<process::Child> {
    process::spawn_system_cmd(CLEANUP_CMD, &[("KACHINA_CLEANUP", path)], true)
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
    let algorithm = Algorithm::from_name(&args.hash_algorithm)
        .ok_or_else(|| anyhow::anyhow!("NO_HASH_ALGO_ERR"))?;
    let own_image = match &args.copy_from {
        Some(_) => None,
        None => {
            let mut image = crate::local::get_base_with_config().await?;
            let mut bytes = Vec::new();
            image
                .read_to_end(&mut bytes)
                .await
                .context("CREATE_UNINSTALLER_ERR")?;
            Some(bytes)
        }
    };
    let mut out = Vec::new();
    for name in &args.names {
        if !crate::fs::staging::is_safe_rel(name) {
            return Err(anyhow::Error::from(Coded::bare(FILE_IO_FAILED)).into());
        }
        let staged = crate::fs::staging::join_rel(new_dir, name);
        if let Some(parent) = staged.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("CREATE_DIR_ERR")?;
        }
        let source = match (&args.copy_from, &own_image) {
            (Some(src), _) => ImageSource::File(PathBuf::from(src)),
            (None, Some(bytes)) => ImageSource::Own(bytes.clone()),
            (None, None) => unreachable!("own image is read when nothing is copied"),
        };
        let target = staged.clone();
        let hash = tokio::task::spawn_blocking(move || write_image(source, &target, algorithm))
            .await
            .context("STAGE_IMAGE_THREAD_ERR")??;
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

/// Where a staged image comes from.
enum ImageSource {
    /// The running installer's `base + config`; its layout header is cleared
    /// on the way out, since the image carries no packed data.
    Own(Vec<u8>),
    /// An existing file, duplicated byte for byte.
    File(PathBuf),
}

/// Write one image to `staged` and return the hash of what the file holds
/// (see [`write_staged_verified`]).
fn write_image(source: ImageSource, staged: &Path, algorithm: Algorithm) -> anyhow::Result<String> {
    let never = || false;
    let written = match source {
        ImageSource::Own(bytes) => {
            write_staged_verified(&mut bytes.as_slice(), staged, algorithm, true, &never)
        }
        ImageSource::File(path) => {
            let mut file = std::fs::File::open(path).context("CREATE_UPDATER_ERR")?;
            write_staged_verified(&mut file, staged, algorithm, false, &never)
        }
    };
    written.map_err(|failure| staged_write_error(failure, staged))
}
