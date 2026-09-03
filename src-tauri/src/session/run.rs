use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail};
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
use crate::installer::registry::read_uninstall_metadata_raw;
use crate::installer::registry::WriteRegistryParams;
use crate::installer::uninstall::{schedule_delete_on_exit, RunUninstallArgs};
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
    build_plan, collect_skip_hash, files_to_probe_writable, find_local, join_install,
    mark_unwritable, normalize_rel, strip_install_prefix, HashKey, InstallPlan, LocalFile,
    PlanAction, PlanInput, SkipReason,
};
use crate::session::source::{
    cleanup_dfs2, ensure_dfs2_session, fetch_metadata, hash_of_item, needs_js_plugin, parse_source,
    prefetch_chunk_urls, resolve_file_location, resolve_range_url, FileLocation, ParsedSource,
    SourceCtx,
};
use crate::session::state::{Phase, Progress as UiProgress, Prompt, UiState};
use crate::session::types::{
    version_gt, ProjectConfig, SessionResult, Settings, SourceField,
};
use crate::session::ui::{send_ev_insight, SessionUi, SilentPluginUi};
use crate::utils::code::{
    attach_download, attach_download_or, attach_metadata, coded_for_mirrorc_response,
    coded_from_error, extract, fail_kind, tag_session, Attach, Cancelled, Coded, Extracted,
    DISK_FULL, FILE_IO_FAILED, HASH_ALGORITHM_UNSUPPORTED, METADATA_UNREACHABLE,
    MIRRORC_CDK_MISSING, MIRRORC_CONFIG_INVALID, MIRRORC_FAILED, MIRRORC_UNREACHABLE,
    NO_DOWNLOAD_NODE, PKG_BROKEN, PROCESS_KILL_FAILED, REGISTRY_WRITE_FAILED,
    RUNTIME_INSTALL_FAILED, SHORTCUT_FAILED, UNINSTALL_INFO_MISSING, WEBVIEW2_REQUIRED,
};
use tokio_util::sync::CancellationToken;
use crate::thirdparty::mirrorc::get_mirrorc_status;
use crate::utils::error::IntoAnyhow;
use crate::utils::metadata::{FileMeta, RepoMetadata};

pub async fn run_op(
    mgr: &ManagedElevate,
    elevate: bool,
    op: IpcOperation,
    on_progress: ProgressNotify,
) -> anyhow::Result<IpcResult> {
    mgr.run(op, elevate, on_progress).await.into_anyhow()
}

