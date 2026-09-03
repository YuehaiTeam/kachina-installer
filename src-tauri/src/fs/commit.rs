//! Phase two of the file commit protocol: swap the files produced under the
//! staging directory's `new\` into the install directory with same-volume
//! renames, journaled so an interrupted swap can be finished (or undone) by
//! the next run.
//!
//! Units are the granularity of the swap: a file, a whole directory subtree,
//! a deletion, or a copy (files under a reparse point, where rename would
//! cross volumes). Every unit records the hash it expects before and after
//! so recovery can tell "not swapped yet", "already swapped" and "someone
//! changed this" apart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::fs::staging::{join_rel, remove_tree, Staging};
use crate::ipc::{Progress, ProgressNotify};
use crate::utils::code::{code_for_local_io, Attach, FILE_IO_FAILED};
use crate::utils::hash::hash_file;

pub const JOURNAL_VERSION: &str = "kachina-journal 1";
const BACKOFF_MS: &[u64] = &[50, 100, 200, 400, 800];
const ROOT_OLD_NAME: &str = "~root";
const COPY_TMP: &str = ".kachina-tmp";
const COPY_OLD: &str = ".kachina-old";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path with `/` separators.
    pub rel: String,
    /// Hash of the file the target held before the swap; `None` when it did
    /// not exist (or is unknown, Mirror酱 path).
    pub old: Option<String>,
    pub new: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    File(FileEntry),
    /// Whole subtree swap; `rel` empty means the install directory itself.
    Dir {
        rel: String,
        files: Vec<FileEntry>,
    },
    Del {
        rel: String,
        old: Option<String>,
    },
    Copy(FileEntry),
}

impl Unit {
    pub fn rel(&self) -> &str {
        match self {
            Unit::File(f) | Unit::Copy(f) => &f.rel,
            Unit::Dir { rel, .. } | Unit::Del { rel, .. } => rel,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    pub hash_algorithm: String,
    /// Mirror酱 archive digest; `None` for DFS commits.
    pub archive: Option<String>,
    pub units: Vec<Unit>,
}

fn opt(h: &Option<String>) -> &str {
    h.as_deref().unwrap_or("-")
}

fn parse_opt(s: &str) -> Option<String> {
    if s == "-" {
        None
    } else {
        Some(s.to_string())
    }
}

fn rel_under(rel: &str, dir: &str) -> bool {
    dir.is_empty() || rel.starts_with(&format!("{dir}/"))
}

impl Journal {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(JOURNAL_VERSION);
        out.push('\n');
        out.push_str(&format!("hash\t{}\n", self.hash_algorithm));
        if let Some(a) = &self.archive {
            out.push_str(&format!("archive\t{a}\n"));
        }
        for unit in &self.units {
            match unit {
                Unit::File(f) => {
                    out.push_str(&format!("file\t{}\t{}\t{}\n", f.rel, opt(&f.old), f.new))
                }
                Unit::Copy(f) => {
                    out.push_str(&format!("copy\t{}\t{}\t{}\n", f.rel, opt(&f.old), f.new))
                }
                Unit::Del { rel, old } => out.push_str(&format!("del\t{rel}\t{}\n", opt(old))),
                Unit::Dir { rel, files } => {
                    out.push_str(&format!("dir\t{rel}\n"));
                    for f in files {
                        out.push_str(&format!("file\t{}\t{}\t{}\n", f.rel, opt(&f.old), f.new));
                    }
                }
            }
        }
        out
    }

    /// `None` when the version line does not match or a line is malformed:
    /// the caller drops the staging directory in both cases.
    pub fn parse(text: &str) -> Option<Journal> {
        let mut lines = text.lines();
        if lines.next()?.trim_end() != JOURNAL_VERSION {
            return None;
        }
        let mut hash_algorithm = None;
        let mut archive = None;
        let mut units: Vec<Unit> = Vec::new();
        let mut open_dir: Option<usize> = None;
        for line in lines {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            match parts.as_slice() {
                ["hash", algo] => hash_algorithm = Some(algo.to_string()),
                ["archive", digest] => archive = Some(digest.to_string()),
                ["dir", rel] => {
                    units.push(Unit::Dir {
                        rel: rel.to_string(),
                        files: Vec::new(),
                    });
                    open_dir = Some(units.len() - 1);
                }
                ["file", rel, old, new] => {
                    let entry = FileEntry {
                        rel: rel.to_string(),
                        old: parse_opt(old),
                        new: new.to_string(),
                    };
                    let mut attached = false;
                    if let Some(idx) = open_dir {
                        if let Unit::Dir { rel: dir, files } = &mut units[idx] {
                            if rel_under(&entry.rel, dir) {
                                files.push(entry.clone());
                                attached = true;
                            }
                        }
                    }
                    if !attached {
                        open_dir = None;
                        units.push(Unit::File(entry));
                    }
                }
                ["copy", rel, old, new] => {
                    open_dir = None;
                    units.push(Unit::Copy(FileEntry {
                        rel: rel.to_string(),
                        old: parse_opt(old),
                        new: new.to_string(),
                    }));
                }
                ["del", rel, old] => {
                    open_dir = None;
                    units.push(Unit::Del {
                        rel: rel.to_string(),
                        old: parse_opt(old),
                    });
                }
                _ => return None,
            }
        }
        Some(Journal {
            hash_algorithm: hash_algorithm?,
            archive,
            units,
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommitArgs {
    pub staging_root: String,
    pub install_dir: String,
    pub journal: Journal,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    /// The running executable was among the swapped targets; the staging
    /// directory still holds it under `old\` and must outlive the process.
    pub self_replaced: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RecoverOutcome {
    /// Every pending unit was swapped; same follow-up as a fresh commit.
    Completed { self_replaced: bool },
    /// The directory no longer matched the journal; nothing was renamed and
    /// the staging directory is gone.
    Discarded,
}

fn is_transient(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(32) | Some(33) | Some(5))
}

fn rename_retry(from: &Path, to: &Path, backoff: &[u64]) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if is_transient(&err) && attempt < backoff.len() => {
                std::thread::sleep(Duration::from_millis(backoff[attempt]));
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

fn local_err(err: std::io::Error, rel: &str) -> anyhow::Error {
    let code = code_for_local_io(&err);
    anyhow::Error::new(err).attach_with(code, rel)
}

/// Files under `dir`, recursive, as (`rel` with `/`, absolute). `None` when
/// a reparse point is encountered: such a tree is never a clean unit.
fn list_tree(dir: &Path) -> std::io::Result<Option<Vec<(String, PathBuf)>>> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = stack.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if meta.file_type().is_symlink() || is_reparse(&meta) {
                return Ok(None);
            }
            if meta.is_dir() {
                stack.push((entry.path(), rel));
            } else {
                out.push((rel, entry.path()));
            }
        }
    }
    Ok(Some(out))
}

pub fn is_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// The target directory holds exactly the unit's files (by name) and nothing
/// else. A missing directory is clean.
fn dir_is_clean(target: &Path, files: &[FileEntry]) -> bool {
    if !target.exists() {
        return true;
    }
    let Ok(Some(listing)) = list_tree(target) else {
        return false;
    };
    let want: std::collections::HashSet<String> = files.iter().map(|f| lower(&f.rel)).collect();
    listing.iter().all(|(rel, _)| want.contains(&lower(rel)))
}

#[derive(Default, Debug)]
struct FileState {
    old_moved: bool,
    new_placed: bool,
}

#[derive(Debug)]
enum UnitState {
    File(FileState),
    Copy(FileState),
    Del {
        old_moved: bool,
    },
    Dir {
        old_moved: bool,
        new_placed: bool,
        removed_empty_root: bool,
    },
    Degraded(Vec<(FileEntry, FileState)>),
    Skipped,
}

struct Ctx<'a> {
    staging: &'a Staging,
    install: &'a Path,
    algo: &'a str,
    current_exe: Option<PathBuf>,
    backoff: &'a [u64],
}

impl Ctx<'_> {
    fn target(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.install.to_path_buf()
        } else {
            join_rel(self.install, rel)
        }
    }

