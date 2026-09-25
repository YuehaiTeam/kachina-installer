use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::bail;
use futures::future::join_all;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::dfs::InsightItem;
use crate::fs::commit::{
    journal_matches_target, CommitArgs, FileEntry, Journal, RecoverOutcome, Unit,
};
use crate::fs::staging::Staging;
use crate::fs::LocalScan;
use crate::installer::config::InstallerConfig;
use crate::installer::lnk::get_dirs;
use crate::installer::lnk::CreateLnkArgs;
use crate::installer::registry::WriteRegistryParams;
use crate::installer::uninstall::{remove_paths, RunUninstallArgs};
use crate::ipc::install_file::{
    InstallFileArgs, InstallFileMode, InstallFileSource, InstallMultiStreamArgs,
};
use crate::ipc::manager::ManagedElevate;
use crate::ipc::operation::IpcOperation;
use crate::ipc::{progress_noop, progress_notify, IpcResult, Progress, ProgressNotify};
use crate::local::Embedded;
use crate::session::commands::SessionState;
use crate::session::dump::session_dump;
use crate::session::merge::{dfs2_ranges, file_mode, plan_tasks, FileMode, FilePos, InstallTask};
use crate::session::plan::{
    build_plan, collect_skip_hash, files_to_probe_writable, find_local, is_under, join_install,
    mark_unwritable, normalize_rel, strip_install_prefix, HashKey, InstallPlan, LocalFile,
    PlanAction, PlanInput, SkipReason,
};
use crate::session::source::{
    cleanup_dfs2, ensure_dfs2_session, fetch_metadata, hash_of_item, needs_js_plugin, parse_source,
    resolve_file_location, resolve_range_urls, FileLocation, ParsedSource, SourceCtx,
};
use crate::session::state::{
    ByteProgress, CancelState, FileAction, FileProgress, Phase, Progress as UiProgress,
    ProgressCounter, ProgressStage, ProgressUnit, Prompt, UiState,
};
use crate::session::types::{version_gt, ProjectConfig, SessionResult, Settings, SourceField};
use crate::session::ui::{send_ev_insight, SessionUi, SilentPluginUi};
use crate::thirdparty::mirrorc::get_mirrorc_status;
use crate::utils::code::{
    attach_download, attach_download_or, attach_metadata, coded_for_mirrorc_response,
    coded_from_error, extract, fail_kind, tag_session, Attach, Cancelled, Coded, Extracted,
    DISK_FULL, ELEVATED_DRIVE_UNAVAILABLE, FILE_IO_FAILED, HASH_ALGORITHM_UNSUPPORTED,
    METADATA_UNREACHABLE, MIRRORC_CDK_MISSING, MIRRORC_CONFIG_INVALID, MIRRORC_FAILED,
    MIRRORC_UNREACHABLE, NO_DOWNLOAD_NODE, PKG_BROKEN, PROCESS_KILL_FAILED, REGISTRY_WRITE_FAILED,
    RUNTIME_INSTALL_FAILED, SHORTCUT_FAILED, UNINSTALL_INCOMPLETE, UNINSTALL_INFO_MISSING,
    WEBVIEW2_REQUIRED,
};
use crate::utils::error::IntoAnyhow;
use crate::utils::metadata::{FileMeta, RepoMetadata};
use tokio_util::sync::CancellationToken;

pub async fn run_op(
    mgr: &ManagedElevate,
    elevate: bool,
    op: IpcOperation,
    on_progress: ProgressNotify,
) -> anyhow::Result<IpcResult> {
    mgr.run(op, elevate, on_progress).await.into_anyhow()
}

struct OperationProgress {
    network_pending: bool,
    network_bytes: u64,
    processing_bytes: u64,
    finished: bool,
    network_rate: super::rate::Rate,
    processing_rate: super::rate::Rate,
}
impl OperationProgress {
    fn new(network_pending: bool) -> Self {
        Self {
            network_pending,
            network_bytes: 0,
            processing_bytes: 0,
            finished: false,
            network_rate: super::rate::Rate::new(Instant::now()),
            processing_rate: super::rate::Rate::new(Instant::now()),
        }
    }
    fn observe(&mut self, p: &Progress) -> bool {
        if self.finished {
            return false;
        }
        match p {
            Progress::Network(snapshot) | Progress::NetworkFinal(snapshot) => {
                self.network_bytes = self.network_bytes.max(snapshot.bytes);
                if snapshot.bytes > 0 || snapshot.active > 0 {
                    self.network_pending = snapshot.active > 0;
                }
                if matches!(p, Progress::NetworkFinal(_)) {
                    self.network_pending = false;
                    self.finished = true;
                }
            }
            Progress::BytesOf { done, .. } => {
                self.processing_bytes = self.processing_bytes.max(*done)
            }
            Progress::Stage(_) => self.network_pending = false,
            _ => {}
        }
        true
    }
    fn publish(&mut self, ui: &LiveUi<'_>) {
        let show = {
            let state = ui.live.lock().unwrap();
            match &state.phase {
                Phase::Running(p) => {
                    let rates = self.network_pending || p.stage.unit() == ProgressUnit::Bytes;
                    let cancel = p.cancel == CancelState::Available && ui.cancel.is_cancelled();
                    rates || cancel
                }
                _ => false,
            }
        };
        if !show {
            return;
        }
        {
            let mut state = ui.live.lock().unwrap();
            if let Phase::Running(p) = &mut state.phase {
                p.network_pending = self.network_pending;
                let network = self.network_rate.sample(Instant::now(), self.network_bytes);
                p.network_bps = if p.network_pending { network } else { None };
                let processing = self
                    .processing_rate
                    .sample(Instant::now(), self.processing_bytes);
                p.processing_bps = if p.stage.unit() == ProgressUnit::Bytes {
                    processing
                } else {
                    None
                };
            }
        }
        ui.emit_live();
    }
}

async fn run_op_with_ui(
    mgr: &ManagedElevate,
    elevate: bool,
    mut op: IpcOperation,
    ui: &LiveUi<'_>,
    mut on_ui: impl FnMut(&LiveUi<'_>, &Progress),
) -> anyhow::Result<IpcResult> {
    let mut stats = OperationProgress::new(matches!(
        &op,
        IpcOperation::RunMirrorcDownload { .. } | IpcOperation::InstallRuntime { offset: None, .. }
    ));
    let session = if let IpcOperation::RunMirrorcDownload { zip_path, .. } = &op {
        let session = uuid::Uuid::new_v4().to_string();
        run_op(
            mgr,
            elevate,
            IpcOperation::BeginDownload(crate::ipc::download::Config {
                id: session.clone(),
                concurrency: 1,
                prefetch_bytes: 0,
                dl_dir: std::path::Path::new(zip_path)
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            }),
            progress_noop(),
        )
        .await?;
        op = IpcOperation::Download(
            crate::ipc::download::Job {
                session: session.clone(),
                large: true,
            },
            Box::new(op),
        );
        Some(session)
    } else {
        None
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();
    let mut op_fut = Box::pin(run_op(
        mgr,
        elevate,
        op,
        progress_notify(move |p| {
            let _ = tx.send(p);
        }),
    ));
    let mut cancelled = false;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    let result = loop {
        tokio::select! {
            Some(p) = rx.recv() => { if stats.observe(&p) { on_ui(ui, &p); } },
            _ = ui.cancel.cancelled(), if session.is_some() && !cancelled => {
                cancelled = true;
                // 返回这个错误会跳过 EndDownload，会话留在 SESSIONS。
                cancel_download(mgr, elevate, &session.clone().unwrap()).await;
            }
            _ = interval.tick() => {
                stats.publish(ui);
            }
            result = &mut op_fut => {
                while let Ok(p) = rx.try_recv() { if stats.observe(&p) { on_ui(ui, &p); } }
                break result;
            }
        }
    };
    if let Some(session) = session {
        run_op(
            mgr,
            elevate,
            IpcOperation::EndDownload(session),
            progress_noop(),
        )
        .await?;
        ui.check_cancel()?;
    }
    result
}

async fn cancel_download(mgr: &ManagedElevate, elevate: bool, session: &str) {
    if let Err(err) = run_op(
        mgr,
        elevate,
        IpcOperation::CancelDownload(session.to_string()),
        progress_noop(),
    )
    .await
    {
        tracing::warn!("cancel download session {session} failed: {err:#}");
    }
}

fn runtime_name(tag: &str) -> &str {
    if tag.starts_with("Microsoft.DotNet") {
        "Microsoft .NET Runtime"
    } else {
        tag
    }
}

async fn run_download_op(
    mgr: &ManagedElevate,
    elevate: bool,
    op: IpcOperation,
    ctx: &SourceCtx,
    mode: Option<&str>,
    on_progress: ProgressNotify,
) -> anyhow::Result<(IpcResult, Option<InsightItem>)> {
    let insights = ctx.insight_sink();
    let mode_owned = mode.map(str::to_owned);
    let on_progress = progress_notify(move |p| {
        if let Progress::Insight(mut item) = p {
            if crate::dfs::is_remote_insight_url(&item.url) {
                item.mode = mode_owned.clone();
                insights.lock().unwrap().push(item);
            }
        } else {
            on_progress(p);
        }
    });
    match mgr.run(op, elevate, on_progress).await {
        Ok(result) => {
            let insight = result.insight();
            collect_insight(ctx, insight.clone(), mode);
            Ok((result, insight))
        }
        Err(ta) => {
            collect_insight(ctx, ta.insight, mode);
            Err(ta.error)
        }
    }
}

fn collect_insight(ctx: &SourceCtx, insight: Option<InsightItem>, mode: Option<&str>) {
    let Some(insight) = insight else {
        return;
    };
    if !crate::dfs::is_remote_insight_url(&insight.url) {
        return;
    }
    ctx.add_insight(insight, mode);
}

fn mode_from_op(op: &IpcOperation) -> Option<&'static str> {
    match op {
        IpcOperation::InstallFile(args) => match &args.mode {
            InstallFileMode::HybridPatch { .. } => Some("hybridpatch"),
            InstallFileMode::Patch { .. } => Some("patch"),
            InstallFileMode::Direct(
                InstallFileSource::Url { .. } | InstallFileSource::Sliced { .. },
            ) => Some("direct"),
            _ => None,
        },
        _ => None,
    }
}

fn merged_mode(
    files: &[FilePos],
    local: &[Embedded],
    patches: &[crate::utils::metadata::PatchInfo],
    hash_key: HashKey,
) -> &'static str {
    let mut direct = false;
    let mut patch = false;
    for file in files {
        match file_mode(&file.item, hash_key, local, patches, false) {
            FileMode::Patch => patch = true,
            FileMode::Direct => direct = true,
            _ => {}
        }
    }
    match (direct, patch) {
        (true, true) => "merged-direct-patch",
        (false, true) => "merged-patch",
        _ => "merged-direct",
    }
}

/// The session's copy of `UiState` while it runs: the caller's snapshot with
/// `phase` replaced on every progress step, pushed whole to the renderer.
struct LiveUi<'a> {
    inner: &'a dyn SessionUi,
    live: Mutex<UiState>,
    cancel: CancellationToken,
}

impl<'a> LiveUi<'a> {
    fn new(inner: &'a dyn SessionUi, base: &UiState) -> Self {
        Self {
            inner,
            live: Mutex::new(base.clone()),
            cancel: inner.cancel_token(),
        }
    }

    /// Emit `live` after releasing its lock.
    fn emit_live(&self) {
        let snap = {
            let mut state = self.live.lock().unwrap_or_else(|e| e.into_inner());
            if let Phase::Running(p) = &mut state.phase {
                p.apply_user_cancel(self.cancel.is_cancelled());
            }
            state.clone()
        };
        self.inner.state(&snap);
    }

    /// Phase-one checkpoint: `Err(Cancelled)` once the user asked to stop.
    fn check_cancel(&self) -> anyhow::Result<()> {
        if self.cancel.is_cancelled() {
            Err(anyhow::Error::new(Cancelled))
        } else {
            Ok(())
        }
    }
}

fn ensure_helper_sees_path(settings: &Settings, mgr: &ManagedElevate) -> anyhow::Result<()> {
    if mgr.uses_helper(settings.elevate)
        && crate::utils::dir::on_session_drive(&settings.install_path)
    {
        return Err(Coded::bare_with(ELEVATED_DRIVE_UNAVAILABLE, &settings.install_path).into());
    }
    Ok(())
}

fn is_cancelled(err: &anyhow::Error) -> bool {
    matches!(extract(err), Extracted::Cancelled)
}

/// Staging directory of the running session, opened on the side that has
/// write access (elevated when the install needs it).
struct SessionStaging {
    staging: Staging,
    elevate: bool,
}

impl SessionStaging {
    fn root(&self) -> String {
        self.staging.root().to_string_lossy().to_string()
    }

    async fn discard(&self, mgr: &ManagedElevate) {
        mgr.wait_idle().await;
        let _ = run_op(
            mgr,
            self.elevate,
            IpcOperation::DiscardStaging(self.root()),
            progress_noop(),
        )
        .await;
    }
}

async fn open_staging(
    settings: &Settings,
    mgr: &ManagedElevate,
) -> anyhow::Result<(SessionStaging, Option<String>)> {
    let raw = run_op(
        mgr,
        settings.elevate,
        IpcOperation::OpenStaging(settings.install_path.clone()),
        progress_noop(),
    )
    .await?;
    let IpcResult::OpenStaging(opened) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    Ok((
        SessionStaging {
            staging: Staging::at(opened.root),
            elevate: settings.elevate,
        },
        opened.journal,
    ))
}

