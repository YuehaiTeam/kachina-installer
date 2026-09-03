//! Staging directory for the two-phase file commit (see the file commit
//! protocol note). One directory per install path, derived deterministically
//! so a later run can find what an interrupted one left behind.
//!
//! Layout under the root: `new\<rel>` produced files, `old\<rel>` displaced
//! files, `dl\` scratch downloads (runtimes, WebView2 bootstrapper, Mirror酱
//! archive), `journal` the commit manifest, `lock` the owning pid.
//!
//! This module owns the process cwd and staging under `%TEMP%`. The log file
//! path in `main.rs` is the other `%TEMP%` write.

use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::Digest;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::session::plan::normalize_full;
use crate::utils::code::{
    Attach, Coded, INSTALL_PATH_INVALID, STAGING_IN_USE, TEMP_DIR_UNAVAILABLE,
};

pub const TEMP_BUCKET: &str = "kachina-staged";
pub const SIBLING_SUFFIX: &str = ".kachina-staged";
pub const JOURNAL: &str = "journal";
pub const LOCK: &str = "lock";

/// Switch the process cwd to `%TEMP%` so it never pins the install directory
/// (a directory with a process cwd inside cannot be renamed or removed).
pub fn enter_neutral_cwd() -> anyhow::Result<()> {
    let temp = std::env::temp_dir();
    std::env::set_current_dir(&temp).map_err(|e| anyhow::Error::new(e).attach(TEMP_DIR_UNAVAILABLE))
}

/// A scratch path under `%TEMP%` for the few downloads that happen before any
/// staging directory exists (the WebView2 bootstrapper).
pub fn scratch_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn volume_root(path: &Path) -> Option<String> {
    let src = wide(path);
    let mut buf = vec![0u16; 261];
    unsafe { GetVolumePathNameW(PCWSTR(src.as_ptr()), &mut buf) }.ok()?;
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]).to_ascii_lowercase())
}

/// Nearest existing ancestor (or the path itself); volume queries need a real
/// path.
fn existing_ancestor(path: &Path) -> PathBuf {
    path.ancestors()
        .find(|p| p.exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf())
}

/// Both paths resolve to the same volume root (`GetVolumePathNameW`). Paths
/// that do not exist yet are judged by their nearest existing ancestor.
pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (
        volume_root(&existing_ancestor(a)),
        volume_root(&existing_ancestor(b)),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Free bytes on the volume holding `path` (nearest existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let src = wide(&existing_ancestor(path));
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(src.as_ptr()), None, None, Some(&mut free)) }.ok()?;
    Some(free)
}

/// First 16 hex chars of sha256 over the normalized install path.
pub fn path_hash(install_dir: &str) -> String {
    let digest = sha2::Sha256::digest(normalize_full(install_dir).as_bytes());
    hex::encode(&digest[..8])
}

fn temp_candidate(install_dir: &str) -> PathBuf {
    std::env::temp_dir()
        .join(TEMP_BUCKET)
        .join(path_hash(install_dir))
}

fn sibling_candidate(install_dir: &Path) -> anyhow::Result<PathBuf> {
    let trimmed = install_dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    let trimmed = PathBuf::from(trimmed);
    let name = trimmed
        .file_name()
        .filter(|_| trimmed.parent().is_some_and(|p| !p.as_os_str().is_empty()))
        .ok_or_else(|| {
            anyhow::Error::from(Coded::bare_with(
                INSTALL_PATH_INVALID,
                install_dir.to_string_lossy(),
            ))
        })?;
    let mut sibling = name.to_os_string();
    sibling.push(SIBLING_SUFFIX);
    Ok(trimmed.with_file_name(sibling))
}

/// Where a fresh staging directory for `install_dir` goes: `%TEMP%` when it is
/// on the same volume (invisible to the user), else a sibling directory
/// (rename must stay on one volume to be atomic).
pub fn staging_root(install_dir: &str) -> anyhow::Result<PathBuf> {
    let dir = Path::new(install_dir);
    if same_volume(dir, &std::env::temp_dir()) {
        Ok(temp_candidate(install_dir))
    } else {
        sibling_candidate(dir)
    }
}

/// Every location a previous run may have used, sibling first.
fn candidates(install_dir: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(sibling) = sibling_candidate(Path::new(install_dir)) {
        out.push(sibling);
    }
    out.push(temp_candidate(install_dir));
    out
}

pub fn pid_alive(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code).is_ok();
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE.0 as u32
    }
}

fn lock_holder(root: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(root.join(LOCK)).ok()?;
    text.trim().parse().ok()
}

/// One staging directory, already on disk with our `lock` inside.
#[derive(Debug, Clone)]
pub struct Staging {
    root: PathBuf,
}