    fn new_of(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.staging.new_dir()
        } else {
            self.staging.new_path(rel)
        }
    }

    fn old_of(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.staging.old_dir().join(ROOT_OLD_NAME)
        } else {
            self.staging.old_path(rel)
        }
    }

    fn is_self(&self, target: &Path) -> bool {
        self.current_exe
            .as_ref()
            .is_some_and(|exe| same_path(exe, target))
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    crate::session::plan::normalize_full(&a.to_string_lossy())
        == crate::session::plan::normalize_full(&b.to_string_lossy())
}

fn apply_file(ctx: &Ctx, rel: &str, state: &mut FileState) -> anyhow::Result<()> {
    let target = ctx.target(rel);
    if target.exists() {
        rename_retry(&target, &ctx.old_of(rel), ctx.backoff).map_err(|e| local_err(e, rel))?;
        state.old_moved = true;
    }
    rename_retry(&ctx.new_of(rel), &target, ctx.backoff).map_err(|e| local_err(e, rel))?;
    state.new_placed = true;
    Ok(())
}

fn undo_file(ctx: &Ctx, rel: &str, state: &FileState) {
    let target = ctx.target(rel);
    if state.new_placed {
        if let Err(err) = rename_retry(&target, &ctx.new_of(rel), ctx.backoff) {
            tracing::warn!("rollback {rel}: move new back failed: {err}");
        }
    }
    if state.old_moved {
        if let Err(err) = rename_retry(&ctx.old_of(rel), &target, ctx.backoff) {
            tracing::warn!("rollback {rel}: restore old failed: {err}");
        }
    }
}

fn copy_paths(target: &Path) -> (PathBuf, PathBuf) {
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(COPY_TMP);
    let mut old = target.as_os_str().to_owned();
    old.push(COPY_OLD);
    (PathBuf::from(tmp), PathBuf::from(old))
}

fn apply_copy(ctx: &Ctx, entry: &FileEntry, state: &mut FileState) -> anyhow::Result<()> {
    let target = ctx.target(&entry.rel);
    let (tmp, old) = copy_paths(&target);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| local_err(e, &entry.rel))?;
    }
    std::fs::copy(ctx.new_of(&entry.rel), &tmp).map_err(|e| local_err(e, &entry.rel))?;
    let got = hash_file(ctx.algo, &tmp.to_string_lossy())?;
    if got != entry.new {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::anyhow!("copy verify mismatch").attach_with(FILE_IO_FAILED, &entry.rel));
    }
    if target.exists() {
        let _ = std::fs::remove_file(&old);
        rename_retry(&target, &old, ctx.backoff).map_err(|e| local_err(e, &entry.rel))?;
        state.old_moved = true;
    }
    rename_retry(&tmp, &target, ctx.backoff).map_err(|e| local_err(e, &entry.rel))?;
    state.new_placed = true;
    Ok(())
}

fn undo_copy(ctx: &Ctx, rel: &str, state: &FileState) {
    let target = ctx.target(rel);
    let (tmp, old) = copy_paths(&target);
    if state.new_placed {
        let _ = std::fs::remove_file(&target);
    }
    let _ = std::fs::remove_file(&tmp);
    if state.old_moved {
        if let Err(err) = rename_retry(&old, &target, ctx.backoff) {
            tracing::warn!("rollback copy {rel}: restore old failed: {err}");
        }
    }
}

fn finish_copy(target: &Path) {
    let (tmp, old) = copy_paths(target);
    let _ = std::fs::remove_file(tmp);
    let _ = std::fs::remove_file(old);
}