/// What to do with a journal left by an interrupted commit. Returns the
/// staging to continue with (fresh when the journal was dropped) and whether
/// the forward roll swapped the running executable.
#[allow(clippy::too_many_arguments)]
async fn recover_or_discard(
    settings: &Settings,
    project: &ProjectConfig,
    staged: SessionStaging,
    journal_text: String,
    hash_algorithm: &str,
    wanted: &std::collections::HashMap<String, String>,
    deletes: &std::collections::HashSet<String>,
    archive: Option<&str>,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<(SessionStaging, bool)> {
    let reopen = |staged: SessionStaging| async move {
        staged.discard(mgr).await;
        let (fresh, _) = open_staging(settings, mgr).await?;
        Ok::<_, anyhow::Error>((fresh, false))
    };
    let Some(journal) = Journal::parse(&journal_text) else {
        tracing::info!("staging journal unreadable or wrong version, dropping");
        return reopen(staged).await;
    };
    let self_images: std::collections::HashSet<String> =
        [&project.uninstall_name, &project.updater_name]
            .into_iter()
            .map(|n| normalize_rel(n))
            .collect();
    if !journal_matches_target(
        &journal,
        hash_algorithm,
        wanted,
        deletes,
        archive,
        &self_images,
    ) {
        tracing::info!("staging journal is for different content, dropping");
        return reopen(staged).await;
    }
    tracing::info!(
        "recovering interrupted commit ({} units)",
        journal.units.len()
    );
    if !ui.inner.begin_commit() {
        return Err(crate::utils::code::Cancelled.into());
    }
    progress(ui, 2, 95.0, ProgressStage::Commit, None, None, None);
    let args = CommitArgs {
        staging_root: staged.root(),
        install_dir: settings.install_path.clone(),
        journal,
    };
    let raw = run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::Recover(args),
        ui,
        |ui, p| {
            if let Progress::CountOf { done, total } = p {
                progress(
                    ui,
                    2,
                    95.0,
                    ProgressStage::Commit,
                    None,
                    Some(*done),
                    Some(*total),
                );
            }
        },
    )
    .await?;
    let IpcResult::Recover(outcome) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    match outcome {
        RecoverOutcome::Completed { self_replaced } => {
            // journal is gone; the directory stays for this session's own writes
            Ok((staged, self_replaced))
        }
        RecoverOutcome::Discarded => {
            let (fresh, _) = open_staging(settings, mgr).await?;
            Ok((fresh, false))
        }
    }
}

/// Whether the staging volume can hold this session's produced files.
fn ensure_space(staged: &SessionStaging, needed: u64) -> anyhow::Result<()> {
    if let Some(free) = crate::fs::staging::free_space(staged.staging.root()) {
        if free < needed {
            return Err(anyhow::anyhow!("need {needed} bytes, {free} free")
                .attach_with(DISK_FULL, staged.root()));
        }
    }
    Ok(())
}

fn rel_of(file_name: &str) -> String {
    file_name
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string()
}

fn dir_of(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

fn under_dir(rel_lower: &str, dir_lower: &str) -> bool {
    dir_lower.is_empty() || rel_lower.starts_with(&format!("{dir_lower}/"))
}

/// Turn the plan into commit units: directory units where every managed file
/// under a clean directory is being written, copy units under reparse points,
/// file units otherwise, delete units for the plan's deletes.
fn build_units(
    plan: &InstallPlan,
    hashed: &[crate::utils::metadata::FileMeta],
    hash_key: HashKey,
    local: &[LocalFile],
    scan: &LocalScan,
) -> Vec<Unit> {
    let dirty: std::collections::HashSet<&str> =
        scan.dirty_dirs.iter().map(String::as_str).collect();
    let installing: std::collections::HashMap<String, FileEntry> = plan
        .files
        .iter()
        .filter(|f| f.action == PlanAction::Install)
        .filter_map(|f| {
            let meta = hashed
                .iter()
                .find(|h| normalize_rel(&h.file_name) == normalize_rel(&f.file_name))?;
            let new = hash_of_item(meta, hash_key)?;
            Some((
                normalize_rel(&f.file_name),
                FileEntry {
                    rel: rel_of(&f.file_name),
                    old: f.old_hash.clone(),
                    new,
                },
            ))
        })
        .collect();
    let is_copy = |rel_lower: &str| {
        scan.reparse_dirs
            .iter()
            .any(|d| rel_lower.starts_with(&format!("{d}/")))
    };

    // candidate directories: every managed file under them is being written
    let mut all_dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for h in hashed {
        let rel = normalize_rel(&h.file_name);
        let mut d = dir_of(&rel);
        loop {
            all_dirs.insert(d.clone());
            if d.is_empty() {
                break;
            }
            d = dir_of(&d);
        }
    }
    let mut candidates: Vec<String> = all_dirs
        .into_iter()
        .filter(|d| !dirty.contains(d.as_str()))
        .filter(|d| {
            let mut any = false;
            for h in hashed {
                let rel = normalize_rel(&h.file_name);
                if !under_dir(&rel, d) {
                    continue;
                }
                any = true;
                if !installing.contains_key(&rel) || is_copy(&rel) {
                    return false;
                }
            }
            any
        })
        .collect();
    // topmost wins
    candidates.sort_by_key(|d| d.matches('/').count() + usize::from(!d.is_empty()));
    let mut chosen: Vec<String> = Vec::new();
    for d in candidates {
        if !chosen.iter().any(|c| under_dir(&d, c) || c == &d) {
            chosen.push(d);
        }
    }

    let mut units = Vec::new();
    let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dir in &chosen {
        let mut files: Vec<FileEntry> = installing
            .iter()
            .filter(|(rel, _)| under_dir(rel, dir))
            .map(|(rel, e)| {
                covered.insert(rel.clone());
                e.clone()
            })
            .collect();
        files.sort_by(|a, b| a.rel.cmp(&b.rel));
        units.push(Unit::Dir {
            rel: dir.clone(),
            files,
        });
    }
    let mut rest: Vec<(&String, &FileEntry)> = installing
        .iter()
        .filter(|(rel, _)| !covered.contains(*rel))
        .collect();
    rest.sort_by(|a, b| a.0.cmp(b.0));
    for (rel, entry) in rest {
        if is_copy(rel) {
            units.push(Unit::Copy(entry.clone()));
        } else {
            units.push(Unit::File(entry.clone()));
        }
    }
    for del in &plan.deletes {
        let rel = normalize_rel(del);
        if chosen.iter().any(|c| under_dir(&rel, c)) {
            continue;
        }
        units.push(Unit::Del {
            rel: rel_of(del),
            old: find_local(local, del).map(|l| l.hash.clone()),
        });
    }
    units
}

/// Where this session gets its uninstaller / updater from (see the file
/// commit note, "卸载器与更新器"). `list_has_updater`: the metadata (or the
/// Mirror酱 archive) ships the updater itself; `updater_staged`: that copy is
/// being written this session and sits under `new\`.
struct SelfImagePlan {
    names: Vec<String>,
    copy_from: Option<String>,
}

fn self_image_plan(
    settings: &Settings,
    project: &ProjectConfig,
    staging: &Staging,
    list_has_updater: bool,
    updater_staged: bool,
) -> Option<SelfImagePlan> {
    let uninstaller_path = join_install(&settings.install_path, &project.uninstall_name);
    let updater_path = join_install(&settings.install_path, &project.updater_name);
    let uninstaller_exists = std::path::Path::new(&uninstaller_path).is_file();
    let mut names = Vec::new();
    let mut copy_from = None;
    if !settings.is_update {
        names.push(project.uninstall_name.clone());
        names.push(project.updater_name.clone());
    } else if list_has_updater {
        // the shipped updater is the freshest image around; refresh the
        // uninstaller from it, never generate our own updater
        if uninstaller_exists {
            names.push(project.uninstall_name.clone());
            copy_from = Some(if updater_staged {
                staged_target(staging, &project.updater_name)
            } else {
                updater_path
            });
        }
    } else if is_current_exe(&updater_path) {
        // running as the installed updater: nothing newer than ourselves exists
        if uninstaller_exists {
            names.push(project.uninstall_name.clone());
        }
    } else {
        // a foreign installer (online stub / packed): its image is the updater
        names.push(project.updater_name.clone());
        if uninstaller_exists {
            names.push(project.uninstall_name.clone());
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(SelfImagePlan { names, copy_from })
    }
}

/// Stage the uninstaller / updater under `new\` and return the file units
/// for the ones whose bytes differ from what the install directory holds.
async fn self_image_units(
    settings: &Settings,
    project: &ProjectConfig,
    staged: &SessionStaging,
    algo: &str,
    list_has_updater: bool,
    updater_staged: bool,
    mgr: &ManagedElevate,
) -> anyhow::Result<Vec<Unit>> {
    let Some(plan) = self_image_plan(
        settings,
        project,
        &staged.staging,
        list_has_updater,
        updater_staged,
    ) else {
        return Ok(Vec::new());
    };
    let raw = run_op(
        mgr,
        settings.elevate,
        IpcOperation::StageSelfImage(crate::installer::uninstall::StageSelfImageArgs {
            install_dir: settings.install_path.clone(),
            new_dir: staged.staging.new_dir().to_string_lossy().to_string(),
            hash_algorithm: algo.to_string(),
            names: plan.names,
            copy_from: plan.copy_from,
        }),
        progress_noop(),
    )
    .await
    .map_err(|e| e.attach(FILE_IO_FAILED))?;
    let IpcResult::StageSelfImage(staged_images) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    Ok(staged_images
        .into_iter()
        .filter(|s| !s.unchanged)
        .map(|s| {
            Unit::File(FileEntry {
                rel: s.rel,
                old: s.old,
                new: s.hash,
            })
        })
        .collect())
}

/// Add the installer's own files to the unit list. When the whole install
/// directory swaps as one root unit, they already sit inside `new\` and move
/// with it, so they join that unit's file list instead of getting their own.
fn merge_self_units(mut units: Vec<Unit>, self_units: Vec<Unit>) -> Vec<Unit> {
    let root = units.iter_mut().find_map(|u| match u {
        Unit::Dir { rel, files } if rel.is_empty() => Some(files),
        _ => None,
    });
    match root {
        Some(files) => {
            for unit in self_units {
                if let Unit::File(f) = unit {
                    files.retain(|e| normalize_rel(&e.rel) != normalize_rel(&f.rel));
                    files.push(f);
                }
            }
        }
        None => units.extend(self_units),
    }
    units
}

fn list_has_updater(hashed: &[FileMeta], updater_name: &str) -> bool {
    let want = normalize_rel(updater_name);
    hashed.iter().any(|h| normalize_rel(&h.file_name) == want)
}

/// Phase two: swap the staged files in. Returns whether the running
/// executable was among them.
async fn commit_staged(
    settings: &Settings,
    staged: &SessionStaging,
    journal: Journal,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<bool> {
    if !ui.inner.begin_commit() {
        return Err(Cancelled.into());
    }
    progress(
        ui,
        2,
        95.0,
        ProgressStage::Commit,
        None,
        Some(0),
        Some(journal.units.len() as u64),
    );
    let raw = run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::Commit(CommitArgs {
            staging_root: staged.root(),
            install_dir: settings.install_path.clone(),
            journal,
        }),
        ui,
        |ui, p| {
            if let Progress::CountOf { done, total } = p {
                let total = *total;
                progress(
                    ui,
                    2,
                    95.0 + (*done as f64 / total.max(1) as f64) * 3.0,
                    ProgressStage::Commit,
                    None,
                    Some(*done),
                    Some(total),
                );
            }
        },
    )
    .await?;
    let IpcResult::Commit(outcome) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    Ok(outcome.self_replaced)
}

/// End-of-session staging cleanup: the directory outlives the process only
/// when it parks the running executable.
async fn finish_staging(staged: &SessionStaging, self_replaced: bool, mgr: &ManagedElevate) {
    if self_replaced {
        schedule_cleanup(mgr, staged.elevate, staged.root()).await;
    } else {
        staged.discard(mgr).await;
    }
}

/// The side that created the staging root deletes it at its own exit. A
/// helper holding the cleanup is retired and kept connected until this
/// process exits (see `ManagedElevate::close`), so its cleanup never runs
/// under a later session that reopens the same root.
async fn schedule_cleanup(mgr: &ManagedElevate, elevate: bool, root: String) {
    if let Err(err) = run_op(
        mgr,
        elevate,
        IpcOperation::ScheduleCleanup(root),
        progress_noop(),
    )
    .await
    {
        tracing::warn!("schedule staging cleanup failed: {err:#}");
    }
}

fn notify_error(ui: &LiveUi<'_>, err: anyhow::Error) {
    if let Some(coded) = coded_from_error(&err) {
        ui.notify(&coded);
    }
}

async fn create_lnk_or_notify(
    mgr: &ManagedElevate,
    elevate: bool,
    args: CreateLnkArgs,
    ui: &LiveUi<'_>,
) {
    let lnk = args.lnk.clone();
    if let Err(err) = run_op(mgr, elevate, IpcOperation::CreateLnk(args), progress_noop()).await {
        tracing::warn!("create shortcut failed: {err:#}");
        notify_error(ui, err.attach_with(SHORTCUT_FAILED, lnk));
    }
}

#[async_trait::async_trait]
impl SessionUi for LiveUi<'_> {
    fn state(&self, state: &UiState) {
        self.inner.state(state);
    }
    async fn confirm(&self, prompt: Prompt) -> bool {
        self.inner.confirm(prompt).await
    }
    fn notify(&self, coded: &Coded) {
        self.inner.notify(coded);
    }
    fn plugin_host(&self) -> Option<std::sync::Arc<dyn crate::session::ui::PluginHost>> {
        self.inner.plugin_host()
    }
}

fn progress(
    ui: &LiveUi<'_>,
    sub_step: u32,
    percent: f64,
    stage: ProgressStage,
    subject: Option<&str>,
    done: Option<u64>,
    total: Option<u64>,
) {
    let mut state = ui.live.lock().unwrap_or_else(|e| e.into_inner());
    let step = if state.mode == crate::session::state::Mode::Uninstall {
        None
    } else {
        Some(sub_step as u8)
    };
    let mut value = UiProgress::new(stage, step, Some(percent));
    if let Phase::Running(previous) = &state.phase {
        if previous.stage == stage {
            value.processing_bps = previous.processing_bps;
            value.network_bps = previous.network_bps;
            value.network_pending = previous.network_pending;
        }
    }
    value.subject = subject.map(str::to_string);
    value.summary = done.map(|done| ProgressCounter {
        unit: stage.unit(),
        done,
        total,
    });
    state.phase = Phase::Running(value);
    drop(state);
    ui.emit_live();
}

fn log_session_start(
    kind: &str,
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
) {
    tracing::info!(
        "{kind} path={} source={} update={} silent={} non_interactive={} elevate={} online={} create_lnk={} dump={}",
        settings.install_path,
        settings.source_uri,
        settings.is_update,
        settings.silent,
        settings.non_interactive,
        settings.elevate,
        settings.online,
        settings.create_lnk,
        settings.dump_dir.is_some(),
    );
    let sources = config
        .embedded_config
        .as_ref()
        .and_then(|c| c.get("source"));
    let sources = match sources {
        Some(Value::Array(list)) => json!(list
            .iter()
            .map(|e| json!({ "id": e.get("id"), "uri": e.get("uri") }))
            .collect::<Vec<_>>()),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let mut args = serde_json::to_value(&config.args).unwrap_or(Value::Null);
    if let Some(obj) = args.as_object_mut() {
        if obj
            .get("mirrorc_cdk")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            obj.insert("mirrorc_cdk".into(), json!("<set>"));
        }
    }
    tracing::info!(
        "INSTALLER_CONFIG: {}",
        json!({
            "install_path": config.install_path,
            "install_path_exists": config.install_path_exists,
            "install_path_source": config.install_path_source,
            "is_uninstall": config.is_uninstall,
            "exe_path": config.exe_path,
            "args": args,
            "elevated": config.elevated,
            "app_name": project.app_name,
            "exe_name": project.exe_name,
            "need_web_view2": project.need_web_view2,
            "runtimes": project.runtimes,
            "embedded_config": { "source": sources },
            "embedded_files": config.embedded_files.as_ref().map(|f| f.len()),
            "embedded_index": config.embedded_index.as_ref().map(|i| i.len()),
            "has_metadata": config.enbedded_metadata.is_some(),
            "has_preset": config.preset.is_some(),
            "has_mirrorc_cdk": settings.mirrorc_cdk.as_ref().is_some_and(|s| !s.is_empty()),
        })
    );
}

fn log_plan_summary(plan: &InstallPlan, local: &[LocalFile]) {
    let install = plan
        .files
        .iter()
        .filter(|f| f.action == PlanAction::Install)
        .count();
    let skip_unchanged = plan
        .files
        .iter()
        .filter(|f| f.skip_reason == Some(SkipReason::Unchanged))
        .count();
    let skip_userdata = plan
        .files
        .iter()
        .filter(|f| f.skip_reason == Some(SkipReason::UserData))
        .count();
    let skip_ignore = plan
        .files
        .iter()
        .filter(|f| f.skip_reason == Some(SkipReason::IgnoreFolder))
        .count();
    tracing::info!(
        "plan files={} install={} skip_unchanged={} skip_userdata={} skip_ignore={} deletes={} local_scanned={}",
        plan.files.len(),
        install,
        skip_unchanged,
        skip_userdata,
        skip_ignore,
        plan.deletes.len(),
        local.len(),
    );
}

fn log_task_plan(tasks: &[InstallTask], ranges: &[String]) {
    let mut singles = 0usize;
    let mut merged = 0usize;
    let mut merged_files = 0usize;
    let mut merged_bytes = 0usize;
    for task in tasks {
        match task {
            InstallTask::Single(_) => singles += 1,
            InstallTask::Merged {
                files,
                download_size,
                ..
            } => {
                merged += 1;
                merged_files += files.len();
                merged_bytes += *download_size;
            }
        }
    }
    tracing::info!(
        "File grouping result: tasks={} singles={} merged={} merged_files={} merged_bytes={} ranges={}",
        tasks.len(),
        singles,
        merged,
        merged_files,
        merged_bytes,
        ranges.len(),
    );
    if !ranges.is_empty() {
        tracing::info!("DFS2 ranges collected: {ranges:?}");
    }
}

fn source_id(project: &ProjectConfig, uri: &str) -> String {
    match &project.source {
        SourceField::Single(_) => "default".to_string(),
        SourceField::List(list) => list
            .iter()
            .find(|s| s.uri == uri)
            .map(|s| s.id.clone())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

fn insight_base(
    project: &ProjectConfig,
    settings: &Settings,
    config: &InstallerConfig,
    uninstall: bool,
) -> String {
    let mut qs = Vec::new();
    if settings.non_interactive {
        qs.push("non_interactive=1");
    }
    if settings.silent {
        qs.push("silent=1");
    }
    if uninstall {
        qs.push("uninstall=1");
    }
    if settings.online {
        qs.push("online=1");
    }
    if config
        .embedded_index
        .as_ref()
        .is_some_and(|index| !index.is_empty())
    {
        qs.push("pack=1");
    }
    format!("/{}?{}", project.app_name, qs.join("&"))
}

fn prepare_event(
    settings: &Settings,
    config: &InstallerConfig,
    source_id: &str,
    version: &str,
    used_online: bool,
) -> String {
    let action = if settings.is_update {
        "update"
    } else {
        "install"
    };
    let packed = config
        .embedded_index
        .as_ref()
        .is_some_and(|index| !index.is_empty());
    if packed {
        if used_online {
            format!("{action}/packed+{source_id}/{version}")
        } else {
            format!("{action}/packed/{version}")
        }
    } else {
        format!("{action}/{source_id}/{version}")
    }
}

fn txn_status(result: &anyhow::Result<SessionResult>) -> &'static str {
    match result {
        Ok(r) if r.cancelled => "cancelled",
        Ok(_) => "ok",
        Err(_) => "internal_error",
    }
}

fn emit_insight(
    project: &ProjectConfig,
    settings: &Settings,
    config: &InstallerConfig,
    event: &str,
    data: Option<Value>,
    uninstall: bool,
) {
    let url = insight_base(project, settings, config, uninstall);
    let event = event.to_string();
    tokio::spawn(async move {
        send_ev_insight(&url, &event, data).await;
    });
}

pub async fn run_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    base: &UiState,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    log_session_start("install", settings, config, project);
    ensure_helper_sees_path(settings, mgr)?;
    let ui = LiveUi::new(ui, base);
    let txn = crate::utils::sentry::Transaction::start(
        if settings.is_update {
            "update"
        } else {
            "install"
        },
        "session",
    );
    let result = if settings.source_uri.starts_with("mirrorc://") {
        run_mirrorc(settings, config, project, &ui, mgr).await
    } else {
        run_dfs_install(settings, config, project, &ui, mgr, &txn).await
    };
    // a phase-one cancel is a user decision, not a failure
    let result = match result {
        Err(err) if is_cancelled(&err) => {
            tracing::info!("install cancelled by the user");
            Ok(SessionResult::cancelled(settings.is_update))
        }
        other => other,
    };
    txn.finish(txn_status(&result));
    if let Err(err) = &result {
        // fail counter：低基数分类维度，不携带自由文本（见遥测通道职责收敛）
        emit_insight(
            project,
            settings,
            config,
            "fail",
            Some(json!({ "kind": fail_kind(err) })),
            false,
        );
    }
    result
}

async fn run_dfs_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
    txn: &crate::utils::sentry::Transaction,
) -> anyhow::Result<SessionResult> {
    progress(ui, 0, 1.0, ProgressStage::FetchMetadata, None, None, None);
    session_dump!(
        settings.dump_dir.as_deref(),
        "01-settings.json",
        json!({
            "install_path": settings.install_path,
            "source_uri": settings.source_uri,
            "is_update": settings.is_update,
            "online": settings.online,
            "elevate": settings.elevate,
            "exe_name": project.exe_name,
            "updater_name": project.updater_name,
            "app_name": project.app_name,
            "user_data_path": project.user_data_path,
            "ignore_folder_path": project.ignore_folder_path,
        })
    );

    let embedded_meta = config.enbedded_metadata.clone();
    let mut source_ctx = SourceCtx::from_embedded(config.embedded_files.as_deref().unwrap_or(&[]));
    source_ctx.attach_plugin(ui.plugin_host());
    // span 只包网络部分；pick_metadata 可能弹版本选择框，用户等待不计入
    let mut online_err = None;
    let online_meta = txn
        .timed("metadata", async {
            match fetch_metadata(
                &settings.source_uri,
                settings.dfs_extras.as_deref(),
                &mut source_ctx,
            )
            .await
            {
                Ok(meta) => Some(meta),
                Err(err) => {
                    tracing::warn!("online metadata failed: {err:#}");
                    online_err = Some(attach_metadata(err));
                    None
                }
            }
        })
        .await;

    let (mut latest, used_online) =
        pick_metadata(settings, config, ui, embedded_meta, online_meta, online_err).await?;
    if !used_online {
        source_ctx.restore_local_package(
            config.embedded_index.as_deref(),
            Some(latest.tag_name.clone()),
        );
    }

    if settings.is_update
        && latest.installer.is_some()
        && config.enbedded_metadata.is_none()
        && !latest
            .hashed
            .iter()
            .any(|e| e.file_name == project.updater_name)
    {
        let installer = latest.installer.clone().unwrap();
        latest.hashed.push(FileMeta {
            file_name: project.updater_name.clone(),
            size: installer.size,
            md5: installer.md5,
            xxh: installer.xxh,
            installer: Some(true),
        });
    }

    if settings.elevate {
        let _ = run_op(mgr, true, IpcOperation::Ping, progress_noop()).await;
    }
    emit_insight(
        project,
        settings,
        config,
        &prepare_event(
            settings,
            config,
            &source_id(project, &settings.source_uri),
            &latest.tag_name,
            used_online,
        ),
        None,
        false,
    );
    if !prepare_process(settings, project, ui, mgr, &latest.tag_name).await? {
        tracing::info!("install cancelled at process-running prompt");
        return Ok(SessionResult::cancelled(settings.is_update));
    }
    ui.check_cancel()?;

    let hash_key = latest.hash_key()?;
    let algo = match hash_key {
        HashKey::Md5 => "md5",
        HashKey::Xxh => "xxh",
    };
    // the staging directory is opened once the target is known: a journal left
    // by an interrupted commit is only worth finishing if it still describes
    // what this session is about to install
    let (staged, journal) = open_staging(settings, mgr).await?;
    let wanted: std::collections::HashMap<String, String> = latest
        .hashed
        .iter()
        .filter_map(|h| Some((normalize_rel(&h.file_name), hash_of_item(h, hash_key)?)))
        .collect();
    let deletes: std::collections::HashSet<String> =
        latest.deletes.iter().map(|d| normalize_rel(d)).collect();
    let (staged, recovered_self) = match journal {
        Some(text) => {
            recover_or_discard(
                settings, project, staged, text, algo, &wanted, &deletes, None, ui, mgr,
            )
            .await?
        }
        None => (staged, false),
    };
    let result = dfs_staged(
        settings,
        config,
        project,
        ui,
        mgr,
        txn,
        &latest,
        hash_key,
        algo,
        &mut source_ctx,
        used_online,
        &staged,
    )
    .await;
    let self_replaced = recovered_self || matches!(result, Ok((_, true)));
    finish_staging(&staged, self_replaced, mgr).await;
    result.map(|(r, _)| r)
}