/// Result of opening the staging area for an install directory.
#[derive(Debug)]
pub struct Opened {
    pub staging: Staging,
    /// Journal text left by an interrupted commit, if any.
    pub journal: Option<String>,
}

impl Staging {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn new_dir(&self) -> PathBuf {
        self.root.join("new")
    }

    pub fn old_dir(&self) -> PathBuf {
        self.root.join("old")
    }

    pub fn dl_dir(&self) -> PathBuf {
        self.root.join("dl")
    }

    pub fn journal_path(&self) -> PathBuf {
        self.root.join(JOURNAL)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK)
    }

    pub fn new_path(&self, rel: &str) -> PathBuf {
        join_rel(&self.new_dir(), rel)
    }

    pub fn old_path(&self, rel: &str) -> PathBuf {
        join_rel(&self.old_dir(), rel)
    }

    /// Look for a previous run's directory (sibling, then `%TEMP%`), refuse if
    /// another live installer owns one, keep the one with a journal, delete the
    /// rest, and otherwise create a fresh directory at [`staging_root`].
    pub fn open(install_dir: &str) -> anyhow::Result<Opened> {
        let candidates = candidates(install_dir);
        let mut keep: Option<PathBuf> = None;
        for cand in &candidates {
            if !cand.exists() {
                continue;
            }
            if let Some(pid) = lock_holder(&cand) {
                if pid != std::process::id() && pid_alive(pid) {
                    return Err(anyhow::Error::from(Coded::bare_with(
                        STAGING_IN_USE,
                        install_dir,
                    )));
                }
            }
            if keep.is_none() && cand.join(JOURNAL).is_file() {
                keep = Some(cand.clone());
            }
        }
        let has_journal = keep.is_some();
        let root = match keep {
            Some(root) => root,
            None => staging_root(install_dir)?,
        };
        let staging = Staging::at(root);
        staging.ensure_layout()?;
        claim_lock(&staging.lock_path(), install_dir)?;
        if !has_journal {
            // The lock must be owned before stale contents are removed. A
            // second opener may have observed the same empty root earlier.
            for dir in [staging.new_dir(), staging.old_dir(), staging.dl_dir()] {
                remove_tree(&dir);
            }
            let _ = std::fs::remove_file(staging.journal_path());
            staging.ensure_layout()?;
        }
        // Old candidates are only residue after this process has claimed its
        // selected root. Keep any candidate that became live meanwhile.
        for cand in candidates {
            if cand == staging.root() {
                continue;
            }
            let live = lock_holder(&cand)
                .filter(|pid| *pid != std::process::id())
                .is_some_and(pid_alive);
            if !live {
                remove_tree(&cand);
            }
        }
        let journal = std::fs::read_to_string(staging.journal_path()).ok();
        Ok(Opened { staging, journal })
    }

    pub fn ensure_layout(&self) -> anyhow::Result<()> {
        for dir in [self.new_dir(), self.old_dir(), self.dl_dir()] {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create {}", dir.display()))
                .attach(TEMP_DIR_UNAVAILABLE)?;
        }
        Ok(())
    }

    /// Remove everything. Best effort: a locked file leaves a residue that the
    /// next session's `open` deletes.
    pub fn discard(&self) {
        remove_tree(&self.root);
    }
}

fn in_use(install_dir: &str) -> anyhow::Error {
    anyhow::Error::from(Coded::bare_with(STAGING_IN_USE, install_dir))
}

fn claim_lock(path: &Path, install_dir: &str) -> anyhow::Result<()> {
    let pid = std::process::id();
    let create = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(pid.to_string().as_bytes())?;
        Ok(())
    };
    match create() {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(holder) = lock_holder(path.parent().unwrap_or(path)) {
                if holder != pid && pid_alive(holder) {
                    return Err(in_use(install_dir));
                }
            }
            let _ = std::fs::remove_file(path);
            match create() {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    Err(in_use(install_dir))
                }
                Err(err) => Err(err).context("write staging lock"),
            }
        }
        Err(err) => Err(err).context("write staging lock"),
    }
}

/// Relative path that cannot escape `base` when joined: no absolute form,
/// drive prefix, `.`, or `..` components. Empty is the directory itself.
pub fn is_safe_rel(rel: &str) -> bool {
    let n = rel.replace('/', "\\");
    if n.starts_with('\\') || n.contains(':') {
        return false;
    }
    let n = n.trim_end_matches('\\');
    if n.is_empty() {
        return true;
    }
    n.split('\\')
        .filter(|p| !p.is_empty())
        .all(|p| p != "." && p != "..")
}

pub fn try_join_rel(base: &Path, rel: &str) -> Option<PathBuf> {
    if !is_safe_rel(rel) {
        return None;
    }
    let mut out = base.to_path_buf();
    for part in rel.split(['/', '\\']).filter(|p| !p.is_empty()) {
        out.push(part);
    }
    Some(out)
}