fn apply_unit(ctx: &Ctx, unit: &Unit) -> anyhow::Result<UnitState> {
    match unit {
        Unit::File(f) => {
            let mut st = FileState::default();
            match apply_file(ctx, &f.rel, &mut st) {
                Ok(()) => Ok(UnitState::File(st)),
                Err(e) => {
                    undo_file(ctx, &f.rel, &st);
                    Err(e)
                }
            }
        }
        Unit::Copy(f) => {
            let mut st = FileState::default();
            match apply_copy(ctx, f, &mut st) {
                Ok(()) => Ok(UnitState::Copy(st)),
                Err(e) => {
                    undo_copy(ctx, &f.rel, &st);
                    Err(e)
                }
            }
        }
        Unit::Del { rel, .. } => {
            let target = ctx.target(rel);
            if !target.exists() {
                return Ok(UnitState::Del { old_moved: false });
            }
            rename_retry(&target, &ctx.old_of(rel), ctx.backoff).map_err(|e| local_err(e, rel))?;
            Ok(UnitState::Del { old_moved: true })
        }
        Unit::Dir { rel, files } => apply_dir(ctx, rel, files),
    }
}

fn apply_dir(ctx: &Ctx, rel: &str, files: &[FileEntry]) -> anyhow::Result<UnitState> {
    let target = ctx.target(rel);
    let mut old_moved = false;
    let mut removed_empty_root = false;
    if target.exists() {
        let empty = std::fs::read_dir(&target)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if rel.is_empty() && empty {
            std::fs::remove_dir(&target).map_err(|e| local_err(e, rel))?;
            removed_empty_root = true;
        } else {
            match rename_retry(&target, &ctx.old_of(rel), ctx.backoff) {
                Ok(()) => old_moved = true,
                Err(err) => {
                    tracing::warn!("dir unit {rel:?} rename failed ({err}), degrading to files");
                    return apply_degraded(ctx, files);
                }
            }
        }
    }
    match rename_retry(&ctx.new_of(rel), &target, ctx.backoff) {
        Ok(()) => Ok(UnitState::Dir {
            old_moved,
            new_placed: true,
            removed_empty_root,
        }),
        Err(err) => {
            let st = UnitState::Dir {
                old_moved,
                new_placed: false,
                removed_empty_root,
            };
            undo_unit(ctx, rel, &st);
            Err(local_err(err, rel))
        }
    }
}

fn apply_degraded(ctx: &Ctx, files: &[FileEntry]) -> anyhow::Result<UnitState> {
    let mut states: Vec<(FileEntry, FileState)> = Vec::new();
    for f in files {
        let mut st = FileState::default();
        if let Err(err) = apply_file(ctx, &f.rel, &mut st) {
            states.push((f.clone(), st));
            undo_unit(ctx, "", &UnitState::Degraded(states));
            return Err(err);
        }
        states.push((f.clone(), st));
    }
    Ok(UnitState::Degraded(states))
}

fn undo_unit(ctx: &Ctx, rel: &str, state: &UnitState) {
    match state {
        UnitState::File(st) => undo_file(ctx, rel, st),
        UnitState::Copy(st) => undo_copy(ctx, rel, st),
        UnitState::Del { old_moved } => {
            if *old_moved {
                if let Err(err) = rename_retry(&ctx.old_of(rel), &ctx.target(rel), ctx.backoff) {
                    tracing::warn!("rollback del {rel}: {err}");
                }
            }
        }
        UnitState::Dir {
            old_moved,
            new_placed,
            removed_empty_root,
        } => {
            let target = ctx.target(rel);
            if *new_placed {
                if let Err(err) = rename_retry(&target, &ctx.new_of(rel), ctx.backoff) {
                    tracing::warn!("rollback dir {rel:?}: move new back failed: {err}");
                }
            }
            if *old_moved {
                if let Err(err) = rename_retry(&ctx.old_of(rel), &target, ctx.backoff) {
                    tracing::warn!("rollback dir {rel:?}: restore old failed: {err}");
                }
            } else if *removed_empty_root {
                let _ = std::fs::create_dir_all(&target);
            }
        }
        UnitState::Degraded(states) => {
            for (f, st) in states.iter().rev() {
                undo_file(ctx, &f.rel, st);
            }
        }
        UnitState::Skipped => {}
    }
}

fn unit_touches_self(ctx: &Ctx, unit: &Unit) -> bool {
    match unit {
        Unit::File(f) | Unit::Copy(f) => ctx.is_self(&ctx.target(&f.rel)),
        Unit::Del { rel, .. } => ctx.is_self(&ctx.target(rel)),
        Unit::Dir { files, .. } => files.iter().any(|f| ctx.is_self(&ctx.target(&f.rel))),
    }
}

/// Re-probe directory units right before the swap: anything the user dropped
/// into a directory judged clean at plan time turns that unit into per-file
/// units.
fn degrade_dirty_dirs(ctx: &Ctx, units: Vec<Unit>) -> Vec<Unit> {
    let mut out = Vec::with_capacity(units.len());
    for unit in units {
        match unit {
            Unit::Dir { rel, files } if !dir_is_clean(&ctx.target(&rel), &files) => {
                tracing::info!("dir unit {rel:?} no longer clean, committing per file");
                out.extend(files.into_iter().map(Unit::File));
            }
            other => out.push(other),
        }
    }
    out
}

fn write_journal(staging: &Staging, journal: &Journal) -> anyhow::Result<()> {
    let path = staging.journal_path();
    let mut file = std::fs::File::create(&path).context("create journal")?;
    std::io::Write::write_all(&mut file, journal.to_text().as_bytes()).context("write journal")?;
    file.sync_all().context("sync journal")?;
    Ok(())
}

fn run_units(
    ctx: &Ctx,
    units: &[Unit],
    states: &mut Vec<UnitState>,
    notify: &ProgressNotify,
    stop_after: Option<usize>,
) -> anyhow::Result<()> {
    let total = units.len() as u64;
    for (i, unit) in units.iter().enumerate().skip(states.len()) {
        if stop_after.is_some_and(|n| i >= n) {
            return Err(anyhow::anyhow!("commit interrupted (test hook)"));
        }
        let st = apply_unit(ctx, unit)?;
        states.push(st);
        notify(Progress::CountOf {
            done: (i + 1) as u64,
            total,
        });
    }
    Ok(())
}