fn has_runtimes(runtimes: Option<&[String]>) -> bool {
    runtimes.is_some_and(|r| !r.is_empty())
}

/// Everything between "target known" and "files swapped in", with the staging
/// directory available. Returns the outcome and whether the swap replaced the
/// running executable.
#[allow(clippy::too_many_arguments)]
// `used_online` only feeds the debug session dump
#[cfg_attr(not(debug_assertions), allow(unused_variables))]
async fn dfs_staged(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
    txn: &crate::utils::sentry::Transaction,
    latest: &RepoMetadata,
    hash_key: HashKey,
    algo: &str,
    source_ctx: &mut SourceCtx,
    used_online: bool,
    staged: &SessionStaging,
) -> anyhow::Result<(SessionResult, bool)> {
    let mut ignore_nonempty = Vec::new();
    if settings.is_update {
        for folder in &project.ignore_folder_path {
            let full = settings.expand(folder, &project.app_name);
            match tokio::fs::read_dir(&full).await {
                Ok(mut entries) => match entries.next_entry().await {
                    Ok(Some(_)) => ignore_nonempty.push(full),
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!("ignoreFolderPath check failed ({folder}), skip rule: {err}")
                    }
                },
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    tracing::warn!("ignoreFolderPath check failed ({folder}), skip rule: {err}")
                }
            }
        }
    }
    progress(ui, 1, 5.0, ProgressStage::ScanFiles, None, None, None);
    let (local, scan) = txn
        .timed(
            "hash-scan",
            scan_local(
                settings,
                project,
                latest,
                hash_key,
                &ignore_nonempty,
                ui,
                mgr,
            ),
        )
        .await?;
    ui.check_cancel()?;
    txn.set_measurement("hash_scan_files", local.len() as f64, "none");
    txn.set_measurement(
        "hash_scan_bytes",
        local.iter().map(|f| f.size).sum::<u64>() as f64,
        "byte",
    );

    let plan = build_plan(&PlanInput {
        install_path: settings.install_path.clone(),
        is_update: settings.is_update,
        hash_key,
        hashed: latest.hashed.clone(),
        patches: latest.patches.clone(),
        deletes: latest.deletes.clone(),
        local: local.clone(),
        embedded_names: config
            .embedded_files
            .as_ref()
            .map(|files| files.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default(),
        user_data_path: project.user_data_path.clone(),
        ignore_nonempty: ignore_nonempty.clone(),
        app_name: project.app_name.clone(),
    });

    session_dump!(
        settings.dump_dir.as_deref(),
        "02-meta-scan.json",
        json!({
            "hash_key": hash_key,
            "used_online": used_online,
            "tag_name": latest.tag_name,
            "hashed": latest.hashed,
            "patches": latest.patches,
            "deletes": latest.deletes,
            "local": local,
            "embedded_names": config.embedded_files.as_ref().map(|f| f.iter().map(|e| e.name.clone()).collect::<Vec<_>>()).unwrap_or_default(),
            "ignore_nonempty": ignore_nonempty,
        })
    );
    let mut plan = plan;
    let probe_rels = files_to_probe_writable(&plan, &local);
    if !probe_rels.is_empty() {
        let paths: Vec<String> = probe_rels
            .iter()
            .map(|name| join_install(&settings.install_path, name))
            .collect();
        let raw = run_op(
            mgr,
            settings.elevate,
            IpcOperation::ProbeWritable(paths),
            progress_noop(),
        )
        .await?;
        let IpcResult::ProbeWritable(unwritable) = raw else {
            bail!("IPC_SHAPE_ERR");
        };
        mark_unwritable(&mut plan.files, &settings.install_path, &unwritable);
    }
    session_dump!(settings.dump_dir.as_deref(), "03-plan.json", plan);
    log_plan_summary(&plan, &local);

    let to_install: Vec<_> = plan
        .files
        .iter()
        .filter(|f| f.action == PlanAction::Install)
        .cloned()
        .collect();

    if to_install.is_empty()
        && plan.deletes.is_empty()
        && !has_runtimes(project.runtimes.as_deref())
    {
        session_dump!(
            settings.dump_dir.as_deref(),
            "04-install-ops.json",
            Vec::<IpcOperation>::new()
        );
        tracing::info!("already latest, tag={}", latest.tag_name);
        // nothing else to swap, but the uninstaller may still need a refresh
        let units = self_image_units(
            settings,
            project,
            staged,
            algo,
            list_has_updater(&latest.hashed, &project.updater_name),
            false,
            mgr,
        )
        .await?;
        let self_replaced = if units.is_empty() {
            false
        } else {
            commit_staged(
                settings,
                staged,
                Journal {
                    hash_algorithm: algo.to_string(),
                    archive: None,
                    units,
                },
                ui,
                mgr,
            )
            .await?
        };
        txn.timed(
            "finalize",
            finish_install(settings, config, project, Some(latest), None, ui, mgr),
        )
        .await;
        return Ok((
            SessionResult::install(true, settings.is_update),
            self_replaced,
        ));
    }

    let mut sid = None;
    if to_install.is_empty() {
        session_dump!(
            settings.dump_dir.as_deref(),
            "04-install-ops.json",
            Vec::<IpcOperation>::new()
        );
        tracing::info!(
            "no files to download, applying deletes/runtimes, tag={}",
            latest.tag_name
        );
    } else {
        let occupied: Vec<String> = to_install
            .iter()
            .filter(|f| f.unwritable && f.file_name != project.updater_name)
            .map(|f| f.file_name.clone())
            .collect();
        if !occupied.is_empty() {
            tracing::info!("occupied files: {}", occupied.join(", "));
            if !ui
                .confirm(Prompt {
                    id: String::new(),
                    kind: "occupied_files",
                    items: occupied.clone(),
                    params: std::collections::BTreeMap::new(),
                })
                .await
            {
                tracing::info!("install cancelled at occupied-files prompt");
                return Ok((SessionResult::cancelled(settings.is_update), false));
            }
        }
        ui.check_cancel()?;

        let install_items: Vec<InstallItem> = to_install
            .iter()
            .filter_map(|file| {
                let item = latest
                    .hashed
                    .iter()
                    .find(|h| h.file_name == file.file_name)?
                    .clone();
                Some(InstallItem { item })
            })
            .collect();
        txn.set_measurement("download_files", install_items.len() as f64, "none");
        let download_bytes: u64 = install_items.iter().map(|i| i.item.size).sum();
        txn.set_measurement("download_bytes", download_bytes as f64, "byte");
        // phase one holds every produced file next to the existing install
        ensure_space(staged, download_bytes)?;

        let tasks = plan_tasks(
            &install_items
                .iter()
                .map(|i| i.item.clone())
                .collect::<Vec<_>>(),
            hash_key,
            config.embedded_files.as_deref().unwrap_or(&[]),
            &latest.patches,
            source_ctx,
            &local,
        );
        let ranges = dfs2_ranges(
            &tasks,
            source_ctx,
            hash_key,
            config.embedded_files.as_deref().unwrap_or(&[]),
            &latest.patches,
            &local,
        );
        progress(
            ui,
            2,
            20.0,
            ProgressStage::CreateDownloadSession,
            None,
            None,
            None,
        );
        if let Err(err) =
            ensure_dfs2_session(source_ctx, ranges.clone(), settings.dfs_extras.as_deref()).await
        {
            cleanup_dfs2(source_ctx).await;
            return Err(attach_download_or(err, NO_DOWNLOAD_NODE, None, None));
        }
        // Every failure from here on happened inside this DFS session; the id lets
        // the DFS side find the matching server log.
        sid = source_ctx.dfs2_session_id();
        let tag_sid = |err: anyhow::Error| match &sid {
            Some(sid) => tag_session(err, sid.clone()),
            None => err,
        };
        log_task_plan(&tasks, &ranges);

        progress(
            ui,
            2,
            20.0,
            ProgressStage::PrepareDownload,
            None,
            None,
            None,
        );
        let ops = txn
            .timed(
                "download",
                install_files(
                    settings,
                    config,
                    latest,
                    hash_key,
                    &tasks,
                    &local,
                    source_ctx,
                    &staged.staging,
                    ui,
                    mgr,
                ),
            )
            .await;
        cleanup_dfs2(source_ctx).await;
        #[cfg_attr(not(debug_assertions), allow(unused_variables))]
        let ops = ops.map_err(tag_sid)?;
        tracing::info!(
            "All tasks completed successfully: files={} ops={}",
            to_install.len(),
            ops.len()
        );
        session_dump!(settings.dump_dir.as_deref(), "04-install-ops.json", ops);
    }
    ui.check_cancel()?;
    let tag_sid = |err: anyhow::Error| match &sid {
        Some(sid) => tag_session(err, sid.clone()),
        None => err,
    };

    // the installer's own files join the same journal
    let updater_rel = normalize_rel(&project.updater_name);
    let updater_staged = to_install
        .iter()
        .any(|f| normalize_rel(&f.file_name) == updater_rel);
    let self_units = self_image_units(
        settings,
        project,
        staged,
        algo,
        list_has_updater(&latest.hashed, &project.updater_name),
        updater_staged,
        mgr,
    )
    .await
    .map_err(tag_sid)?;

    // phase two: everything is staged and verified, swap it in
    let units = merge_self_units(
        build_units(&plan, &latest.hashed, hash_key, &local, &scan),
        self_units,
    );
    session_dump!(settings.dump_dir.as_deref(), "05-commit-units.json", units);
    let self_replaced = txn
        .timed(
            "commit",
            commit_staged(
                settings,
                staged,
                Journal {
                    hash_algorithm: algo.to_string(),
                    archive: None,
                    units,
                },
                ui,
                mgr,
            ),
        )
        .await
        .map_err(tag_sid)?;

    txn.timed(
        "runtimes",
        install_runtimes(settings, config, project, &staged.staging, ui, mgr),
    )
    .await;
    progress(ui, 3, 98.0, ProgressStage::Finalize, None, None, None);
    txn.timed(
        "finalize",
        finish_install(settings, config, project, Some(latest), None, ui, mgr),
    )
    .await;
    Ok((
        SessionResult::install(false, settings.is_update),
        self_replaced,
    ))
}