async fn run_op_with_ui(
    mgr: &ManagedElevate,
    elevate: bool,
    op: IpcOperation,
    ui: &LiveUi<'_>,
    mut on_ui: impl FnMut(&LiveUi<'_>, &Progress),
) -> anyhow::Result<IpcResult> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();
    let mut op_fut = Box::pin(run_op(
        mgr,
        elevate,
        op,
        progress_notify(move |p| {
            let _ = tx.send(p);
        }),
    ));
    loop {
        tokio::select! {
            Some(p) = rx.recv() => on_ui(ui, &p),
            res = &mut op_fut => return res,
        }
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
            InstallFileMode::Direct(InstallFileSource::Url { .. }) => Some("direct"),
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

    /// Phase-one checkpoint: `Err(Cancelled)` once the user asked to stop.
    fn check_cancel(&self) -> anyhow::Result<()> {
        if self.cancel.is_cancelled() {
            Err(anyhow::Error::new(Cancelled))
        } else {
            Ok(())
        }
    }
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
    if !journal_matches_target(&journal, hash_algorithm, wanted, deletes, archive, &self_images) {
        tracing::info!("staging journal is for different content, dropping");
        return reopen(staged).await;
    }
    tracing::info!("recovering interrupted commit ({} units)", journal.units.len());
    progress(ui, 2, 95.0, "commit", None, None, None);
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
                progress(ui, 2, 95.0, "commit", None, Some(*done), Some(*total));
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
    file_name.replace('\\', "/").trim_start_matches('/').to_string()
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
    let dirty: std::collections::HashSet<&str> = scan.dirty_dirs.iter().map(String::as_str).collect();
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
    let Some(plan) = self_image_plan(settings, project, &staged.staging, list_has_updater, updater_staged)
    else {
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
    let root = units
        .iter_mut()
        .find_map(|u| match u {
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
    progress(ui, 2, 95.0, "commit", None, Some(0), Some(journal.units.len() as u64));
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
                let total = (*total).max(1);
                progress(
                    ui,
                    2,
                    95.0 + (*done as f64 / total as f64) * 3.0,
                    "commit",
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
        schedule_delete_on_exit(&staged.root());
    } else {
        staged.discard(mgr).await;
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
    stage: &'static str,
    subject: Option<&str>,
    done: Option<u64>,
    total: Option<u64>,
) {
    let mut state = ui.live.lock().unwrap_or_else(|e| e.into_inner());
    state.phase = Phase::Running(UiProgress {
        sub_step,
        percent,
        stage,
        subject: subject.map(str::to_string),
        done,
        total,
    });
    let snap = state.clone();
    drop(state);
    ui.inner.state(&snap);
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
    progress(ui, 0, 1.0, "metadata", None, None, None);
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

    if settings.is_update && latest.installer.is_some() && config.enbedded_metadata.is_none() {
        if !latest
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
        settings, config, project, ui, mgr, txn, &latest, hash_key, algo, &mut source_ctx,
        used_online, &staged,
    )
    .await;
    let self_replaced = recovered_self || matches!(result, Ok((_, true)));
    finish_staging(&staged, self_replaced, mgr).await;
    result.map(|(r, _)| r)
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
    progress(ui, 1, 5.0, "hash_scan", None, None, None);
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

    if to_install.is_empty() {
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
            finish_install(settings, config, project, Some(latest), ui, mgr),
        )
        .await?;
        progress(ui, 3, 100.0, "already_latest", None, None, None);
        return Ok((SessionResult::install(true, settings.is_update), self_replaced));
    }

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
    );
    let ranges = dfs2_ranges(
        &tasks,
        source_ctx,
        hash_key,
        config.embedded_files.as_deref().unwrap_or(&[]),
        &latest.patches,
        &local,
    );
    if let Err(err) = ensure_dfs2_session(
        source_ctx,
        ranges.clone(),
        settings.dfs_extras.as_deref(),
    )
    .await
    {
        cleanup_dfs2(source_ctx).await;
        return Err(attach_download_or(err, NO_DOWNLOAD_NODE, None, None));
    }
    // Every failure from here on happened inside this DFS session; the id lets
    // the DFS side find the matching server log.
    let sid = source_ctx.dfs2_session_id();
    let tag_sid = |err: anyhow::Error| match &sid {
        Some(sid) => tag_session(err, sid.clone()),
        None => err,
    };
    log_task_plan(&tasks, &ranges);
    prefetch_chunk_urls(source_ctx, ranges).await;

    progress(ui, 2, 20.0, "plan", None, None, None);
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
    .await
    .map_err(tag_sid)?;
    progress(ui, 3, 98.0, "finalize", None, None, None);
    txn.timed(
        "finalize",
        finish_install(settings, config, project, Some(latest), ui, mgr),
    )
    .await
    .map_err(tag_sid)?;
    progress(ui, 3, 100.0, "install_done", None, None, None);
    Ok((SessionResult::install(false, settings.is_update), self_replaced))
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
        (None, None) => Err(online_err.unwrap_or_else(|| anyhow::Error::from(Coded::bare(METADATA_UNREACHABLE)))),
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
                let total = total.max(1);
                progress(ui, 1, 5.0 + (cur as f64 / total as f64) * 15.0, "hash_scan", None, Some(cur), Some(total));
            }
            res = &mut op_fut => break res?,
        }
    };
    progress(ui, 1, 20.0, "hash_scan", None, None, None);
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
}