fn rollback_all(ctx: &Ctx, units: &[Unit], states: &[UnitState]) {
    for (unit, st) in units.iter().zip(states.iter()).rev() {
        undo_unit(ctx, unit.rel(), st);
    }
}

fn finish_copies(ctx: &Ctx, units: &[Unit]) {
    for unit in units {
        if let Unit::Copy(f) = unit {
            finish_copy(&ctx.target(&f.rel));
        }
    }
}

fn commit_sync(
    args: CommitArgs,
    notify: ProgressNotify,
    backoff: &[u64],
    stop_after: Option<usize>,
) -> anyhow::Result<CommitOutcome> {
    let staging = Staging::at(&args.staging_root);
    let install = PathBuf::from(&args.install_dir);
    let algo = args.journal.hash_algorithm.clone();
    let ctx = Ctx {
        staging: &staging,
        install: &install,
        algo: &algo,
        current_exe: std::env::current_exe().ok(),
        backoff,
    };
    let units = degrade_dirty_dirs(&ctx, args.journal.units);
    let journal = Journal {
        hash_algorithm: algo.clone(),
        archive: args.journal.archive,
        units,
    };
    write_journal(&staging, &journal)?;
    let mut states = Vec::with_capacity(journal.units.len());
    match run_units(&ctx, &journal.units, &mut states, &notify, stop_after) {
        Ok(()) => {}
        Err(err) => {
            if stop_after.is_some() {
                // test hook: leave the half-committed state and the journal behind
                return Err(err);
            }
            rollback_all(&ctx, &journal.units, &states);
            let _ = std::fs::remove_file(staging.journal_path());
            staging.discard();
            return Err(err);
        }
    }
    finish_copies(&ctx, &journal.units);
    let _ = std::fs::remove_file(staging.journal_path());
    let self_replaced = journal.units.iter().any(|u| unit_touches_self(&ctx, u));
    Ok(CommitOutcome { self_replaced })
}

pub async fn commit(args: CommitArgs, notify: ProgressNotify) -> anyhow::Result<CommitOutcome> {
    tokio::task::spawn_blocking(move || commit_sync(args, notify, BACKOFF_MS, None))
        .await
        .context("commit thread")?
}

#[derive(Debug, PartialEq, Eq)]
enum Status {
    Done,
    Pending,
    /// The target still holds the old content but `new\` lost the replacement:
    /// the swap cannot be finished, only undone.
    Unrecoverable,
    Changed,
}

fn hash_opt(algo: &str, path: &Path) -> Option<String> {
    if path.is_file() {
        hash_file(algo, &path.to_string_lossy()).ok()
    } else {
        None
    }
}

fn classify_file(ctx: &Ctx, f: &FileEntry) -> Status {
    let got = hash_opt(ctx.algo, &ctx.target(&f.rel));
    if got.as_deref() == Some(f.new.as_str()) {
        Status::Done
    } else if got == f.old {
        if ctx.new_of(&f.rel).is_file() {
            Status::Pending
        } else {
            Status::Unrecoverable
        }
    } else {
        Status::Changed
    }
}

fn classify(ctx: &Ctx, unit: &Unit) -> Status {
    match unit {
        Unit::File(f) | Unit::Copy(f) => classify_file(ctx, f),
        Unit::Del { rel, old } => {
            let got = hash_opt(ctx.algo, &ctx.target(rel));
            if got.is_none() {
                Status::Done
            } else if got == *old {
                Status::Pending
            } else {
                Status::Changed
            }
        }
        Unit::Dir { rel, files } => {
            let target = ctx.target(rel);
            let new_set: HashMap<String, String> = files
                .iter()
                .map(|f| (lower(&f.rel), f.new.clone()))
                .collect();
            let old_set: HashMap<String, String> = files
                .iter()
                .filter_map(|f| f.old.clone().map(|h| (lower(&f.rel), h)))
                .collect();
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            let current: Option<HashMap<String, String>> = if target.exists() {
                match list_tree(&target) {
                    Ok(Some(list)) => Some(
                        list.into_iter()
                            .filter_map(|(sub, path)| {
                                let full = lower(&format!("{prefix}{sub}"));
                                hash_opt(ctx.algo, &path).map(|h| (full, h))
                            })
                            .collect(),
                    ),
                    _ => return Status::Changed,
                }
            } else {
                None
            };
            let pending = |has_new: bool| {
                if has_new {
                    Status::Pending
                } else {
                    Status::Unrecoverable
                }
            };
            match current {
                Some(cur) if cur == new_set => Status::Done,
                Some(cur) if cur == old_set => pending(ctx.new_of(rel).is_dir()),
                None if old_set.is_empty() => pending(ctx.new_of(rel).is_dir()),
                _ => Status::Changed,
            }
        }
    }
}

/// State to rebuild for a unit the previous run already swapped, so a failed
/// forward roll can undo it too.
fn done_state(ctx: &Ctx, unit: &Unit) -> UnitState {
    match unit {
        Unit::File(f) => UnitState::File(FileState {
            old_moved: ctx.old_of(&f.rel).exists(),
            new_placed: true,
        }),
        Unit::Copy(f) => UnitState::Copy(FileState {
            old_moved: copy_paths(&ctx.target(&f.rel)).1.exists(),
            new_placed: true,
        }),
        Unit::Del { rel, .. } => UnitState::Del {
            old_moved: ctx.old_of(rel).exists(),
        },
        Unit::Dir { rel, .. } => UnitState::Dir {
            old_moved: ctx.old_of(rel).exists(),
            new_placed: true,
            removed_empty_root: false,
        },
    }
}