struct InstallItem {
    item: FileMeta,
}

async fn pick_metadata(
    settings: &Settings,
    config: &InstallerConfig,
    ui: &LiveUi<'_>,
    local: Option<RepoMetadata>,
    online: Option<RepoMetadata>,
    online_err: Option<anyhow::Error>,
) -> anyhow::Result<(RepoMetadata, bool)> {
    match (local, online) {
        (None, None) => {
            Err(online_err
                .unwrap_or_else(|| anyhow::Error::from(Coded::bare(METADATA_UNREACHABLE))))
        }
        (None, Some(online)) => {
            tracing::info!("Local meta not found, use online meta");
            Ok((online, true))
        }
        (Some(local), None) => {
            tracing::info!("Local meta found, use local meta");
            Ok((local, false))
        }
        (Some(local), Some(online)) => {
            if settings.online {
                tracing::info!("Force online meta, tag={}", online.tag_name);
                return Ok((online, true));
            }
            if online.tag_name != local.tag_name && version_gt(&online.tag_name, &local.tag_name) {
                tracing::info!(
                    "Version update detected local={} online={}",
                    local.tag_name,
                    online.tag_name
                );
                let no_index = config
                    .embedded_index
                    .as_ref()
                    .map(|i| i.is_empty())
                    .unwrap_or(true);
                let take_online = if settings.auto_answer {
                    settings.is_update && no_index
                } else {
                    (settings.is_update && no_index)
                        || ui
                            .confirm(Prompt {
                                id: String::new(),
                                kind: "version_mismatch",
                                items: Vec::new(),
                                params: std::collections::BTreeMap::new(),
                            })
                            .await
                };
                if take_online {
                    tracing::info!("use online meta, tag={}", online.tag_name);
                    return Ok((online, true));
                }
                tracing::info!("Has version update but use local meta");
            } else {
                tracing::info!("Local meta found, use local meta");
            }
            Ok((local, false))
        }
    }
}

async fn prepare_process(
    settings: &Settings,
    project: &ProjectConfig,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
    _version: &str,
) -> anyhow::Result<bool> {
    let found = run_op(
        mgr,
        false,
        IpcOperation::FindProcessByName(project.exe_name.clone()),
        progress_noop(),
    )
    .await
    .unwrap_or(IpcResult::FindProcessByName(Vec::new()));
    let IpcResult::FindProcessByName(procs) = found else {
        bail!("IPC_SHAPE_ERR");
    };
    let target = join_install(&settings.install_path, &project.exe_name)
        .replace('\\', "/")
        .to_lowercase();
    let running: Vec<(u32, String)> = procs
        .into_iter()
        .filter(|(_, path)| path.replace('\\', "/").to_lowercase() == target)
        .collect();
    if running.is_empty() {
        return Ok(true);
    }
    if !ui
        .confirm(Prompt {
            id: String::new(),
            kind: "process_running",
            items: vec![project.app_name.clone()],
            params: std::collections::BTreeMap::new(),
        })
        .await
    {
        return Ok(false);
    }
    for (pid, _) in &running {
        if run_op(
            mgr,
            settings.elevate,
            IpcOperation::KillProcess(*pid),
            progress_noop(),
        )
        .await
        .is_err()
        {
            run_op(mgr, true, IpcOperation::KillProcess(*pid), progress_noop())
                .await
                .attach(PROCESS_KILL_FAILED)?;
        }
    }
    Ok(true)
}

async fn scan_local(
    settings: &Settings,
    project: &ProjectConfig,
    latest: &RepoMetadata,
    hash_key: HashKey,
    ignore_nonempty: &[String],
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<(Vec<LocalFile>, LocalScan)> {
    let algo = match hash_key {
        HashKey::Md5 => "md5",
        HashKey::Xxh => "xxh",
    };
    let files: Vec<String> = latest.hashed.iter().map(|e| e.file_name.clone()).collect();
    let skip_hash = collect_skip_hash(
        &latest.hashed,
        &settings.install_path,
        &project.app_name,
        &project.user_data_path,
        ignore_nonempty,
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();
    let mut op_fut = Box::pin(run_op(
        mgr,
        settings.elevate,
        IpcOperation::CheckLocalFiles {
            source: settings.install_path.clone(),
            hash_algorithm: algo.to_string(),
            file_list: files,
            skip_hash,
        },
        progress_notify(move |p| {
            let _ = tx.send(p);
        }),
    ));
    let raw = loop {
        tokio::select! {
            Some(p) = rx.recv() => {
                let Progress::CountOf { done: cur, total } = p else {
                    continue;
                };

                progress(ui, 1, 5.0 + (cur as f64 / total.max(1) as f64) * 15.0, ProgressStage::ScanFiles, None, Some(cur), Some(total));
            }
            res = &mut op_fut => break res?,
        }
    };
    progress(ui, 1, 20.0, ProgressStage::ScanFiles, None, None, None);
    let IpcResult::CheckLocalFiles(scan) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    let files = scan
        .files
        .iter()
        .map(|e| {
            let file_name = strip_install_prefix(&e.file_name, &settings.install_path);
            LocalFile {
                file_name,
                hash: e.hash.clone(),
                size: e.size,
                unwritable: e.unwritable,
            }
        })
        .collect();
    Ok((
        files,
        LocalScan {
            files: Vec::new(),
            ..scan
        },
    ))
}

struct FileProg {
    name: String,
    size: u64,
    downloaded: u64,
    running: bool,
    network_pending: bool,
    action: FileAction,
    bytes: Option<ByteProgress>,
}

struct DownloadProg {
    files: Vec<FileProg>,
    processed: u64,
    network_bytes: u64,
    processing_rate: super::rate::Rate,
    network_rate: super::rate::Rate,
}

impl DownloadProg {
    fn from_tasks(
        tasks: &[InstallTask],
        local: &[Embedded],
        patches: &[crate::utils::metadata::PatchInfo],
        hash_key: HashKey,
    ) -> Self {
        let mut files = Vec::new();
        for task in tasks {
            let network = !is_local_task(task, local, patches, hash_key);
            let mut push = |item: &FileMeta, size| {
                files.push(FileProg {
                    name: item.file_name.clone(),
                    size,
                    downloaded: 0,
                    running: false,
                    network_pending: network,
                    action: FileAction::Download,
                    bytes: None,
                })
            };
            match task {
                InstallTask::Single(item) => push(item, item.size),
                InstallTask::Merged { files, .. } => {
                    for file in files {
                        push(
                            &file.item,
                            file.patch.as_ref().map_or(file.item.size, |p| p.size),
                        );
                    }
                }
            }
        }
        Self {
            files,
            processed: 0,
            network_bytes: 0,
            processing_rate: super::rate::Rate::new(Instant::now()),
            network_rate: super::rate::Rate::new(Instant::now()),
        }
    }
    fn render(&mut self) -> UiProgress {
        let total: u64 = self.files.iter().map(|f| f.size).sum();
        let done: u64 = self.files.iter().map(|f| f.downloaded).sum();
        let mut progress = UiProgress::new(
            ProgressStage::ProcessFiles,
            Some(2),
            Some(
                20.0 + if total == 0 {
                    75.0
                } else {
                    done as f64 / total.max(1) as f64 * 75.0
                },
            ),
        );
        progress.summary = Some(ProgressCounter {
            unit: ProgressUnit::Bytes,
            done,
            total: Some(total),
        });
        progress.processing_bps = self.processing_rate.sample(Instant::now(), self.processed);
        progress.network_pending = self.files.iter().any(|f| f.network_pending);
        let speed = self.network_rate.sample(Instant::now(), self.network_bytes);
        progress.network_bps = if progress.network_pending {
            speed
        } else {
            None
        };
        progress.files = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.running)
            .map(|(id, f)| FileProgress {
                id: id as u32,
                name: f.name.clone(),
                action: f.action,
                bytes: f.bytes.clone(),
            })
            .collect();
        progress
    }
}

struct DownloadFeed {
    network_bytes: u64,
    network_finished: bool,
    sequence: Vec<u64>,
    processed: Vec<u64>,
}

#[derive(Clone)]
struct ProgressHandle {
    inner: Arc<Mutex<DownloadProg>>,
    ids: Vec<usize>,
    job: crate::ipc::download::Job,
}

impl ProgressHandle {
    fn prepare(&self, op: &IpcOperation) {
        if let IpcOperation::InstallFile(args) = op {
            let total = args.mode.transfer_total(args.output_size);
            let source = args.mode.primary();
            let mut g = self.inner.lock().unwrap();
            if let Some(file) = self.ids.first().and_then(|id| g.files.get_mut(*id)) {
                file.size = total;
                file.downloaded = 0;
                file.network_pending = !source.is_local();
                file.bytes = None;
                file.action = FileAction::Retry;
            }
        }
    }
    fn finish(&self) {
        let mut g = self.inner.lock().unwrap();
        for &id in &self.ids {
            g.files[id].running = false;
            g.files[id].network_pending = false;
        }
    }
    fn only(&self, local_idx: usize) -> Self {
        Self {
            inner: self.inner.clone(),
            job: self.job.clone(),
            ids: self.ids.get(local_idx).copied().into_iter().collect(),
        }
    }
    fn callback(&self) -> ProgressNotify {
        let handle = self.clone();
        let state = Mutex::new(DownloadFeed {
            network_bytes: 0,
            network_finished: false,
            sequence: vec![0; self.ids.len()],
            processed: vec![0; self.ids.len()],
        });
        progress_notify(move |p| {
            let mut state = state.lock().unwrap();
            if state.network_finished {
                return;
            }
            let mut g = handle.inner.lock().unwrap();
            match p {
                Progress::File(file) => {
                    let index = file.index as usize;
                    let Some(&id) = handle.ids.get(index) else {
                        return;
                    };
                    if file.sequence <= state.sequence[index] {
                        return;
                    }
                    state.sequence[index] = file.sequence;
                    g.processed += file.processed.saturating_sub(state.processed[index]);
                    state.processed[index] = file.processed;
                    let target = &mut g.files[id];
                    target.running = !file.finished;
                    target.size = file.work_total;
                    target.downloaded = file.completed;
                    target.action = file.action;
                    target.bytes = file.done.map(|done| ByteProgress {
                        done,
                        total: file.total,
                    });
                    if file.finished
                        || matches!(file.action, FileAction::Verify | FileAction::Flush)
                    {
                        target.network_pending = false;
                    }
                }
                Progress::Network(snapshot) | Progress::NetworkFinal(snapshot) => {
                    g.network_bytes += snapshot.bytes.saturating_sub(state.network_bytes);
                    state.network_bytes = state.network_bytes.max(snapshot.bytes);
                    let finished = matches!(p, Progress::NetworkFinal(_));
                    if finished || snapshot.bytes > 0 || snapshot.active > 0 {
                        for &id in &handle.ids {
                            g.files[id].network_pending = !finished && snapshot.active > 0;
                        }
                    }
                    state.network_finished = finished;
                }
                _ => {}
            }
        })
    }
}

fn download_progress(ui: &LiveUi<'_>, prog: &Arc<Mutex<DownloadProg>>) {
    let value = prog.lock().unwrap().render();
    {
        let mut state = ui.live.lock().unwrap();
        state.phase = Phase::Running(value);
    }
    ui.emit_live();
}

fn task_bytes(task: &InstallTask) -> u64 {
    match task {
        InstallTask::Single(item) => item.size,
        InstallTask::Merged { files, .. } => files.iter().map(|f| f.item.size).sum(),
    }
}

fn is_local_task(
    task: &InstallTask,
    local_files: &[Embedded],
    patches: &[crate::utils::metadata::PatchInfo],
    hash_key: HashKey,
) -> bool {
    match task {
        InstallTask::Single(item) => {
            file_mode(item, hash_key, local_files, patches, false) == FileMode::Local
        }
        InstallTask::Merged { .. } => false,
    }
}

fn size_threshold(sizes: &[u64]) -> u64 {
    if sizes.len() <= 3 {
        return 0;
    }
    let mut sorted = sizes.to_vec();
    sorted.sort_by(|a, b| b.cmp(a));
    let target = 5.min(2.max(sorted.len() * 3 / 10));
    let idx = target.min(sorted.len() - 1);
    sorted[idx] * 8 / 10
}

fn format_size(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1}MB", size as f64 / 1024.0 / 1024.0)
    } else if size >= 1024 {
        format!("{:.0}KB", size as f64 / 1024.0)
    } else {
        format!("{size}B")
    }
}