pub fn join_rel(base: &Path, rel: &str) -> PathBuf {
    try_join_rel(base, rel).unwrap_or_else(|| base.to_path_buf())
}

pub fn remove_tree(path: &Path) {
    if path.exists() {
        if let Err(err) = std::fs::remove_dir_all(path) {
            tracing::warn!("remove {} failed: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachina-staging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_hash_ignores_case_and_slashes() {
        assert_eq!(path_hash(r"C:\Apps\Foo"), path_hash("c:/apps/foo/"));
        assert_eq!(path_hash(r"C:\Apps\Foo").len(), 16);
        assert_ne!(path_hash(r"C:\Apps\Foo"), path_hash(r"C:\Apps\Bar"));
    }

    #[test]
    fn staging_root_same_volume_goes_to_temp() {
        let install = tmp().join("app");
        let root = staging_root(&install.to_string_lossy()).unwrap();
        assert!(root.starts_with(std::env::temp_dir().join(TEMP_BUCKET)));
        assert!(root.ends_with(path_hash(&install.to_string_lossy())));
    }

    #[test]
    fn sibling_candidate_shape_and_root_rejection() {
        let sib = sibling_candidate(Path::new(r"D:\games\app\")).unwrap();
        assert_eq!(sib, PathBuf::from(r"D:\games\app.kachina-staged"));
        let err = sibling_candidate(Path::new(r"D:\")).unwrap_err();
        assert!(matches!(
            crate::utils::code::extract(&err),
            crate::utils::code::Extracted::Coded(c) if c.code == INSTALL_PATH_INVALID
        ));
    }

    #[test]
    fn open_keeps_journal_dir_and_deletes_residue() {
        let base = tmp();
        let install = base.join("app");
        let install_s = install.to_string_lossy().to_string();
        let root = temp_candidate(&install_s);
        std::fs::create_dir_all(root.join("new")).unwrap();
        std::fs::write(root.join("new").join("junk"), b"x").unwrap();
        let opened = Staging::open(&install_s).unwrap();
        assert!(!opened.staging.new_path("junk").exists(), "residue removed");
        assert!(opened.journal.is_none());
        assert_eq!(
            std::fs::read_to_string(opened.staging.lock_path()).unwrap(),
            std::process::id().to_string()
        );

        std::fs::write(opened.staging.journal_path(), "kachina-journal 1\n").unwrap();
        std::fs::write(opened.staging.new_path("keep"), b"k").unwrap();
        let again = Staging::open(&install_s).unwrap();
        assert_eq!(again.journal.as_deref(), Some("kachina-journal 1\n"));
        assert!(again.staging.new_path("keep").exists());
        again.staging.discard();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_refuses_live_lock() {
        let base = tmp();
        let install = base.join("app2");
        let install_s = install.to_string_lossy().to_string();
        let root = temp_candidate(&install_s);
        std::fs::create_dir_all(&root).unwrap();
        // a pid that is certainly alive and is not us: the parent shell is
        // unknowable here, so spawn a sleeper
        let child =
            crate::utils::process::spawn("cmd", &["/C", "ping", "127.0.0.1", "-n", "3"], true)
                .unwrap();
        let pid = child.pid();
        std::fs::write(root.join(LOCK), pid.to_string()).unwrap();
        let err = Staging::open(&install_s).unwrap_err();
        assert!(matches!(
            crate::utils::code::extract(&err),
            crate::utils::code::Extracted::Coded(c) if c.code == STAGING_IN_USE
        ));
        let _ = child.wait_blocking();
        remove_tree(&root);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn join_rel_rejects_escape() {
        assert!(!is_safe_rel("..\\x"));
        assert!(!is_safe_rel("../x"));
        assert!(!is_safe_rel("foo/../bar"));
        assert!(!is_safe_rel("C:\\abs"));
        assert!(!is_safe_rel("/abs"));
        assert!(is_safe_rel(""));
        assert!(is_safe_rel("lib/a.dll"));
        let base = Path::new(r"D:\app");
        assert!(try_join_rel(base, "../x").is_none());
        assert_eq!(
            try_join_rel(base, "lib/a.dll").unwrap(),
            PathBuf::from(r"D:\app\lib\a.dll")
        );
    }

    #[test]
    fn open_replaces_stale_lock() {
        let base = tmp();
        let install = base.join("app3");
        let install_s = install.to_string_lossy().to_string();
        let root = temp_candidate(&install_s);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(LOCK), "1").unwrap();
        let opened = Staging::open(&install_s).unwrap();
        assert_eq!(
            std::fs::read_to_string(opened.staging.lock_path()).unwrap(),
            std::process::id().to_string()
        );
        opened.staging.discard();
        let _ = std::fs::remove_dir_all(&base);
    }
}