fn recover_sync(
    args: CommitArgs,
    notify: ProgressNotify,
    backoff: &[u64],
) -> anyhow::Result<RecoverOutcome> {
    let staging = Staging::at(&args.staging_root);
    let install = PathBuf::from(&args.install_dir);
    let ctx = Ctx {
        staging: &staging,
        install: &install,
        algo: &args.journal.hash_algorithm,
        current_exe: std::env::current_exe().ok(),
        backoff,
    };
    let units = &args.journal.units;
    let statuses: Vec<Status> = units.iter().map(|u| classify(&ctx, u)).collect();
    if statuses.iter().any(|s| *s == Status::Changed) {
        tracing::info!(
            "recovery: directory changed since the journal was written, dropping staging"
        );
        staging.discard();
        return Ok(RecoverOutcome::Discarded);
    }
    if statuses.iter().any(|s| *s == Status::Unrecoverable) {
        tracing::info!("recovery: staged files missing, undoing the swapped units");
        let done: Vec<UnitState> = units
            .iter()
            .zip(statuses.iter())
            .map(|(u, s)| match s {
                Status::Done => done_state(&ctx, u),
                _ => UnitState::Skipped,
            })
            .collect();
        rollback_all(&ctx, units, &done);
        staging.discard();
        return Ok(RecoverOutcome::Discarded);
    }
    let mut states: Vec<UnitState> = Vec::with_capacity(units.len());
    let total = units.len() as u64;
    for (i, (unit, status)) in units.iter().zip(statuses.iter()).enumerate() {
        let st = match status {
            Status::Done => done_state(&ctx, unit),
            Status::Pending => match apply_unit(&ctx, unit) {
                Ok(st) => st,
                Err(err) => {
                    rollback_all(&ctx, units, &states);
                    let _ = std::fs::remove_file(staging.journal_path());
                    staging.discard();
                    return Err(err);
                }
            },
            Status::Changed | Status::Unrecoverable => unreachable!(),
        };
        states.push(st);
        notify(Progress::CountOf {
            done: (i + 1) as u64,
            total,
        });
    }
    finish_copies(&ctx, units);
    let _ = std::fs::remove_file(staging.journal_path());
    let self_replaced = units.iter().any(|u| unit_touches_self(&ctx, u));
    Ok(RecoverOutcome::Completed { self_replaced })
}

pub async fn recover(args: CommitArgs, notify: ProgressNotify) -> anyhow::Result<RecoverOutcome> {
    tokio::task::spawn_blocking(move || recover_sync(args, notify, BACKOFF_MS))
        .await
        .context("recover thread")?
}

/// Whether the journal still describes what this session wants to install:
/// every file unit's new hash equals the wanted hash for that path, every
/// delete is still wanted, and (Mirror酱) the archive digest matches.
/// `self_images` are the installer-generated files (uninstaller, updater)
/// that never appear in the metadata; they pass on their own journal hash.
pub fn journal_matches_target(
    journal: &Journal,
    hash_algorithm: &str,
    wanted: &HashMap<String, String>,
    deletes: &std::collections::HashSet<String>,
    archive: Option<&str>,
    self_images: &std::collections::HashSet<String>,
) -> bool {
    if journal.hash_algorithm != hash_algorithm {
        return false;
    }
    if journal.archive.as_deref() != archive {
        return false;
    }
    if journal.archive.is_some() {
        return true;
    }
    let file_ok = |f: &FileEntry| {
        let rel = lower(&f.rel);
        self_images.contains(&rel) || wanted.get(&rel) == Some(&f.new)
    };
    for unit in &journal.units {
        match unit {
            Unit::File(f) | Unit::Copy(f) => {
                if !file_ok(f) {
                    return false;
                }
            }
            Unit::Dir { files, .. } => {
                if !files.iter().all(file_ok) {
                    return false;
                }
            }
            Unit::Del { rel, .. } => {
                if !deletes.contains(&lower(rel)) {
                    return false;
                }
            }
        }
    }
    true
}

/// Drop a staging directory whose contents are no longer wanted.
pub fn discard(staging_root: &str) {
    remove_tree(Path::new(staging_root));
}