fn log_task(
    mode: &str,
    size: u64,
    name: &str,
    insight: Option<&InsightItem>,
    ok: bool,
    err: Option<&str>,
) {
    let insight_json = insight
        .and_then(|i| serde_json::to_string(i).ok())
        .unwrap_or_else(|| "{}".to_string());
    if ok {
        tracing::info!("[{mode}] {} {name} {insight_json}", format_size(size));
    } else {
        tracing::error!(
            "[{mode}] {} {name} {} {insight_json}",
            format_size(size),
            err.unwrap_or("")
        );
    }
}

const RESOLVE_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);

/// Answers the executor's URL requests. Requests arriving within a 50ms window,
/// reset by each new request, are resolved together so DFS2 pays for one batch
/// request. Every waiting request holds a request slot, so the window closes
/// once all slots are waiting. Never returns.
async fn resolve_in_windows(mgr: &ManagedElevate, ctx: &SourceCtx, extras: Option<&str>) {
    let mut window = Vec::new();
    let mut inflight = futures::stream::FuturesUnordered::new();
    let timer = tokio::time::sleep(RESOLVE_WINDOW);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            Some(query) = mgr.next_resolution() => {
                window.push(query);
                timer.as_mut().reset(tokio::time::Instant::now() + RESOLVE_WINDOW);
            }
            _ = &mut timer, if !window.is_empty() => {
                let queries: Vec<(crate::ipc::download::Resolve, bool)> = std::mem::take(&mut window);
                inflight.push(async move {
                    let ranges: Vec<_> = queries.iter().map(|(q, _)| (q.offset, q.len)).collect();
                    let urls = resolve_range_urls(ctx, extras, &ranges).await;
                    for ((query, remote), url) in queries.into_iter().zip(urls) {
                        mgr.resolve_reply(crate::ipc::download::Reply { id: query.id, url }, remote)
                            .await;
                    }
                });
            }
            Some(()) = futures::StreamExt::next(&mut inflight), if !inflight.is_empty() => {}
            else => std::future::pending::<()>().await,
        }
    }
}

async fn install_files(
    settings: &Settings,
    config: &InstallerConfig,
    latest: &RepoMetadata,
    hash_key: HashKey,
    tasks: &[InstallTask],
    disk_files: &[LocalFile],
    source_ctx: &SourceCtx,
    staging: &Staging,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<Vec<IpcOperation>> {
    let local_files = Arc::new(config.embedded_files.clone().unwrap_or_default());
    let disk_files = Arc::new(disk_files.to_vec());
    let prog = Arc::new(Mutex::new(DownloadProg::from_tasks(
        tasks,
        local_files.as_slice(),
        &latest.patches,
        hash_key,
    )));
    let has_error = Arc::new(AtomicBool::new(false));
    let cancel = ui.cancel.clone();
    let sizes: Vec<u64> = tasks.iter().map(task_bytes).collect();
    let threshold = size_threshold(&sizes);
    let session = uuid::Uuid::new_v4().to_string();
    let (large_limit, small_limit) = crate::ipc::download::limits(source_ctx.policy.concurrency);
    tracing::info!(
        policy = ?source_ctx.policy,
        threshold,
        large_limit,
        small_limit,
        "download plan limits"
    );
    run_op(
        mgr,
        settings.elevate,
        IpcOperation::BeginDownload(crate::ipc::download::Config {
            id: session.clone(),
            concurrency: source_ctx.policy.concurrency,
            prefetch_bytes: crate::fs::staging::free_space(staging.root())
                .map(|free| free.saturating_sub(tasks.iter().map(task_bytes).sum()))
                .unwrap_or(256 * 1024 * 1024)
                .min(256 * 1024 * 1024) as usize,
            dl_dir: staging.dl_dir().to_string_lossy().into_owned(),
        }),
        progress_noop(),
    )
    .await?;
    // 这里限制同时进行的安装任务。download::Session 用同一组大/小名额限制在途
    // HTTP 请求，并让每个任务先拿到一个请求，同文件的后续切片才能预取。
    // 并成一个信号量会让切片数乘上任务数。
    let local_sem = Arc::new(Semaphore::new(16));
    let large_sem = Arc::new(Semaphore::new(large_limit));
    let small_sem = Arc::new(Semaphore::new(small_limit));
    let mut resolving = Box::pin(resolve_in_windows(
        mgr,
        source_ctx,
        settings.dfs_extras.as_deref(),
    ));
    let mut file_cursor = 0usize;
    let mut futs = Vec::new();
    for task in tasks {
        let ids = match task {
            InstallTask::Single(_) => {
                let ids = vec![file_cursor];
                file_cursor += 1;
                ids
            }
            InstallTask::Merged { files, .. } => {
                let ids: Vec<usize> = (file_cursor..file_cursor + files.len()).collect();
                file_cursor += files.len();
                ids
            }
        };
        let handle = ProgressHandle {
            inner: prog.clone(),
            job: crate::ipc::download::Job {
                session: session.clone(),
                large: task_bytes(task) >= threshold,
            },
            ids,
        };
        let sem = if is_local_task(task, local_files.as_ref(), &latest.patches, hash_key) {
            local_sem.clone()
        } else if task_bytes(task) >= threshold {
            large_sem.clone()
        } else {
            small_sem.clone()
        };
        let has_error = has_error.clone();
        let local_files = local_files.clone();
        let disk_files = disk_files.clone();
        let cancel = cancel.clone();
        futs.push(async move {
            let _permit = sem.acquire().await;
            if has_error.load(Ordering::Relaxed) || cancel.is_cancelled() {
                return None;
            }
            let local_ref = local_files.as_slice();
            let disk_ref = disk_files.as_slice();
            let res = match task {
                InstallTask::Single(item) => {
                    install_one(
                        settings,
                        local_ref,
                        disk_ref,
                        latest,
                        hash_key,
                        item,
                        source_ctx,
                        staging,
                        mgr,
                        false,
                        Some(handle.clone()),
                    )
                    .await
                }
                InstallTask::Merged {
                    files,
                    range,
                    download_size,
                    ..
                } => {
                    match install_merged(
                        settings,
                        local_ref,
                        latest,
                        hash_key,
                        files,
                        range,
                        *download_size,
                        source_ctx,
                        staging,
                        mgr,
                        handle.clone(),
                    )
                    .await
                    {
                        Ok(merged) if merged.failed.is_empty() => Ok(merged.op),
                        Ok(merged) => {
                            tracing::warn!(
                                "merged download partial fail, fallback {} files",
                                merged.failed.len()
                            );
                            fallback_merged_files(
                                settings,
                                local_ref,
                                disk_ref,
                                latest,
                                hash_key,
                                files,
                                &merged.failed,
                                source_ctx,
                                staging,
                                mgr,
                                &handle,
                                &has_error,
                            )
                            .await
                        }
                        Err(err) => {
                            tracing::warn!("merged download failed, retry: {err:#}");
                            match install_merged(
                                settings,
                                local_ref,
                                latest,
                                hash_key,
                                files,
                                range,
                                *download_size,
                                source_ctx,
                                staging,
                                mgr,
                                handle.clone(),
                            )
                            .await
                            {
                                Ok(merged) if merged.failed.is_empty() => Ok(merged.op),
                                Ok(merged) => {
                                    fallback_merged_files(
                                        settings,
                                        local_ref,
                                        disk_ref,
                                        latest,
                                        hash_key,
                                        files,
                                        &merged.failed,
                                        source_ctx,
                                        staging,
                                        mgr,
                                        &handle,
                                        &has_error,
                                    )
                                    .await
                                }
                                Err(err) => {
                                    tracing::warn!("merged download failed, fallback: {err:#}");
                                    let all: Vec<usize> = (0..files.len()).collect();
                                    fallback_merged_files(
                                        settings, local_ref, disk_ref, latest, hash_key, files,
                                        &all, source_ctx, staging, mgr, &handle, &has_error,
                                    )
                                    .await
                                }
                            }
                        }
                    }
                }
            };
            match res {
                Ok(op) => {
                    handle.finish();
                    Some(Ok(op))
                }
                Err(err) => {
                    handle.finish();
                    has_error.store(true, Ordering::Relaxed);
                    Some(Err(err))
                }
            }
        });
    }

    let mut download = Box::pin(join_all(futs));
    let mut cancel_sent = false;
    let results = loop {
        tokio::select! {
            _ = &mut resolving => {},
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                download_progress(ui, &prog);
            }
            _ = cancel.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                cancel_download(mgr, settings.elevate, &session).await;
            }
            results = &mut download => break results,
        }
    };
    run_op(
        mgr,
        settings.elevate,
        IpcOperation::EndDownload(session),
        progress_noop(),
    )
    .await?;
    ui.check_cancel()?;
    download_progress(ui, &prog);
    collect_ops(results.into_iter().flatten())
}

/// 任一项失败时返回第一个非取消错误；只有取消时才返回取消。
fn collect_ops<T>(results: impl IntoIterator<Item = anyhow::Result<T>>) -> anyhow::Result<Vec<T>> {
    let mut ops = Vec::new();
    let mut first_err = None;
    for res in results {
        match res {
            Ok(op) => ops.push(op),
            Err(err) => {
                if first_err.as_ref().is_none_or(is_cancelled) {
                    first_err = Some(err);
                }
            }
        }
    }
    match first_err {
        Some(err) => Err(err),
        None => Ok(ops),
    }
}

async fn fallback_merged_files(
    settings: &Settings,
    local_files: &[Embedded],
    disk_files: &[LocalFile],
    latest: &RepoMetadata,
    hash_key: HashKey,
    files: &[FilePos],
    failed: &[usize],
    source_ctx: &SourceCtx,
    staging: &Staging,
    mgr: &ManagedElevate,
    handle: &ProgressHandle,
    has_error: &AtomicBool,
) -> anyhow::Result<IpcOperation> {
    let mut last = None;
    for &i in failed {
        if has_error.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let Some(file) = files.get(i) else {
            continue;
        };
        last = Some(
            install_one(
                settings,
                local_files,
                disk_files,
                latest,
                hash_key,
                &file.item,
                source_ctx,
                staging,
                mgr,
                true,
                Some(handle.only(i)),
            )
            .await?,
        );
    }
    last.ok_or_else(|| Cancelled.into())
}

async fn install_one(
    settings: &Settings,
    local_files: &[Embedded],
    disk_files: &[LocalFile],
    latest: &RepoMetadata,
    hash_key: HashKey,
    item: &FileMeta,
    source_ctx: &SourceCtx,
    staging: &Staging,
    mgr: &ManagedElevate,
    skip_patch_first: bool,
    handle: Option<ProgressHandle>,
) -> anyhow::Result<IpcOperation> {
    let file_name = item.file_name.clone();
    let mut last_err = None;
    let mut first_op = None;
    let mut last_insight = None;
    for attempt in 1..=if skip_patch_first { 2 } else { 3 } {
        let skip_patch = skip_patch_first || attempt > 1;
        let ipc = build_install_op(
            settings,
            local_files,
            disk_files,
            latest,
            hash_key,
            item,
            source_ctx,
            staging,
            skip_patch,
        )
        .await?;
        if first_op.is_none() {
            first_op = Some(ipc.clone());
        }
        if let Some(handle) = &handle {
            handle.prepare(&ipc);
        }
        let mode = mode_from_op(&ipc);
        let result = if let Some(handle) = handle.clone() {
            run_download_op(
                mgr,
                settings.elevate,
                IpcOperation::Download(handle.job.clone(), Box::new(ipc)),
                source_ctx,
                mode,
                handle.callback(),
            )
            .await
        } else {
            run_download_op(
                mgr,
                settings.elevate,
                ipc,
                source_ctx,
                mode,
                progress_noop(),
            )
            .await
        };
        match result {
            Ok((_, insight)) => {
                last_insight = insight;
                log_task(
                    mode.unwrap_or("local"),
                    item.size,
                    &file_name,
                    last_insight.as_ref(),
                    true,
                    None,
                );
                return Ok(first_op.unwrap());
            }
            Err(err)
                if is_cancelled(&err)
                    || matches!(extract(&err), Extracted::Coded(c) if matches!(c.code, DISK_FULL | FILE_IO_FAILED | crate::utils::code::PERMISSION_DENIED)) =>
            {
                return Err(err)
            }
            Err(err) => last_err = Some(err),
        }
    }
    let err = last_err.unwrap_or_else(|| anyhow::Error::from(Coded::bare(FILE_IO_FAILED)));
    log_task(
        "direct",
        item.size,
        &file_name,
        last_insight.as_ref(),
        false,
        Some(&err.to_string()),
    );
    Err(attach_download(err, Some(&file_name), None))
}