struct DownloadProg {
    files: Vec<FileProg>,
    last_bytes: u64,
    last_at: Instant,
}

impl DownloadProg {
    fn from_tasks(tasks: &[InstallTask]) -> Self {
        let mut files = Vec::new();
        for task in tasks {
            match task {
                InstallTask::Single(item) => files.push(FileProg {
                    name: item.file_name.clone(),
                    size: item.size.max(1),
                    downloaded: 0,
                    running: false,
                }),
                InstallTask::Merged { files: group, .. } => {
                    for file in group {
                        files.push(FileProg {
                            name: file.item.file_name.clone(),
                            size: file.item.size.max(1),
                            downloaded: 0,
                            running: false,
                        });
                    }
                }
            }
        }
        Self {
            files,
            last_bytes: 0,
            last_at: Instant::now(),
        }
    }

    fn render(&mut self) -> (f64, Option<String>, u64, u64) {
        let total: u64 = self.files.iter().map(|f| f.size).sum::<u64>().max(1);
        let done: u64 = self.files.iter().map(|f| f.downloaded.min(f.size)).sum();
        let now = Instant::now();
        let dt = now.duration_since(self.last_at).as_millis() as f64;
        if dt > 100.0 {
            self.last_bytes = done;
            self.last_at = now;
        }
        let subject = self
            .files
            .iter()
            .find(|f| f.running && f.downloaded < f.size)
            .map(|f| basename(&f.name).to_string());
        (20.0 + (done as f64 / total as f64) * 75.0, subject, done, total)
    }
}

#[derive(Clone)]
struct ProgressHandle {
    inner: Arc<Mutex<DownloadProg>>,
    ids: Vec<usize>,
}

impl ProgressHandle {
    fn start(&self) {
        if let Ok(mut g) = self.inner.lock() {
            for id in &self.ids {
                if let Some(f) = g.files.get_mut(*id) {
                    f.running = true;
                }
            }
        }
    }

    fn set(&self, local_idx: usize, bytes: u64) {
        let Some(&id) = self.ids.get(local_idx) else {
            return;
        };
        if let Ok(mut g) = self.inner.lock() {
            if let Some(f) = g.files.get_mut(id) {
                f.downloaded = bytes;
            }
        }
    }

    fn finish(&self, ok: bool) {
        if let Ok(mut g) = self.inner.lock() {
            for id in &self.ids {
                if let Some(f) = g.files.get_mut(*id) {
                    f.running = false;
                    if ok {
                        f.downloaded = f.size;
                    }
                }
            }
        }
    }

    fn only(&self, local_idx: usize) -> Self {
        Self {
            inner: self.inner.clone(),
            ids: self.ids.get(local_idx).copied().into_iter().collect(),
        }
    }

    fn callback(&self) -> ProgressNotify {
        let handle = self.clone();
        progress_notify(move |p| apply_ipc_progress(&handle, &p))
    }
}