/// Delete `old\` after a successful commit that did not touch the running
/// executable; the rest of the staging directory follows when the session
/// ends.
pub fn drop_old(staging_root: &str) {
    remove_tree(&Staging::at(staging_root).old_dir());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::progress_notify;
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;

    const FAST: &[u64] = &[0, 0];

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachina-commit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn md5(bytes: &[u8]) -> String {
        chksum_md5::hash(bytes).to_hex_lowercase()
    }

    fn read(path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    struct Fixture {
        base: PathBuf,
        install: PathBuf,
        staging: Staging,
    }

    impl Fixture {
        fn new() -> Self {
            let base = tmp();
            let install = base.join("app");
            let staging = Staging::at(base.join("staged"));
            staging.ensure_layout().unwrap();
            Self {
                base,
                install,
                staging,
            }
        }

        fn args(&self, units: Vec<Unit>) -> CommitArgs {
            CommitArgs {
                staging_root: self.staging.root().to_string_lossy().to_string(),
                install_dir: self.install.to_string_lossy().to_string(),
                journal: Journal {
                    hash_algorithm: "md5".into(),
                    archive: None,
                    units,
                },
            }
        }

        fn target(&self, rel: &str) -> PathBuf {
            join_rel(&self.install, rel)
        }

        /// Three files: a.txt changes, b.txt is new, c.txt changes.
        fn three_files(&self) -> Vec<Unit> {
            write(&self.target("a.txt"), b"a-old");
            write(&self.target("c.txt"), b"c-old");
            write(&self.staging.new_path("a.txt"), b"a-new");
            write(&self.staging.new_path("b.txt"), b"b-new");
            write(&self.staging.new_path("c.txt"), b"c-new");
            vec![
                Unit::File(FileEntry {
                    rel: "a.txt".into(),
                    old: Some(md5(b"a-old")),
                    new: md5(b"a-new"),
                }),
                Unit::File(FileEntry {
                    rel: "b.txt".into(),
                    old: None,
                    new: md5(b"b-new"),
                }),
                Unit::File(FileEntry {
                    rel: "c.txt".into(),
                    old: Some(md5(b"c-old")),
                    new: md5(b"c-new"),
                }),
            ]
        }

        fn assert_old(&self) {
            assert_eq!(read(&self.target("a.txt")), Some(b"a-old".to_vec()));
            assert_eq!(read(&self.target("b.txt")), None);
            assert_eq!(read(&self.target("c.txt")), Some(b"c-old".to_vec()));
        }

        fn assert_new(&self) {
            assert_eq!(read(&self.target("a.txt")), Some(b"a-new".to_vec()));
            assert_eq!(read(&self.target("b.txt")), Some(b"b-new".to_vec()));
            assert_eq!(read(&self.target("c.txt")), Some(b"c-new".to_vec()));
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// Open handle without FILE_SHARE_DELETE: reads and hashing still work,
    /// rename / delete fail with a sharing violation.
    fn lock(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(path)
            .unwrap()
    }

    #[test]
    fn journal_roundtrip_and_version_gate() {
        let j = Journal {
            hash_algorithm: "xxh".into(),
            archive: Some("abc".into()),
            units: vec![
                Unit::Dir {
                    rel: "lib".into(),
                    files: vec![FileEntry {
                        rel: "lib/x y.dll".into(),
                        old: None,
                        new: "n1".into(),
                    }],
                },
                Unit::File(FileEntry {
                    rel: "app.exe".into(),
                    old: Some("o".into()),
                    new: "n".into(),
                }),
                Unit::Del {
                    rel: "gone.txt".into(),
                    old: Some("g".into()),
                },
                Unit::Copy(FileEntry {
                    rel: "link/f".into(),
                    old: None,
                    new: "c".into(),
                }),
            ],
        };
        let text = j.to_text();
        assert!(text.starts_with("kachina-journal 1\nhash\txxh\narchive\tabc\n"));
        assert_eq!(Journal::parse(&text), Some(j.clone()));
        assert!(Journal::parse("kachina-journal 0\nhash\tmd5\n").is_none());
        assert!(Journal::parse("").is_none());
        assert!(Journal::parse("kachina-journal 1\nbogus\tline\n").is_none());
    }

    #[test]
    fn commit_swaps_three_files_and_removes_journal() {
        let fx = Fixture::new();
        let units = fx.three_files();
        let out = commit_sync(fx.args(units), progress_notify(|_| {}), FAST, None).unwrap();
        assert!(!out.self_replaced);
        fx.assert_new();
        assert!(!fx.staging.journal_path().exists());
        assert_eq!(read(&fx.staging.old_path("a.txt")), Some(b"a-old".to_vec()));
    }

    #[test]
    fn commit_rolls_back_when_second_unit_is_locked() {
        let fx = Fixture::new();
        let units = fx.three_files();
        write(&fx.target("b.txt"), b"b-old");
        let mut units = units;
        if let Unit::File(f) = &mut units[1] {
            f.old = Some(md5(b"b-old"));
        }
        let _hold = lock(&fx.target("b.txt"));
        let err = commit_sync(fx.args(units), progress_notify(|_| {}), FAST, None).unwrap_err();
        assert!(matches!(
            crate::utils::code::extract(&err),
            crate::utils::code::Extracted::Coded(c) if c.code == crate::utils::code::FILE_IN_USE
                && c.subject.as_deref() == Some("b.txt")
        ));
        assert_eq!(read(&fx.target("a.txt")), Some(b"a-old".to_vec()));
        assert_eq!(read(&fx.target("b.txt")), Some(b"b-old".to_vec()));
        assert_eq!(read(&fx.target("c.txt")), Some(b"c-old".to_vec()));
        assert!(!fx.staging.root().exists());
    }

    #[test]
    fn interrupted_commit_recovers_forward() {
        let fx = Fixture::new();
        let units = fx.three_files();
        let args = fx.args(units);
        let err = commit_sync(args.clone(), progress_notify(|_| {}), FAST, Some(2)).unwrap_err();
        assert!(err.to_string().contains("interrupted"));
        assert!(fx.staging.journal_path().exists());
        assert_eq!(read(&fx.target("a.txt")), Some(b"a-new".to_vec()));
        assert_eq!(read(&fx.target("c.txt")), Some(b"c-old".to_vec()));

        let journal =
            Journal::parse(&std::fs::read_to_string(fx.staging.journal_path()).unwrap()).unwrap();
        let out = recover_sync(
            CommitArgs {
                journal,
                ..args.clone()
            },
            progress_notify(|_| {}),
            FAST,
        )
        .unwrap();
        assert_eq!(
            out,
            RecoverOutcome::Completed {
                self_replaced: false
            }
        );
        fx.assert_new();
        assert!(!fx.staging.journal_path().exists());
    }

    #[test]
    fn recovery_discards_when_target_was_overwritten() {
        let fx = Fixture::new();
        let units = fx.three_files();
        let args = fx.args(units);
        let _ = commit_sync(args.clone(), progress_notify(|_| {}), FAST, Some(2));
        write(&fx.target("c.txt"), b"portable-build");
        let journal =
            Journal::parse(&std::fs::read_to_string(fx.staging.journal_path()).unwrap()).unwrap();
        let out = recover_sync(
            CommitArgs { journal, ..args },
            progress_notify(|_| {}),
            FAST,
        )
        .unwrap();
        assert_eq!(out, RecoverOutcome::Discarded);
        assert_eq!(read(&fx.target("c.txt")), Some(b"portable-build".to_vec()));
        assert_eq!(
            read(&fx.target("a.txt")),
            Some(b"a-new".to_vec()),
            "already swapped unit left alone"
        );
        assert!(!fx.staging.root().exists());
    }

    #[test]
    fn recovery_discards_when_swapped_unit_was_modified() {
        let fx = Fixture::new();
        let units = fx.three_files();
        let args = fx.args(units);
        let _ = commit_sync(args.clone(), progress_notify(|_| {}), FAST, Some(2));
        write(&fx.target("a.txt"), b"user-edit");
        let journal =
            Journal::parse(&std::fs::read_to_string(fx.staging.journal_path()).unwrap()).unwrap();
        let out = recover_sync(
            CommitArgs { journal, ..args },
            progress_notify(|_| {}),
            FAST,
        )
        .unwrap();
        assert_eq!(out, RecoverOutcome::Discarded);
        assert!(!fx.staging.root().exists());
    }

    #[test]
    fn recovery_failure_rolls_back_previously_swapped_units() {
        let fx = Fixture::new();
        let units = fx.three_files();
        let args = fx.args(units);
        let _ = commit_sync(args.clone(), progress_notify(|_| {}), FAST, Some(2));
        let _hold = lock(&fx.target("c.txt"));
        let journal =
            Journal::parse(&std::fs::read_to_string(fx.staging.journal_path()).unwrap()).unwrap();
        let err = recover_sync(
            CommitArgs { journal, ..args },
            progress_notify(|_| {}),
            FAST,
        )
        .unwrap_err();
        assert!(matches!(
            crate::utils::code::extract(&err),
            crate::utils::code::Extracted::Coded(c) if c.code == crate::utils::code::FILE_IN_USE
        ));
        drop(_hold);
        fx.assert_old();
        assert!(!fx.staging.root().exists());
    }

    #[test]
    fn recovery_with_new_dir_deleted_rolls_back_swapped_units() {
        let fx = Fixture::new();
        let units = fx.three_files();
        let args = fx.args(units);
        let _ = commit_sync(args.clone(), progress_notify(|_| {}), FAST, Some(2));
        std::fs::remove_dir_all(fx.staging.new_dir()).unwrap();
        let journal =
            Journal::parse(&std::fs::read_to_string(fx.staging.journal_path()).unwrap()).unwrap();
        let out = recover_sync(
            CommitArgs { journal, ..args },
            progress_notify(|_| {}),
            FAST,
        )
        .unwrap();
        // c.txt still holds the old bytes but its replacement is gone: the two
        // swapped units are undone and the staging goes away
        assert_eq!(out, RecoverOutcome::Discarded);
        fx.assert_old();
        assert!(!fx.staging.root().exists());
    }

    #[test]
    fn delete_unit_moves_to_old_and_rolls_back() {
        let fx = Fixture::new();
        write(&fx.target("gone.txt"), b"bye");
        let units = vec![
            Unit::Del {
                rel: "gone.txt".into(),
                old: Some(md5(b"bye")),
            },
            Unit::Del {
                rel: "never.txt".into(),
                old: None,
            },
        ];
        commit_sync(fx.args(units.clone()), progress_notify(|_| {}), FAST, None).unwrap();
        assert!(!fx.target("gone.txt").exists());
        assert_eq!(
            read(&fx.staging.old_path("gone.txt")),
            Some(b"bye".to_vec())
        );

        // rollback path: a locked file after the delete forces undo
        let fx2 = Fixture::new();
        write(&fx2.target("gone.txt"), b"bye");
        write(&fx2.target("locked.txt"), b"l");
        write(&fx2.staging.new_path("locked.txt"), b"l2");
        let _hold = lock(&fx2.target("locked.txt"));
        let units = vec![
            Unit::Del {
                rel: "gone.txt".into(),
                old: Some(md5(b"bye")),
            },
            Unit::File(FileEntry {
                rel: "locked.txt".into(),
                old: Some(md5(b"l")),
                new: md5(b"l2"),
            }),
        ];
        assert!(commit_sync(fx2.args(units), progress_notify(|_| {}), FAST, None).is_err());
        assert_eq!(read(&fx2.target("gone.txt")), Some(b"bye".to_vec()));
    }

    #[test]
    fn dir_unit_degrades_when_a_file_is_locked() {
        let fx = Fixture::new();
        write(&fx.target("lib/a.dll"), b"a1");
        write(&fx.target("lib/b.dll"), b"b1");
        write(&fx.staging.new_path("lib/a.dll"), b"a2");
        write(&fx.staging.new_path("lib/b.dll"), b"b2");
        let files = vec![
            FileEntry {
                rel: "lib/a.dll".into(),
                old: Some(md5(b"a1")),
                new: md5(b"a2"),
            },
            FileEntry {
                rel: "lib/b.dll".into(),
                old: Some(md5(b"b1")),
                new: md5(b"b2"),
            },
        ];
        // a read handle without FILE_SHARE_DELETE blocks renaming the parent dir
        // but not the sibling file
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(fx.target("lib/a.dll"))
            .unwrap();
        let err = commit_sync(
            fx.args(vec![Unit::Dir {
                rel: "lib".into(),
                files: files.clone(),
            }]),
            progress_notify(|_| {}),
            FAST,
            None,
        );
        drop(hold);
        // degraded: b.dll swapped, a.dll locked → whole thing rolled back
        assert!(err.is_err());
        assert_eq!(read(&fx.target("lib/a.dll")), Some(b"a1".to_vec()));
        assert_eq!(read(&fx.target("lib/b.dll")), Some(b"b1".to_vec()));

        // without the lock the directory swaps as one unit
        let fx = Fixture::new();
        write(&fx.target("lib/a.dll"), b"a1");
        write(&fx.target("lib/b.dll"), b"b1");
        write(&fx.staging.new_path("lib/a.dll"), b"a2");
        write(&fx.staging.new_path("lib/b.dll"), b"b2");
        commit_sync(
            fx.args(vec![Unit::Dir {
                rel: "lib".into(),
                files,
            }]),
            progress_notify(|_| {}),
            FAST,
            None,
        )
        .unwrap();
        assert_eq!(read(&fx.target("lib/a.dll")), Some(b"a2".to_vec()));
        assert_eq!(
            read(&fx.staging.old_path("lib/a.dll")),
            Some(b"a1".to_vec())
        );
    }

    #[test]
    fn dir_unit_degrades_when_no_longer_clean() {
        let fx = Fixture::new();
        write(&fx.staging.new_path("lib/a.dll"), b"a2");
        write(&fx.target("lib/user.txt"), b"mine");
        let out = commit_sync(
            fx.args(vec![Unit::Dir {
                rel: "lib".into(),
                files: vec![FileEntry {
                    rel: "lib/a.dll".into(),
                    old: None,
                    new: md5(b"a2"),
                }],
            }]),
            progress_notify(|_| {}),
            FAST,
            None,
        )
        .unwrap();
        assert!(!out.self_replaced);
        assert_eq!(read(&fx.target("lib/a.dll")), Some(b"a2".to_vec()));
        assert_eq!(read(&fx.target("lib/user.txt")), Some(b"mine".to_vec()));
    }

    #[test]
    fn root_unit_missing_and_empty_install_dir() {
        let fx = Fixture::new();
        write(&fx.staging.new_path("app.exe"), b"exe");
        write(&fx.staging.new_path("lib/a.dll"), b"a");
        let files = vec![
            FileEntry {
                rel: "app.exe".into(),
                old: None,
                new: md5(b"exe"),
            },
            FileEntry {
                rel: "lib/a.dll".into(),
                old: None,
                new: md5(b"a"),
            },
        ];
        assert!(!fx.install.exists());
        commit_sync(
            fx.args(vec![Unit::Dir {
                rel: String::new(),
                files: files.clone(),
            }]),
            progress_notify(|_| {}),
            FAST,
            None,
        )
        .unwrap();
        assert_eq!(read(&fx.target("lib/a.dll")), Some(b"a".to_vec()));
        assert!(!fx.staging.new_dir().exists());

        let fx = Fixture::new();
        std::fs::create_dir_all(&fx.install).unwrap();
        write(&fx.staging.new_path("app.exe"), b"exe");
        write(&fx.staging.new_path("lib/a.dll"), b"a");
        commit_sync(
            fx.args(vec![Unit::Dir {
                rel: String::new(),
                files,
            }]),
            progress_notify(|_| {}),
            FAST,
            None,
        )
        .unwrap();
        assert_eq!(read(&fx.target("app.exe")), Some(b"exe".to_vec()));
    }

    #[test]
    fn copy_unit_via_junction() {
        let fx = Fixture::new();
        let real = fx.base.join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        write(&real.join("f.txt"), b"old");
        std::fs::create_dir_all(&fx.install).unwrap();
        let link = fx.target("link");
        let status = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &real.to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink: {:?}", status);
        write(&fx.staging.new_path("link/f.txt"), b"new");
        let unit = Unit::Copy(FileEntry {
            rel: "link/f.txt".into(),
            old: Some(md5(b"old")),
            new: md5(b"new"),
        });
        commit_sync(
            fx.args(vec![unit.clone()]),
            progress_notify(|_| {}),
            FAST,
            None,
        )
        .unwrap();
        assert_eq!(read(&real.join("f.txt")), Some(b"new".to_vec()));
        assert!(!real.join("f.txt.kachina-tmp").exists());
        assert!(!real.join("f.txt.kachina-old").exists());

        // second rename failure → old content restored
        write(&real.join("f.txt"), b"old");
        write(&fx.staging.new_path("link/f.txt"), b"new");
        write(&fx.staging.new_path("x.txt"), b"x");
        write(&fx.target("x.txt"), b"x0");
        let _hold = lock(&fx.target("x.txt"));
        let err = commit_sync(
            fx.args(vec![
                unit,
                Unit::File(FileEntry {
                    rel: "x.txt".into(),
                    old: Some(md5(b"x0")),
                    new: md5(b"x"),
                }),
            ]),
            progress_notify(|_| {}),
            FAST,
            None,
        );
        assert!(err.is_err());
        assert_eq!(read(&real.join("f.txt")), Some(b"old".to_vec()));
        let _ = std::fs::remove_dir(&link);
    }

    #[test]
    fn journal_target_check() {
        let j = Journal {
            hash_algorithm: "md5".into(),
            archive: None,
            units: vec![
                Unit::File(FileEntry {
                    rel: "A.txt".into(),
                    old: None,
                    new: "1".into(),
                }),
                Unit::Del {
                    rel: "gone".into(),
                    old: None,
                },
            ],
        };
        let none = std::collections::HashSet::new();
        let mut wanted = HashMap::new();
        wanted.insert("a.txt".to_string(), "1".to_string());
        let mut deletes = std::collections::HashSet::new();
        deletes.insert("gone".to_string());
        assert!(journal_matches_target(
            &j, "md5", &wanted, &deletes, None, &none
        ));
        assert!(!journal_matches_target(
            &j, "xxh", &wanted, &deletes, None, &none
        ));
        wanted.insert("a.txt".to_string(), "2".to_string());
        assert!(!journal_matches_target(
            &j, "md5", &wanted, &deletes, None, &none
        ));
        // an installer-generated file passes on its own hash
        let mut selfs = std::collections::HashSet::new();
        selfs.insert("a.txt".to_string());
        assert!(journal_matches_target(
            &j, "md5", &wanted, &deletes, None, &selfs
        ));
        wanted.insert("a.txt".to_string(), "1".to_string());
        deletes.clear();
        assert!(!journal_matches_target(
            &j, "md5", &wanted, &deletes, None, &none
        ));

        let m = Journal {
            hash_algorithm: "md5".into(),
            archive: Some("zip1".into()),
            units: vec![],
        };
        assert!(journal_matches_target(
            &m,
            "md5",
            &HashMap::new(),
            &Default::default(),
            Some("zip1"),
            &none
        ));
        assert!(!journal_matches_target(
            &m,
            "md5",
            &HashMap::new(),
            &Default::default(),
            Some("zip2"),
            &none
        ));
    }
}