struct MergedResult {
    op: IpcOperation,
    failed: Vec<usize>,
}

async fn install_merged(
    settings: &Settings,
    local_files: &[Embedded],
    latest: &RepoMetadata,
    hash_key: HashKey,
    files: &[FilePos],
    range: &str,
    download_size: usize,
    source_ctx: &SourceCtx,
    staging: &Staging,
    mgr: &ManagedElevate,
    handle: ProgressHandle,
) -> anyhow::Result<MergedResult> {
    let chunks: Vec<InstallFileArgs> = files
        .iter()
        .map(|file| InstallFileArgs {
            mode: {
                let source = InstallFileSource::Sliced {
                    parts: vec![(file.offset as u64, file.size as u64)],
                    skip_decompress: false,
                };
                if let Some(patch) = &file.patch {
                    InstallFileMode::Patch {
                        source,
                        diff_size: patch.size as usize,
                    }
                } else {
                    InstallFileMode::Direct(source)
                }
            },
            output_size: file.item.size,
            target: staged_target(staging, &file.item.file_name),
            old: file
                .patch
                .as_ref()
                .map(|_| join_install(&settings.install_path, &file.item.file_name)),
            md5: file.item.md5.clone(),
            xxh: file.item.xxh.clone(),
            clear_installer_index_mark: None,
        })
        .collect();
    let ipc = IpcOperation::InstallMultichunkStream(InstallMultiStreamArgs {
        range: range.to_string(),
        chunks,
    });
    let mode = merged_mode(files, local_files, &latest.patches, hash_key);
    let (value, insight) = run_download_op(
        mgr,
        settings.elevate,
        IpcOperation::Download(handle.job.clone(), Box::new(ipc.clone())),
        source_ctx,
        Some(mode),
        handle.callback(),
    )
    .await?;
    let IpcResult::InstallMultichunkStream(multi) = value else {
        bail!("IPC_SHAPE_ERR");
    };
    let mut failed = Vec::new();
    for (i, res) in multi.results.iter().enumerate() {
        if res.is_err() {
            failed.push(i);
        }
    }
    let names = files
        .iter()
        .map(|f| f.item.file_name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if failed.is_empty() {
        log_task(
            "MERGED",
            download_size as u64,
            &names,
            insight.as_ref(),
            true,
            None,
        );
    } else {
        log_task(
            "MERGED",
            download_size as u64,
            &names,
            insight.as_ref(),
            false,
            Some("merged chunk failed"),
        );
    }
    Ok(MergedResult { op: ipc, failed })
}

async fn build_install_op(
    settings: &Settings,
    local_files: &[Embedded],
    disk_files: &[LocalFile],
    latest: &RepoMetadata,
    hash_key: HashKey,
    item: &FileMeta,
    source_ctx: &SourceCtx,
    staging: &Staging,
    skip_patch: bool,
) -> anyhow::Result<IpcOperation> {
    let target = staged_target(staging, &item.file_name);
    // the file currently on disk, if any: the base for a patch
    let old = find_local(disk_files, &item.file_name)
        .map(|_| join_install(&settings.install_path, &item.file_name));
    let hash = hash_of_item(item, hash_key)
        .ok_or_else(|| anyhow::Error::from(Coded::bare(HASH_ALGORITHM_UNSUPPORTED)))?;
    // the packed installer image carries an index mark that must be cleared
    // once it lands as the app's updater; a self-update is the same case
    let installer = item.installer.unwrap_or(false)
        || is_current_exe(&join_install(&settings.install_path, &item.file_name));
    if !skip_patch {
        if let Some(local) = local_files.iter().find(|l| l.name == hash) {
            return Ok(IpcOperation::InstallFile(InstallFileArgs {
                mode: InstallFileMode::Direct(InstallFileSource::Local {
                    offset: local.offset,
                    size: local.size,
                    skip_decompress: false,
                }),
                target,
                output_size: item.size,
                old: None,
                md5: item.md5.clone(),
                xxh: item.xxh.clone(),
                clear_installer_index_mark: Some(installer),
            }));
        }

        let lpatch = latest.patches.iter().find(|p| {
            side_hash(&p.to, hash_key) == Some(hash.as_str())
                && side_hash(&p.from, hash_key)
                    .is_some_and(|from| local_files.iter().any(|l| l.name == from))
        });
        if let Some(patch) = lpatch {
            if let Some(from) = side_hash(&patch.from, hash_key) {
                if let Some(local) = local_files.iter().find(|l| l.name == from) {
                    let loc = resolve_install_source(
                        source_ctx,
                        &format!("{from}_{hash}"),
                        settings.dfs_extras.as_deref(),
                        false,
                        true,
                    )
                    .await?;
                    return Ok(hybrid_op(
                        local,
                        loc,
                        &target,
                        item,
                        patch.size as usize,
                        patch.from.size,
                    ));
                }
            }
        }

        let disk_hash = find_local(disk_files, &item.file_name).map(|l| l.hash.as_str());
        let patch = latest.patches.iter().find(|p| {
            side_hash(&p.to, hash_key) == Some(hash.as_str())
                && side_hash(&p.from, hash_key) == disk_hash
        });
        if let Some(patch) = patch {
            if let Some(from) = side_hash(&patch.from, hash_key) {
                if let Ok(loc) = resolve_install_source(
                    source_ctx,
                    &format!("{from}_{hash}"),
                    settings.dfs_extras.as_deref(),
                    false,
                    true,
                )
                .await
                {
                    return Ok(url_op(
                        loc,
                        &target,
                        old,
                        item,
                        Some(patch.size as usize),
                        installer,
                    ));
                }
            }
        }
    }

    let loc = resolve_install_source(
        source_ctx,
        &hash,
        settings.dfs_extras.as_deref(),
        installer,
        !skip_patch,
    )
    .await?;
    Ok(url_op(loc, &target, None, item, None, installer))
}

async fn resolve_install_source(
    ctx: &SourceCtx,
    hash: &str,
    extras: Option<&str>,
    installer: bool,
    slice: bool,
) -> anyhow::Result<InstallFileSource> {
    if let Some(entry) = ctx.find(hash).filter(|entry| entry.size > 0) {
        let range = super::download_plan::Range {
            start: entry.offset as u64,
            len: entry.size as u64,
        };
        let parts = if slice {
            ctx.policy.split(range)
        } else {
            vec![range]
        };
        return Ok(InstallFileSource::Sliced {
            parts: parts.into_iter().map(|p| (p.start, p.len)).collect(),
            skip_decompress: false,
        });
    }
    if installer && ctx.has_dfs2_session() && ctx.installer_end > 0 {
        return Ok(InstallFileSource::Sliced {
            parts: vec![(0, ctx.installer_end as u64)],
            skip_decompress: true,
        });
    }
    Ok(file_source(
        resolve_file_location(ctx, hash, extras, installer).await?,
    ))
}

fn side_hash(side: &crate::utils::metadata::PatchSide, key: HashKey) -> Option<&str> {
    match key {
        HashKey::Md5 => side.md5.as_deref(),
        HashKey::Xxh => side.xxh.as_deref(),
    }
}

/// Staged output path for a managed file (`new\<rel>` under the staging root).
fn staged_target(staging: &Staging, file_name: &str) -> String {
    staging.new_path(file_name).to_string_lossy().to_string()
}

fn is_current_exe(path: &str) -> bool {
    std::env::current_exe().is_ok_and(|exe| {
        crate::session::plan::normalize_full(&exe.to_string_lossy())
            == crate::session::plan::normalize_full(path)
    })
}

fn file_source(loc: FileLocation) -> InstallFileSource {
    if let Some(url) = loc.url {
        InstallFileSource::Url {
            url,
            offset: loc.offset,
            size: loc.size,
            skip_decompress: loc.skip_decompress,
            request_range: loc.request_range,
        }
    } else {
        InstallFileSource::Local {
            offset: loc.offset,
            size: loc.size,
            skip_decompress: loc.skip_decompress,
        }
    }
}

fn url_op(
    source: InstallFileSource,
    target: &str,
    old: Option<String>,
    item: &FileMeta,
    diff_size: Option<usize>,
    installer: bool,
) -> IpcOperation {
    let mode = if let Some(diff_size) = diff_size {
        InstallFileMode::Patch { source, diff_size }
    } else {
        InstallFileMode::Direct(source)
    };
    IpcOperation::InstallFile(InstallFileArgs {
        mode,
        target: target.to_string(),
        output_size: item.size,
        old,
        md5: item.md5.clone(),
        xxh: item.xxh.clone(),
        clear_installer_index_mark: Some(installer),
    })
}

fn hybrid_op(
    local: &Embedded,
    source: InstallFileSource,
    target: &str,
    item: &FileMeta,
    diff_size: usize,
    base_size: u64,
) -> IpcOperation {
    IpcOperation::InstallFile(InstallFileArgs {
        mode: InstallFileMode::HybridPatch {
            diff_size,
            base_size,
            diff: source,
            source: InstallFileSource::Local {
                offset: local.offset,
                size: local.size,
                skip_decompress: false,
            },
        },
        target: target.to_string(),
        output_size: item.size,
        old: None,
        md5: item.md5.clone(),
        xxh: item.xxh.clone(),
        clear_installer_index_mark: None,
    })
}

async fn install_runtimes(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    staging: &Staging,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) {
    let Some(runtimes) = project.runtimes.as_ref() else {
        return;
    };
    let dl_dir = staging.dl_dir().to_string_lossy().to_string();
    tracing::info!("latest_meta.runtimes {runtimes:?}");
    for tag in runtimes {
        tracing::info!("Installing runtime: {tag}");
        let embed = config
            .embedded_files
            .as_ref()
            .and_then(|files| files.iter().find(|e| e.name == *tag));
        let name = runtime_name(tag);
        progress(
            ui,
            3,
            96.0,
            ProgressStage::InstallRuntime,
            Some(name),
            None,
            None,
        );
        let mut last_err = None;
        for _ in 0..3 {
            progress(
                ui,
                3,
                96.0,
                ProgressStage::DownloadRuntime,
                Some(name),
                None,
                None,
            );
            let res = run_op_with_ui(
                mgr,
                settings.elevate,
                IpcOperation::InstallRuntime {
                    tag: tag.clone(),
                    offset: embed.map(|e| e.offset),
                    size: embed.map(|e| e.size),
                    dl_dir: dl_dir.clone(),
                },
                ui,
                |ui, p| match p {
                    Progress::BytesOf { done, total } => progress(
                        ui,
                        3,
                        96.0,
                        ProgressStage::DownloadRuntime,
                        Some(name),
                        Some(*done),
                        (*total > 0).then_some(*total),
                    ),
                    Progress::Stage(ProgressStage::InstallRuntime) => progress(
                        ui,
                        3,
                        96.0,
                        ProgressStage::InstallRuntime,
                        Some(name),
                        None,
                        None,
                    ),
                    _ => {}
                },
            )
            .await;
            match res {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(err) => {
                    tracing::info!("runtime {name} failed: {err:#}, retrying");
                    last_err = Some(err);
                }
            }
        }
        if let Some(err) = last_err {
            tracing::error!("runtime {name} failed: {err:#}");
            notify_error(ui, err.attach_with(RUNTIME_INSTALL_FAILED, name));
        }
    }
}

async fn write_registration(
    settings: &Settings,
    project: &ProjectConfig,
    latest: Option<&RepoMetadata>,
    partial_version: Option<&str>,
    exe_path: &str,
    uninstaller_path: &str,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) {
    use crate::installer::registry::{remove_record, write_at_hive, Manifest, RegHive};

    if settings.reg_hives.is_empty() {
        return;
    }
    let version = latest
        .map(|m| m.tag_name.as_str())
        .or(partial_version)
        .unwrap_or("");
    if latest.is_none() {
        tracing::info!("registry manifest not refreshed; keeping existing InstallerMeta");
    }
    progress(ui, 3, 99.0, ProgressStage::WriteRegistry, None, None, None);
    let manifest = latest.map(|m| Manifest {
        metadata: serde_json::to_string(m).unwrap_or_default(),
        size: m.hashed.iter().map(|e| e.size).sum(),
    });
    let params = WriteRegistryParams {
        reg_name: project.reg_name.clone(),
        name: project.app_name.clone(),
        version: version.to_string(),
        exe: exe_path.to_string(),
        source: settings.install_path.clone(),
        uninstaller: uninstaller_path.to_string(),
        publisher: project.publisher.clone(),
        manifest,
    };
    let mut hklm_written = false;
    for &hive in &settings.reg_hives {
        let result = match hive {
            RegHive::Hkcu => write_at_hive(hive, &params),
            RegHive::Hklm => run_op(
                mgr,
                true,
                IpcOperation::WriteRegistry(params.clone()),
                progress_noop(),
            )
            .await
            .map(|_| ()),
        };
        match result {
            Ok(()) => hklm_written |= hive == RegHive::Hklm,
            Err(err) => {
                tracing::warn!("write registry failed: {err:#}");
                notify_error(ui, err.attach(REGISTRY_WRITE_FAILED));
            }
        }
    }
    if settings.reg_drop_hkcu && hklm_written {
        if let Err(err) = remove_record(RegHive::Hkcu, &project.reg_name) {
            tracing::warn!("remove migrated HKCU record failed: {err:#}");
        }
    }
}

async fn finish_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    latest: Option<&RepoMetadata>,
    partial_version: Option<&str>,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) {
    let exe_path = join_install(&settings.install_path, &project.exe_name);
    // the uninstaller itself was swapped in with the commit (see
    // `self_image_units`); only its shortcut is made here when this session
    // writes an uninstall record.
    let uninstaller_path = join_install(&settings.install_path, &project.uninstall_name);
    progress(
        ui,
        3,
        98.0,
        ProgressStage::CreateShortcuts,
        None,
        None,
        None,
    );
    match get_dirs(settings.elevate).await.into_anyhow() {
        Ok(dirs) => {
            create_shortcuts(
                settings,
                project,
                dirs,
                &exe_path,
                &uninstaller_path,
                ui,
                mgr,
            )
            .await
        }
        Err(err) => {
            tracing::warn!("shortcut folders unavailable: {err:#}");
            notify_error(ui, err.attach(SHORTCUT_FAILED));
        }
    }
    write_registration(
        settings,
        project,
        latest,
        partial_version,
        &exe_path,
        &uninstaller_path,
        ui,
        mgr,
    )
    .await;
    emit_insight(project, settings, config, "finish", None, false);
}