fn apply_ipc_progress(handle: &ProgressHandle, p: &Progress) {
    match p {
        Progress::Chunk(index, bytes) => handle.set(*index as usize, *bytes),
        Progress::Bytes(n) => handle.set(0, *n),
        _ => {}
    }
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

fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
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
    let prog = Arc::new(Mutex::new(DownloadProg::from_tasks(tasks)));
    let has_error = Arc::new(AtomicBool::new(false));
    let cancel = ui.cancel.clone();
    let sizes: Vec<u64> = tasks.iter().map(task_bytes).collect();
    let threshold = size_threshold(&sizes);
    let local_sem = Arc::new(Semaphore::new(16));
    let large_sem = Arc::new(Semaphore::new(5));
    let small_sem = Arc::new(Semaphore::new(11));
    tracing::info!(
        "TaskManager initialized: threshold={}MB large=5 small=11 local=16 total={}",
        (threshold as f64 / 1024.0 / 1024.0).round(),
        tasks.len()
    );

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
            handle.start();
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
                    start,
                    download_size,
                } => {
                    match install_merged(
                        settings,
                        local_ref,
                        latest,
                        hash_key,
                        files,
                        range,
                        *start,
                        *download_size,
                        source_ctx,
                        staging,
                        mgr,
                        Some(handle.clone()),
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
                                *start,
                                *download_size,
                                source_ctx,
                                staging,
                                mgr,
                                Some(handle.clone()),
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
                    handle.finish(true);
                    Some(Ok(op))
                }
                Err(err) => {
                    handle.finish(false);
                    has_error.store(true, Ordering::Relaxed);
                    Some(Err(err))
                }
            }
        });
    }

    let mut download = Box::pin(join_all(futs));
    let results = loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                if let Ok(mut g) = prog.lock() {
                    let (pct, subject, done, total) = g.render();
                    progress(ui, 2, pct, "download", subject.as_deref(), Some(done), Some(total));
                }
            }
            // dropping `download` aborts every in-flight stream; the staged
            // files are thrown away with the staging directory
            _ = cancel.cancelled() => return Err(anyhow::Error::new(Cancelled)),
            results = &mut download => break results,
        }
    };
    if let Ok(mut g) = prog.lock() {
        let (pct, subject, done, total) = g.render();
        progress(ui, 2, pct, "download", subject.as_deref(), Some(done), Some(total));
    }

    let mut ops = Vec::new();
    let mut first_err = None;
    for res in results.into_iter().flatten() {
        match res {
            Ok(op) => ops.push(op),
            Err(err) => {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }
    if let Some(err) = first_err {
        return Err(err);
    }
    Ok(ops)
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
            return Err(anyhow!("merged fallback cancelled"));
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
                false,
                Some(handle.only(i)),
            )
            .await?,
        );
    }
    last.ok_or_else(|| anyhow!("merged fallback cancelled"))
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
    for attempt in 1..=3 {
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
        let mode = mode_from_op(&ipc);
        let result = if let Some(handle) = handle.clone() {
            run_download_op(
                mgr,
                settings.elevate,
                ipc,
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
    start: usize,
    download_size: usize,
    source_ctx: &SourceCtx,
    staging: &Staging,
    mgr: &ManagedElevate,
    handle: Option<ProgressHandle>,
) -> anyhow::Result<MergedResult> {
    let url = resolve_range_url(
        source_ctx,
        settings.dfs_extras.as_deref(),
        start,
        download_size,
    )
    .await?;
    let chunks: Vec<InstallFileArgs> = files
        .iter()
        .map(|file| InstallFileArgs {
            mode: InstallFileMode::Direct(InstallFileSource::Url {
                url: url.clone(),
                offset: file.offset.saturating_sub(start),
                size: file.size,
                skip_decompress: false,
                request_range: Some(range.to_string()),
            }),
            target: staged_target(staging, &file.item.file_name),
            old: None,
            md5: file.item.md5.clone(),
            xxh: file.item.xxh.clone(),
            clear_installer_index_mark: None,
        })
        .collect();
    let ipc = IpcOperation::InstallMultichunkStream(InstallMultiStreamArgs {
        url,
        range: range.to_string(),
        chunks,
    });
    let mode = merged_mode(files, local_files, &latest.patches, hash_key);
    let (value, insight) = if let Some(handle) = handle.clone() {
        run_download_op(
            mgr,
            settings.elevate,
            ipc.clone(),
            source_ctx,
            Some(mode),
            handle.callback(),
        )
        .await?
    } else {
        run_download_op(
            mgr,
            settings.elevate,
            ipc.clone(),
            source_ctx,
            Some(mode),
            progress_noop(),
        )
        .await?
    };
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
    let hash =
        hash_of_item(item, hash_key).ok_or_else(|| anyhow::Error::from(Coded::bare(HASH_ALGORITHM_UNSUPPORTED)))?;
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
                    let loc = resolve_file_location(
                        source_ctx,
                        &format!("{from}_{hash}"),
                        settings.dfs_extras.as_deref(),
                        false,
                    )
                    .await?;
                    return Ok(hybrid_op(local, loc, &target, item));
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
                if let Ok(loc) = resolve_file_location(
                    source_ctx,
                    &format!("{from}_{hash}"),
                    settings.dfs_extras.as_deref(),
                    false,
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

    let loc =
        resolve_file_location(source_ctx, &hash, settings.dfs_extras.as_deref(), installer).await?;
    Ok(url_op(loc, &target, None, item, None, installer))
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
    loc: FileLocation,
    target: &str,
    old: Option<String>,
    item: &FileMeta,
    diff_size: Option<usize>,
    installer: bool,
) -> IpcOperation {
    let source = file_source(loc);
    let mode = if let Some(diff_size) = diff_size {
        InstallFileMode::Patch { source, diff_size }
    } else {
        InstallFileMode::Direct(source)
    };
    IpcOperation::InstallFile(InstallFileArgs {
        mode,
        target: target.to_string(),
        old,
        md5: item.md5.clone(),
        xxh: item.xxh.clone(),
        clear_installer_index_mark: Some(installer),
    })
}

fn hybrid_op(local: &Embedded, loc: FileLocation, target: &str, item: &FileMeta) -> IpcOperation {
    IpcOperation::InstallFile(InstallFileArgs {
        mode: InstallFileMode::HybridPatch {
            diff: file_source(loc),
            source: InstallFileSource::Local {
                offset: local.offset,
                size: local.size,
                skip_decompress: false,
            },
        },
        target: target.to_string(),
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
) -> anyhow::Result<()> {
    let Some(runtimes) = project.runtimes.as_ref() else {
        return Ok(());
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
        progress(ui, 3, 96.0, "runtime_install", Some(name), None, None);
        let mut last_err = None;
        for _ in 0..3 {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();
            let mut op_fut = Box::pin(run_op(
                mgr,
                settings.elevate,
                IpcOperation::InstallRuntime {
                    tag: tag.clone(),
                    offset: embed.map(|e| e.offset),
                    size: embed.map(|e| e.size),
                    dl_dir: dl_dir.clone(),
                },
                progress_notify(move |p| {
                    let _ = tx.send(p);
                }),
            ));
            let res = loop {
                tokio::select! {
                    Some(p) = rx.recv() => {
                        let Progress::BytesOf { done: cur, total } = p else {
                            continue;
                        };
                        if total > 0 && cur + 1 < total {
                            progress(
                                ui,
                                3,
                                96.0,
                                "runtime_download",
                                Some(name),
                                Some(cur),
                                Some(total),
                            );
                        } else {
                            progress(ui, 3, 96.0, "runtime_install", Some(name), None, None);
                        }
                    }
                    res = &mut op_fut => break res,
                }
            };
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
    Ok(())
}

async fn finish_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    latest: Option<&RepoMetadata>,
    ui: &LiveUi<'_>,
    mgr: &ManagedElevate,
) -> anyhow::Result<()> {
    let (program, desktop) = get_dirs(settings.elevate).await.into_anyhow()?;
    let exe_path = join_install(&settings.install_path, &project.exe_name);
    let program_lnk = format!(
        "{}\\{}\\{}.lnk",
        program, project.app_name, project.app_name
    );
    let desktop_lnk = format!("{}\\{}.lnk", desktop, project.app_name);
    let uninstall_name = crate::utils::i18n::t(
        "shortcut.uninstall",
        &[("subject", project.app_name.as_str())],
    );
    let uninstall_lnk = format!(
        "{}\\{}\\{}.lnk",
        program, project.app_name, uninstall_name
    );
    progress(ui, 3, 98.0, "shortcut", None, None, None);
    if settings.create_lnk && !settings.is_update {
        create_lnk_or_notify(
            mgr,
            settings.elevate,
            CreateLnkArgs {
                target: exe_path.clone(),
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
                target: exe_path.clone(),
                lnk: program_lnk,
            },
            ui,
        )
        .await;
    }
    // the uninstaller itself was swapped in with the commit (see
    // `self_image_units`); only its shortcut is made here, and only if it exists
    let uninstaller_path = join_install(&settings.install_path, &project.uninstall_name);
    if (!settings.is_update || config.install_path_source.starts_with("REG"))
        && std::path::Path::new(&uninstaller_path).is_file()
    {
        create_lnk_or_notify(
            mgr,
            settings.elevate,
            CreateLnkArgs {
                target: uninstaller_path,
                lnk: uninstall_lnk,
            },
            ui,
        )
        .await;
    }
    if let Some(latest) = latest {
        progress(ui, 3, 99.0, "registry", None, None, None);
        let size: u64 = latest.hashed.iter().map(|e| e.size).sum();
        if let Err(err) = run_op(
            mgr,
            settings.elevate,
            IpcOperation::WriteRegistry(WriteRegistryParams {
                reg_name: project.reg_name.clone(),
                name: project.app_name.clone(),
                version: latest.tag_name.clone(),
                exe: exe_path,
                source: settings.install_path.clone(),
                uninstaller: join_install(&settings.install_path, &project.uninstall_name),
                metadata: serde_json::to_string(latest).unwrap_or_default(),
                size,
                publisher: project.publisher.clone(),
            }),
            progress_noop(),
        )
        .await
        {
            tracing::warn!("write registry failed: {err:#}");
            notify_error(ui, err.attach(REGISTRY_WRITE_FAILED));
        }
    }
    emit_insight(project, settings, config, "finish", None, false);
    Ok(())
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
    progress(ui, 0, 2.0, "mirrorc_metadata", None, None, None);
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
        finish_install(settings, config, project, None, ui, mgr).await?;
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

    let result = mirrorc_staged(settings, config, project, ui, mgr, &staged, &url, &sha256).await;
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
) -> anyhow::Result<(SessionResult, bool)> {
    let zip_path = staged
        .staging
        .dl_dir()
        .join(format!("{sha256}.zip"))
        .to_string_lossy()
        .to_string();
    progress(ui, 1, 5.0, "mirrorc_download", None, None, None);
    let cancel = ui.cancel.clone();
    let download = run_op_with_ui(
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
                let total = (*total).max(1);
                progress(
                    ui,
                    1,
                    5.0 + (*downloaded as f64 / total as f64) * 65.0,
                    "mirrorc_download",
                    None,
                    Some(*downloaded),
                    Some(total),
                );
            }
        },
    );
    tokio::select! {
        res = download => res?,
        _ = cancel.cancelled() => return Err(anyhow::Error::new(Cancelled)),
    };
    ui.check_cancel()?;
    progress(ui, 2, 70.0, "mirrorc_verify", None, None, None);
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
                let total = (*total).max(1);
                progress(
                    ui,
                    2,
                    70.0 + (*count as f64 / total as f64) * 25.0,
                    "extract",
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
        Some(text) => Some(
            serde_json::from_str(text)
                .map_err(|e| attach_metadata(e.into()))?,
        ),
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
    install_runtimes(settings, config, project, &staged.staging, ui, mgr).await?;
    finish_install(settings, config, project, meta.as_ref(), ui, mgr).await?;
    progress(ui, 3, 100.0, "install_done", None, None, None);
    Ok((SessionResult::install(false, settings.is_update), self_replaced))
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
    progress(ui, 0, 10.0, "uninstall_scan", None, None, None);
    let meta = read_uninstall_metadata_raw(&project.reg_name, Some(settings.install_path.as_str()))
        .map_err(|e| e.attach(UNINSTALL_INFO_MISSING))?;
    tracing::info!("UNINSTALL_METADATA: {meta}");
    let meta: UninstallMeta = serde_json::from_str(&meta).unwrap_or_default();
    let mut files: Vec<String> = meta.hashed.into_iter().map(|e| e.file_name).collect();
    files.extend(meta.deletes);
    files.push(project.updater_name.clone());
    files.sort();
    files.dedup();
    tracing::info!("uninstall files: {}", files.join(", "));
    let user_data = if settings.delete_user_data {
        project
            .user_data_path
            .iter()
            .map(|p| settings.expand(p, &project.app_name))
            .collect()
    } else {
        Vec::new()
    };
    let (program, desktop) = get_dirs(settings.elevate).await.into_anyhow()?;
    let mut extra: Vec<String> = project
        .extra_uninstall_path
        .iter()
        .map(|p| settings.expand(p, &project.app_name))
        .collect();
    extra.push(format!("{}\\{}", program, project.app_name));
    extra.push(format!("{}\\{}.lnk", desktop, project.app_name));
    if settings.elevate {
        let _ = run_op(mgr, true, IpcOperation::Ping, progress_noop()).await;
    }
    progress(ui, 1, 40.0, "uninstall_delete", None, None, None);
    let raw = run_op(
        mgr,
        settings.elevate,
        IpcOperation::RunUninstall(RunUninstallArgs {
            source: settings.install_path.clone(),
            files,
            user_data_path: user_data,
            extra_uninstall_path: extra,
            reg_name: project.reg_name.clone(),
            uninstall_name: project.uninstall_name.clone(),
        }),
        progress_noop(),
    )
    .await?;
    let IpcResult::RunUninstall(outcome) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    for err in &outcome.errors {
        tracing::warn!("uninstall: {err}");
    }
    if let Some(root) = outcome.self_moved_to.as_deref() {
        schedule_delete_on_exit(root);
    }
    progress(ui, 2, 100.0, "uninstall_done", None, None, None);
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
        }
        let mgr = ManagedElevate::new();
        return run_uninstall(&settings, &config, &project, &ui, &UiState::default(), &mgr)
            .await
            .map(|_| ());
    }
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
    run_install(settings, config, project, ui, &UiState::default(), &mgr)
        .await
        .map(|_| ())
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
        }
    }

    #[test]
    fn self_image_plan_follows_session_kind() {
        let base = crate::fs::staging::scratch_file(&format!("kachina-selfimg-{}", uuid::Uuid::new_v4()));
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
        let p = names(self_image_plan(&test_settings(&install, false), &project, &staging, false, false));
        assert_eq!(p, Some((vec!["uninst.exe".into(), "updater.exe".into()], None)));

        // update, nothing shipped, foreign installer, no uninstaller on disk: updater only
        let p = names(self_image_plan(&test_settings(&install, true), &project, &staging, false, false));
        assert_eq!(p, Some((vec!["updater.exe".into()], None)));

        // ... with an uninstaller present it is refreshed too
        std::fs::write(install.join("uninst.exe"), b"old").unwrap();
        let p = names(self_image_plan(&test_settings(&install, true), &project, &staging, false, false));
        assert_eq!(p, Some((vec!["updater.exe".into(), "uninst.exe".into()], None)));

        // update, updater shipped and staged: uninstaller copied from the staged updater
        let p = names(self_image_plan(&test_settings(&install, true), &project, &staging, true, true));
        assert_eq!(
            p,
            Some((
                vec!["uninst.exe".into()],
                Some(staged_target(&staging, "updater.exe"))
            ))
        );
        // ... shipped but unchanged on disk: copied from the installed updater
        let p = names(self_image_plan(&test_settings(&install, true), &project, &staging, true, false));
        assert_eq!(
            p,
            Some((
                vec!["uninst.exe".into()],
                Some(join_install(&install.to_string_lossy(), "updater.exe"))
            ))
        );
        // ... shipped, no uninstaller on disk: nothing to do
        std::fs::remove_file(install.join("uninst.exe")).unwrap();
        assert!(self_image_plan(&test_settings(&install, true), &project, &staging, true, true).is_none());
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
        assert_eq!(rels(&merged), vec!["file:app.exe".to_string(), "file:uninst.exe".to_string()]);
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
}