async fn create_shortcuts(
    settings: &Settings,
    project: &ProjectConfig,
    (program, desktop): (String, String),
    exe_path: &str,
    uninstaller_path: &str,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) {
    let program_lnk = format!(
        "{}\\{}\\{}.lnk",
        program, project.app_name, project.app_name
    );
    let desktop_lnk = format!("{}\\{}.lnk", desktop, project.app_name);
    let uninstall_name = crate::utils::i18n::t(
        "shortcut.uninstall",
        &[("subject", project.app_name.as_str())],
    );
    let uninstall_lnk = format!("{}\\{}\\{}.lnk", program, project.app_name, uninstall_name);
    if settings.create_lnk && !settings.is_update {
        create_lnk_or_notify(
            mgr,
            settings.elevate,
            CreateLnkArgs {
                target: exe_path.to_string(),
                lnk: desktop_lnk,
            },
            ui,
        )
        .await;
    }
    if !settings.is_update {
        create_lnk_or_notify(
            mgr,
            settings.elevate,
            CreateLnkArgs {
                target: exe_path.to_string(),
                lnk: program_lnk,
            },
            ui,
        )
        .await;
    }
    if !settings.reg_hives.is_empty() && std::path::Path::new(uninstaller_path).is_file() {
        create_lnk_or_notify(
            mgr,
            settings.elevate,
            CreateLnkArgs {
                target: uninstaller_path.to_string(),
                lnk: uninstall_lnk,
            },
            ui,
        )
        .await;
    }
}

async fn run_mirrorc(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    let cdk = settings
        .mirrorc_cdk
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Coded::bare(MIRRORC_CDK_MISSING))?; // GUI already prompts for CDK before start
    let parsed = parse_source(&settings.source_uri)?;
    let ParsedSource::Mirrorc {
        resource_id,
        channel,
        arch,
        os,
    } = parsed
    else {
        return Err(anyhow::Error::from(Coded::bare_with(
            MIRRORC_CONFIG_INVALID,
            settings.source_uri.clone(),
        )));
    };
    progress(ui, 0, 2.0, ProgressStage::FetchMetadata, None, None, None);
    let current_version = win32_version_info::VersionInfo::from_file(join_install(
        &settings.install_path,
        &project.exe_name,
    ))
    .map(|v| v.product_version)
    .unwrap_or_default();
    let status = get_mirrorc_status(
        &resource_id,
        &current_version,
        cdk,
        &channel,
        arch.as_deref(),
        os.as_deref(),
    )
    .await
    .into_anyhow()
    .map_err(|e| e.attach(MIRRORC_UNREACHABLE))?;
    if let Some(coded) = coded_for_mirrorc_response(&status) {
        return Err(anyhow::Error::from(coded));
    }
    let version_name = status
        .pointer("/data/version_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    tracing::info!("Mirrorc source version {current_version}");
    tracing::info!("Mirrorc target version {version_name}");
    tracing::info!(
        "Mirrorc update mode {:?}",
        status.pointer("/data/update_type")
    );
    let url = status
        .pointer("/data/url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let sha256 = status
        .pointer("/data/sha256")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());

    let (staged, journal) = open_staging(settings, mgr).await?;
    let (staged, recovered_self) = match journal {
        Some(text) => {
            recover_or_discard(
                settings,
                project,
                staged,
                text,
                "md5",
                &std::collections::HashMap::new(),
                &std::collections::HashSet::new(),
                sha256.as_deref(),
                ui,
                mgr,
            )
            .await?
        }
        None => (staged, false),
    };
    if version_name == current_version {
        tracing::info!("already latest, tag={version_name}");
        finish_staging(&staged, recovered_self, mgr).await;
        finish_install(
            settings,
            config,
            project,
            None,
            Some(&version_name),
            ui,
            mgr,
        )
        .await;
        return Ok(SessionResult::install(true, settings.is_update));
    }
    emit_insight(
        project,
        settings,
        config,
        &prepare_event(
            settings,
            config,
            &source_id(project, &settings.source_uri),
            &version_name,
            true,
        ),
        None,
        false,
    );
    if !prepare_process(settings, project, ui, mgr, &version_name).await? {
        finish_staging(&staged, recovered_self, mgr).await;
        return Ok(SessionResult::cancelled(settings.is_update));
    }
    if ui.check_cancel().is_err() {
        finish_staging(&staged, recovered_self, mgr).await;
        return Err(anyhow::Error::new(Cancelled));
    }
    let (Some(url), Some(sha256)) = (url, sha256) else {
        finish_staging(&staged, recovered_self, mgr).await;
        return Err(anyhow::Error::from(Coded::bare(MIRRORC_FAILED)));
    };
    tracing::info!("Mirrorc URL {url}");

    let result = mirrorc_staged(
        settings,
        config,
        project,
        ui,
        mgr,
        &staged,
        &url,
        &sha256,
        &version_name,
    )
    .await;
    let self_replaced = recovered_self || matches!(result, Ok((_, true)));
    finish_staging(&staged, self_replaced, mgr).await;
    result.map(|(r, _)| r)
}

#[allow(clippy::too_many_arguments)]
async fn mirrorc_staged(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
    staged: &SessionStaging,
    url: &str,
    sha256: &str,
    version_name: &str,
) -> anyhow::Result<(SessionResult, bool)> {
    let zip_path = staged
        .staging
        .dl_dir()
        .join(format!("{sha256}.zip"))
        .to_string_lossy()
        .to_string();
    progress(ui, 1, 5.0, ProgressStage::PrepareDownload, None, None, None);
    run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::RunMirrorcDownload {
            url: url.to_string(),
            zip_path: zip_path.clone(),
        },
        ui,
        |ui, p| {
            if let Progress::BytesOf {
                done: downloaded,
                total,
            } = p
            {
                let total = *total;
                progress(
                    ui,
                    1,
                    5.0 + if total == 0 {
                        0.0
                    } else {
                        *downloaded as f64 / total.max(1) as f64 * 65.0
                    },
                    ProgressStage::DownloadArchive,
                    None,
                    Some(*downloaded),
                    (total > 0).then_some(total),
                );
            }
        },
    )
    .await?;
    ui.check_cancel()?;
    progress(ui, 2, 70.0, ProgressStage::VerifyArchive, None, None, None);
    let new_dir = staged.staging.new_dir().to_string_lossy().to_string();
    let installed = run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::RunMirrorcInstall {
            zip_path,
            new_dir,
            sha256: sha256.to_string(),
        },
        ui,
        |ui, p| {
            if let Progress::Extract {
                file,
                done: count,
                total,
            } = p
            {
                let total = *total;
                progress(
                    ui,
                    2,
                    70.0 + (*count as f64 / total.max(1) as f64) * 25.0,
                    ProgressStage::ExtractArchive,
                    Some(file),
                    Some(*count),
                    Some(total),
                );
            }
        },
    )
    .await?;
    let IpcResult::RunMirrorcInstall(extracted) = installed else {
        bail!("IPC_SHAPE_ERR");
    };
    let meta: Option<RepoMetadata> = match extracted.metadata.as_deref() {
        Some(text) => Some(serde_json::from_str(text).map_err(|e| attach_metadata(e.into()))?),
        None => None,
    };
    ui.check_cancel()?;

    // a package landing in a missing or empty directory is one root unit;
    // anything else is swapped file by file (no per-file metadata to judge
    // directories by)
    let install_dir = std::path::Path::new(&settings.install_path);
    let root_unit = !install_dir.exists()
        || std::fs::read_dir(install_dir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
    let files: Vec<FileEntry> = extracted
        .files
        .iter()
        .map(|(rel, hash)| FileEntry {
            rel: rel.clone(),
            old: None,
            new: hash.clone(),
        })
        .collect();
    let updater_rel = normalize_rel(&project.updater_name);
    let archive_has_updater = files.iter().any(|f| normalize_rel(&f.rel) == updater_rel);
    let self_units = self_image_units(
        settings,
        project,
        staged,
        "md5",
        archive_has_updater,
        archive_has_updater,
        mgr,
    )
    .await?;
    let mut units = Vec::new();
    if root_unit {
        units.push(Unit::Dir {
            rel: String::new(),
            files,
        });
    } else {
        units.extend(files.into_iter().map(Unit::File));
        units.extend(extracted.deletes.iter().map(|rel| Unit::Del {
            rel: rel.clone(),
            old: None,
        }));
    }
    let units = merge_self_units(units, self_units);
    let self_replaced = commit_staged(
        settings,
        staged,
        Journal {
            hash_algorithm: "md5".into(),
            archive: Some(sha256.to_string()),
            units,
        },
        ui,
        mgr,
    )
    .await?;
    install_runtimes(settings, config, project, &staged.staging, ui, mgr).await;
    finish_install(
        settings,
        config,
        project,
        meta.as_ref(),
        Some(version_name),
        ui,
        mgr,
    )
    .await;
    Ok((
        SessionResult::install(false, settings.is_update),
        self_replaced,
    ))
}

pub async fn run_uninstall(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    base: &UiState,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    let ui = LiveUi::new(ui, base);
    let txn = crate::utils::sentry::Transaction::start("uninstall", "session");
    emit_insight(project, settings, config, "uninstall", None, true);
    let result = run_uninstall_inner(settings, config, project, &ui, mgr).await;
    txn.finish(txn_status(&result));
    if let Err(err) = &result {
        emit_insight(
            project,
            settings,
            config,
            "fail",
            Some(json!({ "kind": fail_kind(err) })),
            true,
        );
    }
    result
}

/// Hashed files, leftover deletes, and the updater. Paths under `keep_under`
/// (expanded user-data directories) stay unless the user asked to delete them.
fn uninstall_files(
    hashed: impl IntoIterator<Item = String>,
    deletes: impl IntoIterator<Item = String>,
    updater: &str,
    install_path: &str,
    keep_under: &[String],
) -> Vec<String> {
    let mut files: Vec<String> = hashed.into_iter().collect();
    files.extend(deletes);
    files.push(updater.to_string());
    files.retain(|f| {
        keep_under.is_empty() || {
            let full = join_install(install_path, f);
            !keep_under.iter().any(|dir| is_under(&full, dir))
        }
    });
    files.sort();
    files.dedup();
    files
}

/// 卸载只需要注册表元数据里的这两个字段，用窄投影而非直接解 `RepoMetadata`：后者的
/// `tag_name`/`hashed` 是必填的，缺字段即整体报错，而卸载是最不该硬失败的路径——
/// 旧版或被手工改过的注册表项也应当至少能删掉 updater。
#[derive(serde::Deserialize, Default)]
struct UninstallMeta {
    #[serde(default)]
    hashed: Vec<FileMeta>,
    #[serde(default)]
    deletes: Vec<String>,
}

async fn run_uninstall_inner(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    log_session_start("uninstall", settings, config, project);
    ensure_helper_sees_path(settings, mgr)?;
    progress(ui, 0, 10.0, ProgressStage::UninstallScan, None, None, None);
    let matched =
        settings
            .identity
            .matched(&settings.install_path, &project.exe_name, &project.reg_name);
    let meta = settings
        .identity
        .matched_meta(&settings.install_path, &project.exe_name, &project.reg_name)
        .ok_or_else(|| anyhow::Error::from(Coded::bare(UNINSTALL_INFO_MISSING)))?;
    tracing::info!("UNINSTALL_METADATA: {meta}");
    let meta: UninstallMeta = serde_json::from_str(&meta).unwrap_or_default();
    let user_data_dirs: Vec<String> = project
        .user_data_path
        .iter()
        .map(|p| settings.expand(p, &project.app_name))
        .collect();
    let keep_user_data = if settings.delete_user_data {
        Vec::new()
    } else {
        user_data_dirs.clone()
    };
    let files = uninstall_files(
        meta.hashed.into_iter().map(|e| e.file_name),
        meta.deletes,
        &project.updater_name,
        &settings.install_path,
        &keep_user_data,
    );
    tracing::info!("uninstall files: {}", files.join(", "));
    let user_data = if settings.delete_user_data {
        user_data_dirs
    } else {
        Vec::new()
    };
    let shortcuts = |(program, desktop): (String, String)| {
        [
            format!("{}\\{}", program, project.app_name),
            format!("{}\\{}.lnk", desktop, project.app_name),
        ]
    };
    // A record migrated from per-user to machine scope leaves the per-user
    // shortcuts behind; the launching user removes those, the helper the
    // machine-wide ones.
    let user_shortcuts = shortcuts(get_dirs(false).await.into_anyhow()?);
    let mut extra: Vec<String> = project
        .extra_uninstall_path
        .iter()
        .map(|p| settings.expand(p, &project.app_name))
        .collect();
    if settings.elevate {
        extra.extend(shortcuts(get_dirs(true).await.into_anyhow()?));
    } else {
        extra.extend(user_shortcuts.iter().cloned());
    }
    if settings.elevate {
        let _ = run_op(mgr, true, IpcOperation::Ping, progress_noop()).await;
    }
    progress(
        ui,
        1,
        40.0,
        ProgressStage::UninstallDelete,
        None,
        None,
        None,
    );
    let raw = run_op(
        mgr,
        settings.elevate,
        IpcOperation::RunUninstall(RunUninstallArgs {
            source: settings.install_path.clone(),
            files,
            user_data_path: user_data,
            extra_uninstall_path: extra,
            uninstall_name: project.uninstall_name.clone(),
        }),
        progress_noop(),
    )
    .await?;
    let IpcResult::RunUninstall(outcome) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    if let Some(root) = outcome.self_moved_to {
        schedule_cleanup(mgr, settings.elevate, root).await;
    }
    // Once removal has started, failures are collected and the uninstall
    // runs to completion: the record goes too, leftovers are removed by hand.
    let mut errors = outcome.errors;
    if settings.elevate {
        errors.extend(remove_paths(&user_shortcuts).await);
    }
    use crate::installer::registry::{remove_record, RegHive};
    for hive in matched {
        let result = match hive {
            RegHive::Hkcu => remove_record(hive, &project.reg_name),
            RegHive::Hklm => run_op(
                mgr,
                true,
                IpcOperation::RemoveRegistry(project.reg_name.clone()),
                progress_noop(),
            )
            .await
            .map(|_| ()),
        };
        if let Err(err) = result {
            errors.push(format!("remove registry failed: {err:#}"));
        }
    }
    if !errors.is_empty() {
        for err in &errors {
            tracing::warn!("uninstall: {err}");
        }
        notify_error(
            ui,
            anyhow::anyhow!(errors.join("\n")).attach(UNINSTALL_INCOMPLETE),
        );
    }
    let _ = config;
    Ok(SessionResult::uninstall())
}

pub async fn silent_main(args: crate::cli::arg::InstallArgs) -> anyhow::Result<()> {
    crate::fs::staging::enter_neutral_cwd()?;
    let config = crate::installer::config::resolve_installer_config(args.clone(), true).await?;
    let project = match config.embedded_config.as_ref() {
        Some(value) => ProjectConfig::from_value(value)?,
        None => {
            tracing::error!(
                "embedded config missing (embedded_files={})",
                config.embedded_files.as_ref().map(|f| f.len()).unwrap_or(0)
            );
            return Err(anyhow::Error::from(Coded::bare(PKG_BROKEN)));
        }
    };
    let mut settings = crate::session::types::settings_from_cli(&args, &config, &project).await?;
    if config.is_uninstall || args.uninstall {
        let ui = crate::session::ui::SilentUi;
        settings.install_path = if args.target.is_some() {
            settings.install_path
        } else {
            config.install_path.clone()
        };
        let inspected =
            crate::installer::inspect_dir(settings.install_path.clone(), project.exe_name.clone())
                .await;
        if let Some(dir) = inspected {
            settings.elevate =
                crate::session::types::elevate_from_state(&dir.state, &project.uac_strategy);
            settings.is_update = dir.upgrade;
        }
        crate::session::types::apply_registry(&mut settings, &config.install_path, &project)?;
        let mgr = ManagedElevate::new();
        let result =
            run_uninstall(&settings, &config, &project, &ui, &UiState::default(), &mgr).await;
        mgr.close().await;
        return result.map(|_| ());
    }
    crate::session::types::apply_registry(&mut settings, &config.install_path, &project)?;
    if needs_js_plugin(&settings.source_uri) {
        if crate::host::webview_version().is_err() {
            return Err(anyhow::Error::from(Coded::bare(WEBVIEW2_REQUIRED)));
        }
        let session = SessionState::default();
        let runtime = crate::host::spawn_plugin_runtime(args.clone(), session.clone()).await?;
        let ui = SilentPluginUi::new(runtime.handle().clone(), session.plugins.clone());
        let result = silent_install(&settings, &config, &project, &ui).await;
        runtime.close();
        return result;
    }
    silent_install(&settings, &config, &project, &crate::session::ui::SilentUi).await
}

async fn silent_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
) -> anyhow::Result<()> {
    let mgr = ManagedElevate::new();
    let result = run_install(settings, config, project, ui, &UiState::default(), &mgr).await;
    mgr.close().await;
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::plan::PlanFile;

    fn meta(name: &str, md5: &str) -> FileMeta {
        FileMeta {
            file_name: name.into(),
            size: 1,
            md5: Some(md5.into()),
            xxh: None,
            installer: None,
        }
    }

    fn plan_file(name: &str, action: PlanAction, old: Option<&str>) -> PlanFile {
        PlanFile {
            file_name: name.into(),
            action,
            skip_reason: None,
            old_hash: old.map(str::to_string),
            unwritable: false,
            has_patch: false,
            has_lpatch: false,
        }
    }

    fn rels(units: &[Unit]) -> Vec<String> {
        units
            .iter()
            .map(|u| match u {
                Unit::Dir { rel, files } => format!("dir:{rel}[{}]", files.len()),
                Unit::File(f) => format!("file:{}", f.rel),
                Unit::Copy(f) => format!("copy:{}", f.rel),
                Unit::Del { rel, .. } => format!("del:{rel}"),
            })
            .collect()
    }

    #[test]
    fn units_root_dir_on_fresh_install() {
        let hashed = vec![meta("app.exe", "a"), meta("lib/x.dll", "x")];
        let plan = InstallPlan {
            files: vec![
                plan_file("app.exe", PlanAction::Install, None),
                plan_file("lib/x.dll", PlanAction::Install, None),
            ],
            deletes: vec!["old.txt".into()],
        };
        let units = build_units(&plan, &hashed, HashKey::Md5, &[], &LocalScan::default());
        // the whole install is one root unit; a delete under it is moot
        assert_eq!(rels(&units), vec!["dir:[2]".to_string()]);
    }

    #[test]
    fn units_deletes_when_nothing_to_install() {
        let hashed = vec![meta("app.exe", "a")];
        let plan = InstallPlan {
            files: vec![plan_file("app.exe", PlanAction::Skip, Some("a"))],
            deletes: vec!["old.txt".into()],
        };
        let units = build_units(&plan, &hashed, HashKey::Md5, &[], &LocalScan::default());
        assert_eq!(rels(&units), vec!["del:old.txt".to_string()]);
    }

    #[test]
    fn collect_ops_prefers_the_real_error_over_cancellation() {
        let results: Vec<anyhow::Result<u8>> = vec![
            Err(Cancelled.into()),
            Err(Coded::bare(FILE_IO_FAILED).into()),
            Err(Cancelled.into()),
            Ok(1),
        ];
        let err = collect_ops(results).unwrap_err();
        assert!(matches!(extract(&err), Extracted::Coded(c) if c.code == FILE_IO_FAILED));

        let only_cancel: Vec<anyhow::Result<u8>> = vec![Ok(1), Err(Cancelled.into())];
        assert!(is_cancelled(&collect_ops(only_cancel).unwrap_err()));
    }

    #[test]
    fn uninstall_files_keeps_paths_under_user_data() {
        let files = uninstall_files(
            [
                "app.exe".into(),
                "User/save.dat".into(),
                "User/deep/x.bin".into(),
            ],
            ["User/old.dat".into(), "gone.dll".into()],
            "updater.exe",
            r"C:\Games\App",
            &[r"C:\Games\App\User".into()],
        );
        assert_eq!(files, ["app.exe", "gone.dll", "updater.exe"]);
    }

    #[test]
    fn uninstall_files_includes_user_data_when_not_kept() {
        let files = uninstall_files(
            ["app.exe".into(), "User/save.dat".into()],
            Vec::<String>::new(),
            "updater.exe",
            r"C:\Games\App",
            &[],
        );
        assert_eq!(files, ["User/save.dat", "app.exe", "updater.exe"]);
    }

    #[test]
    fn units_subdir_when_root_is_dirty_or_partly_unchanged() {
        let hashed = vec![
            meta("app.exe", "a"),
            meta("lib/x.dll", "x"),
            meta("lib/y.dll", "y"),
            meta("plugins/p.dll", "p"),
        ];
        let local = vec![LocalFile {
            file_name: "app.exe".into(),
            hash: "a".into(),
            size: 1,
            unwritable: false,
        }];
        let plan = InstallPlan {
            files: vec![
                plan_file("app.exe", PlanAction::Skip, Some("a")),
                plan_file("lib/x.dll", PlanAction::Install, Some("x0")),
                plan_file("lib/y.dll", PlanAction::Install, None),
                plan_file("plugins/p.dll", PlanAction::Install, None),
            ],
            deletes: vec!["lib/gone.dll".into(), "plugins/old.dll".into()],
        };
        let scan = LocalScan {
            files: vec![],
            dirty_dirs: vec![String::new(), "plugins".into()],
            reparse_dirs: vec![],
        };
        let units = build_units(&plan, &hashed, HashKey::Md5, &local, &scan);
        assert_eq!(
            rels(&units),
            vec![
                "dir:lib[2]".to_string(),
                "file:plugins/p.dll".to_string(),
                "del:plugins/old.dll".to_string(),
            ]
        );
        let Unit::Dir { files, .. } = &units[0] else {
            panic!()
        };
        assert_eq!(files[0].old.as_deref(), Some("x0"));
        assert_eq!(files[1].old, None);
    }

    fn test_settings(install: &std::path::Path, is_update: bool) -> Settings {
        Settings {
            install_path: install.to_string_lossy().to_string(),
            source_uri: String::new(),
            create_lnk: false,
            delete_user_data: false,
            mirrorc_cdk: None,
            online: false,
            silent: true,
            non_interactive: true,
            dump_dir: None,
            dfs_extras: None,
            elevate: false,
            is_update,
            auto_answer: true,
            reg_hives: Vec::new(),
            reg_drop_hkcu: false,
            identity: crate::installer::registry::Identity::default(),
        }
    }

    #[test]
    fn self_image_plan_follows_session_kind() {
        let base =
            crate::fs::staging::scratch_file(&format!("kachina-selfimg-{}", uuid::Uuid::new_v4()));
        let install = base.join("app");
        std::fs::create_dir_all(&install).unwrap();
        let staging = Staging::at(base.join("staged"));
        let project = ProjectConfig::from_value(&json!({
            "source": "https://x/meta.json",
            "appName": "App",
            "publisher": "P",
            "regName": "App",
            "exeName": "app.exe",
            "uninstallName": "uninst.exe",
            "updaterName": "updater.exe",
            "programFilesPath": "App",
            "title": "t",
            "description": "d",
            "windowTitle": "w",
            "runtimes": null,
            "windowBorderless": null
        }))
        .unwrap();
        let names = |p: Option<SelfImagePlan>| p.map(|p| (p.names, p.copy_from));

        // fresh install: both from self
        let p = names(self_image_plan(
            &test_settings(&install, false),
            &project,
            &staging,
            false,
            false,
        ));
        assert_eq!(
            p,
            Some((vec!["uninst.exe".into(), "updater.exe".into()], None))
        );

        // update, nothing shipped, foreign installer, no uninstaller on disk: updater only
        let p = names(self_image_plan(
            &test_settings(&install, true),
            &project,
            &staging,
            false,
            false,
        ));
        assert_eq!(p, Some((vec!["updater.exe".into()], None)));

        // ... with an uninstaller present it is refreshed too
        std::fs::write(install.join("uninst.exe"), b"old").unwrap();
        let p = names(self_image_plan(
            &test_settings(&install, true),
            &project,
            &staging,
            false,
            false,
        ));
        assert_eq!(
            p,
            Some((vec!["updater.exe".into(), "uninst.exe".into()], None))
        );

        // update, updater shipped and staged: uninstaller copied from the staged updater
        let p = names(self_image_plan(
            &test_settings(&install, true),
            &project,
            &staging,
            true,
            true,
        ));
        assert_eq!(
            p,
            Some((
                vec!["uninst.exe".into()],
                Some(staged_target(&staging, "updater.exe"))
            ))
        );
        // ... shipped but unchanged on disk: copied from the installed updater
        let p = names(self_image_plan(
            &test_settings(&install, true),
            &project,
            &staging,
            true,
            false,
        ));
        assert_eq!(
            p,
            Some((
                vec!["uninst.exe".into()],
                Some(join_install(&install.to_string_lossy(), "updater.exe"))
            ))
        );
        // ... shipped, no uninstaller on disk: nothing to do
        std::fs::remove_file(install.join("uninst.exe")).unwrap();
        assert!(self_image_plan(
            &test_settings(&install, true),
            &project,
            &staging,
            true,
            true
        )
        .is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn self_units_fold_into_root_unit_or_append() {
        let selfs = || {
            vec![Unit::File(FileEntry {
                rel: "uninst.exe".into(),
                old: None,
                new: "u".into(),
            })]
        };
        let root = vec![Unit::Dir {
            rel: String::new(),
            files: vec![FileEntry {
                rel: "app.exe".into(),
                old: None,
                new: "a".into(),
            }],
        }];
        let merged = merge_self_units(root, selfs());
        assert_eq!(merged.len(), 1);
        let Unit::Dir { files, .. } = &merged[0] else {
            panic!()
        };
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.rel == "uninst.exe"));

        let flat = vec![Unit::File(FileEntry {
            rel: "app.exe".into(),
            old: None,
            new: "a".into(),
        })];
        let merged = merge_self_units(flat, selfs());
        assert_eq!(
            rels(&merged),
            vec!["file:app.exe".to_string(), "file:uninst.exe".to_string()]
        );
    }

    #[test]
    fn units_copy_under_reparse_point() {
        let hashed = vec![meta("link/f.dll", "f"), meta("app.exe", "a")];
        let plan = InstallPlan {
            files: vec![
                plan_file("link/f.dll", PlanAction::Install, None),
                plan_file("app.exe", PlanAction::Install, None),
            ],
            deletes: vec![],
        };
        let scan = LocalScan {
            files: vec![],
            dirty_dirs: vec![String::new()],
            reparse_dirs: vec!["link".into()],
        };
        let units = build_units(&plan, &hashed, HashKey::Md5, &[], &scan);
        assert_eq!(
            rels(&units),
            vec!["file:app.exe".to_string(), "copy:link/f.dll".to_string()]
        );
    }
    #[test]
    fn file_progress_keeps_attempt_work_and_ignores_late_snapshots() {
        let tasks = vec![InstallTask::Single(meta("app.exe", "h"))];
        let prog = Arc::new(Mutex::new(DownloadProg::from_tasks(
            &tasks,
            &[],
            &[],
            HashKey::Md5,
        )));
        let handle = ProgressHandle {
            inner: prog.clone(),
            ids: vec![0],
            job: crate::ipc::download::Job {
                session: "test".into(),
                large: true,
            },
        };
        let first = handle.callback();
        let reporter = crate::ipc::file_progress::Reporter::new(20, first.clone());
        reporter.action(FileAction::Patch, Some(20));
        reporter.bytes(10);
        first(Progress::Network(crate::ipc::network::Snapshot {
            bytes: u32::MAX as u64 + 5,
            active: 1,
        }));
        first(Progress::NetworkFinal(crate::ipc::network::Snapshot {
            bytes: u32::MAX as u64 + 8,
            active: 0,
        }));
        reporter.bytes(19);
        first(Progress::Network(crate::ipc::network::Snapshot {
            bytes: 3,
            active: 1,
        }));
        let second = handle.callback();
        let reporter = crate::ipc::file_progress::Reporter::new(100, second.clone());
        reporter.action(FileAction::Extract, Some(80));
        reporter.bytes(80);
        reporter.action(FileAction::Patch, Some(20));
        reporter.bytes(20);
        reporter.action(FileAction::Verify, None);
        {
            let mut prog = prog.lock().unwrap();
            assert_eq!(prog.processed, 110);
            assert_eq!(prog.network_bytes, u32::MAX as u64 + 8);
            let view = prog.render();
            assert_eq!(view.files[0].action, FileAction::Verify);
            assert!(view.files[0].bytes.is_none());
            assert_eq!(view.summary.unwrap().done, 100);
        }
        reporter.action(FileAction::Flush, None);
        reporter.finish();
        assert!(prog.lock().unwrap().render().files.is_empty());
        assert_eq!(prog.lock().unwrap().processed, 110);
    }
}
