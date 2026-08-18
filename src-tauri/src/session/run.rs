use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context};
use futures::future::join_all;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::dfs::InsightItem;
use crate::fs::is_dir_empty;
use crate::installer::config::InstallerConfig;
use crate::installer::lnk::get_dirs;
use crate::installer::lnk::CreateLnkArgs;
use crate::installer::registry::read_uninstall_metadata_raw;
use crate::installer::registry::WriteRegistryParams;
use crate::installer::uninstall::{CreateUninstallerArgs, RunUninstallArgs};
use crate::ipc::install_file::{
    InstallFileArgs, InstallFileMode, InstallFileSource, InstallMultiStreamArgs,
};
use crate::ipc::manager::ManagedElevate;
use crate::ipc::operation::IpcOperation;
use crate::ipc::{progress_noop, progress_notify, ProgressNotify};
use crate::local::Embedded;
use crate::session::commands::SessionState;
use crate::session::dump::session_dump;
use crate::session::merge::{dfs2_ranges, file_mode, plan_tasks, FileMode, FilePos, InstallTask};
use crate::session::plan::{
    build_plan, collect_skip_hash, files_to_probe_writable, find_local, join_install,
    mark_unwritable, HashInfo, HashKey, LocalFile, PlanAction, PlanInput,
};
use crate::session::source::{
    cleanup_dfs2, ensure_dfs2_session, fetch_metadata, hash_of_item, needs_js_plugin, parse_source,
    prefetch_chunk_urls, resolve_file_location, resolve_range_url, FileLocation, ParsedSource,
    SourceCtx,
};
use crate::session::types::{
    version_gt, DfsMetadata, ProgressEvent, ProjectConfig, SessionResult, Settings, SourceField,
};
use crate::session::ui::{PromptKind, SessionUi, SilentPluginUi};
use crate::thirdparty::mirrorc::get_mirrorc_status;
use crate::utils::error::IntoAnyhow;

pub async fn run_op(
    mgr: &ManagedElevate,
    elevate: bool,
    op: IpcOperation,
    on_progress: ProgressNotify,
) -> anyhow::Result<Value> {
    let value = mgr.run(op, elevate, on_progress).await.into_anyhow()?;
    unwrap_ipc(value)
}

async fn run_op_with_ui(
    mgr: &ManagedElevate,
    elevate: bool,
    op: IpcOperation,
    ui: &dyn SessionUi,
    mut on_ui: impl FnMut(&dyn SessionUi, &Value),
) -> anyhow::Result<Value> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
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
) -> anyhow::Result<(Value, Option<InsightItem>)> {
    match mgr.run(op, elevate, on_progress).await {
        Ok(value) => {
            let insight = take_insight(&value);
            match unwrap_ipc(value) {
                Ok(v) => {
                    let insight = insight.or_else(|| take_insight(&v));
                    collect_insight(ctx, insight.clone(), mode);
                    Ok((v, insight))
                }
                Err(err) => {
                    collect_insight(ctx, insight, mode);
                    Err(err)
                }
            }
        }
        Err(ta) => {
            collect_insight(ctx, ta.insight, mode);
            Err(ta.error)
        }
    }
}

fn take_insight(value: &Value) -> Option<InsightItem> {
    for key in ["insight"] {
        if let Some(v) = value.get(key) {
            if !v.is_null() {
                if let Ok(item) = serde_json::from_value::<InsightItem>(v.clone()) {
                    return Some(item);
                }
            }
        }
    }
    for path in ["/Ok/insight", "/Err/insight"] {
        if let Some(v) = value.pointer(path) {
            if !v.is_null() {
                if let Ok(item) = serde_json::from_value::<InsightItem>(v.clone()) {
                    return Some(item);
                }
            }
        }
    }
    None
}

fn collect_insight(ctx: &SourceCtx, insight: Option<InsightItem>, mode: Option<&str>) {
    if let (Some(insight), Some(mode)) = (insight, mode) {
        ctx.add_insight(insight, mode);
    }
}

fn mode_from_op(op: &IpcOperation) -> Option<&'static str> {
    match op {
        IpcOperation::InstallFile(args) => match &args.mode {
            InstallFileMode::HybridPatch { .. } => Some("hybridpatch"),
            InstallFileMode::Patch { .. } => Some("patch"),
            InstallFileMode::Direct {
                source: InstallFileSource::Url { .. },
            } => Some("direct"),
            _ => None,
        },
        _ => None,
    }
}

fn merged_mode(
    files: &[FilePos],
    local: &[Embedded],
    patches: &[crate::session::plan::PatchInfo],
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

fn unwrap_ipc(value: Value) -> anyhow::Result<Value> {
    if let Some(err) = value.get("Err") {
        if let Some(msg) = err.as_str() {
            return Err(anyhow!(msg.to_string()));
        }
        if let Some(msg) = err.get("message").and_then(|m| m.as_str()) {
            return Err(anyhow!(msg.to_string()));
        }
        return Err(anyhow!(err.to_string()));
    }
    if let Some(ok) = value.get("Ok") {
        return Ok(ok.clone());
    }
    Ok(value)
}

fn progress(ui: &dyn SessionUi, sub_step: u32, percent: f64, current: impl Into<String>) {
    ui.progress(ProgressEvent {
        sub_step,
        percent,
        current: current.into(),
    });
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

fn emit_insight(
    ui: &dyn SessionUi,
    project: &ProjectConfig,
    settings: &Settings,
    config: &InstallerConfig,
    event: &str,
    data: Option<Value>,
    uninstall: bool,
) {
    ui.insight(
        &insight_base(project, settings, config, uninstall),
        event,
        data,
    );
}

pub async fn run_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    if settings.source_uri.starts_with("mirrorc://") {
        return run_mirrorc(settings, config, project, ui, mgr).await;
    }
    run_dfs_install(settings, config, project, ui, mgr).await
}

async fn run_dfs_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    progress(ui, 0, 1.0, "获取最新版本");
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

    let embedded_meta = config
        .enbedded_metadata
        .as_ref()
        .and_then(|v| serde_json::from_value::<DfsMetadata>(v.clone()).ok());
    let mut source_ctx = SourceCtx::from_embedded(config.embedded_files.as_deref().unwrap_or(&[]));
    source_ctx.attach_plugin(ui.plugin_host());
    let mut online_err = None;
    let online_meta = match fetch_metadata(
        &settings.source_uri,
        settings.dfs_extras.as_deref(),
        &mut source_ctx,
    )
    .await
    {
        Ok(meta) => Some(meta),
        Err(err) => {
            tracing::warn!("online metadata failed: {err:#}");
            online_err = Some(crate::session::error::friendly(&err));
            None
        }
    };

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
            latest.hashed.push(HashInfo {
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
        ui,
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
        return Ok(SessionResult::cancelled(settings.is_update));
    }

    let hash_key = latest.hash_key()?;
    let mut ignore_nonempty = Vec::new();
    if settings.is_update {
        for folder in &project.ignore_folder_path {
            let full = settings.expand(folder, &project.app_name);
            let (empty, _) = is_dir_empty(full.clone(), String::new()).await;
            if !empty {
                ignore_nonempty.push(full);
            }
        }
    }
    progress(ui, 1, 5.0, "校验本地文件……");
    let local = scan_local(
        settings,
        project,
        &latest,
        hash_key,
        &ignore_nonempty,
        ui,
        mgr,
    )
    .await?;

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
            IpcOperation::ProbeWritable { file_list: paths },
            progress_noop(),
        )
        .await?;
        let unwritable: Vec<String> = raw
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        mark_unwritable(&mut plan.files, &settings.install_path, &unwritable);
    }
    session_dump!(settings.dump_dir.as_deref(), "03-plan.json", plan);

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
        finish_install(settings, config, project, Some(&latest), ui, mgr).await?;
        progress(ui, 2, 100.0, "已是最新版本");
        return Ok(SessionResult::install(true, settings.is_update));
    }

    let occupied: Vec<String> = to_install
        .iter()
        .filter(|f| f.unwritable && f.file_name != project.updater_name)
        .map(|f| f.file_name.clone())
        .collect();
    if !occupied.is_empty()
        && !ui
            .confirm(
                PromptKind::OccupiedFiles,
                "提示",
                &format!(
                    "检测到部分文件被占用，继续安装可能无法成功，是否继续？\n\n被占用的文件列表：{}",
                    occupied.join("\n")
                ),
            )
            .await
    {
        return Ok(SessionResult::cancelled(settings.is_update));
    }

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

    let tasks = plan_tasks(
        &install_items
            .iter()
            .map(|i| i.item.clone())
            .collect::<Vec<_>>(),
        hash_key,
        config.embedded_files.as_deref().unwrap_or(&[]),
        &latest.patches,
        &source_ctx,
    );
    let ranges = dfs2_ranges(
        &tasks,
        &source_ctx,
        hash_key,
        config.embedded_files.as_deref().unwrap_or(&[]),
        &latest.patches,
        &local,
    );
    if let Err(err) = ensure_dfs2_session(
        &mut source_ctx,
        ranges.clone(),
        settings.dfs_extras.as_deref(),
    )
    .await
    {
        cleanup_dfs2(&mut source_ctx).await;
        let detail = crate::session::error::friendly(&err);
        if detail == crate::session::error::DFS2_SESSION
            || detail.starts_with(crate::session::error::DFS2_SESSION)
        {
            return Err(err);
        }
        return Err(anyhow!(
            "{}: {}",
            crate::session::error::DFS2_SESSION,
            detail
        ));
    }
    prefetch_chunk_urls(&source_ctx, ranges).await;

    progress(ui, 2, 20.0, "准备下载……");
    let ops = install_files(
        settings,
        config,
        &latest,
        hash_key,
        &tasks,
        &local,
        &source_ctx,
        ui,
        mgr,
    )
    .await;
    cleanup_dfs2(&mut source_ctx).await;
    #[cfg_attr(not(debug_assertions), allow(unused_variables))]
    let ops = ops?;
    session_dump!(settings.dump_dir.as_deref(), "04-install-ops.json", ops);

    if !plan.deletes.is_empty() {
        progress(ui, 2, 95.0, "删除旧版残留文件……");
        let list: Vec<String> = plan
            .deletes
            .iter()
            .map(|e| join_install(&settings.install_path, e))
            .collect();
        let _ = run_op(
            mgr,
            settings.elevate,
            IpcOperation::RmList { list },
            progress_noop(),
        )
        .await;
    }

    install_runtimes(settings, config, project, ui, mgr).await?;
    progress(ui, 3, 98.0, "很快就好……");
    finish_install(settings, config, project, Some(&latest), ui, mgr).await?;
    progress(ui, 3, 100.0, "安装完成");
    Ok(SessionResult::install(false, settings.is_update))
}

struct InstallItem {
    item: HashInfo,
}

async fn pick_metadata(
    settings: &Settings,
    config: &InstallerConfig,
    ui: &dyn SessionUi,
    local: Option<DfsMetadata>,
    online: Option<DfsMetadata>,
    online_err: Option<String>,
) -> anyhow::Result<(DfsMetadata, bool)> {
    match (local, online) {
        (None, None) => Err(anyhow!(
            "{}{}",
            crate::session::error::META_FAILED,
            online_err
                .map(|e| format!("\n{}", crate::session::error::friendly(&e)))
                .unwrap_or_else(|| "：未知错误，请检查日志".to_string())
        )),
        (None, Some(online)) => Ok((online, true)),
        (Some(local), None) => Ok((local, false)),
        (Some(local), Some(online)) => {
            if settings.online {
                return Ok((online, true));
            }
            if online.tag_name != local.tag_name && version_gt(&online.tag_name, &local.tag_name) {
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
                            .confirm(
                                PromptKind::VersionMismatch,
                                "提示",
                                "当前安装包不是最新版本，是否直接安装最新版本？",
                            )
                            .await
                };
                if take_online {
                    return Ok((online, true));
                }
            }
            Ok((local, false))
        }
    }
}

async fn prepare_process(
    settings: &Settings,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
    _version: &str,
) -> anyhow::Result<bool> {
    let found = run_op(
        mgr,
        false,
        IpcOperation::FindProcessByName {
            name: project.exe_name.clone(),
        },
        progress_noop(),
    )
    .await
    .unwrap_or(json!([]));
    let procs: Vec<(u32, String)> = serde_json::from_value(found).unwrap_or_default();
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
        .confirm(
            PromptKind::ProcessRunning,
            "提示",
            &format!(
                "检测到{}正在运行，是否结束进程并继续安装？",
                project.app_name
            ),
        )
        .await
    {
        return Ok(false);
    }
    for (pid, _) in &running {
        if run_op(
            mgr,
            settings.elevate,
            IpcOperation::KillProcess { pid: *pid },
            progress_noop(),
        )
        .await
        .is_err()
        {
            run_op(
                mgr,
                true,
                IpcOperation::KillProcess { pid: *pid },
                progress_noop(),
            )
            .await
            .context("结束进程失败")?;
        }
    }
    Ok(true)
}

async fn scan_local(
    settings: &Settings,
    project: &ProjectConfig,
    latest: &DfsMetadata,
    hash_key: HashKey,
    ignore_nonempty: &[String],
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<Vec<LocalFile>> {
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
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
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
                let (cur, total) = scan_progress_pair(&p);
                let total = total.max(1);
                progress(ui, 1, 5.0 + (cur as f64 / total as f64) * 15.0, format!("{cur} / {total}"));
            }
            res = &mut op_fut => break res?,
        }
    };
    progress(ui, 1, 20.0, "校验本地文件……");
    let scanned = raw.as_array().cloned().unwrap_or_default();
    Ok(scanned
        .into_iter()
        .map(|e| {
            let file_name = e
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim_start_matches(&settings.install_path)
                .trim_start_matches(['\\', '/'])
                .to_string();
            LocalFile {
                file_name,
                hash: e
                    .get("hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                size: e.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                unwritable: e
                    .get("unwritable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }
        })
        .collect())
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
    speed: f64,
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
            speed: 0.0,
        }
    }

    fn render(&mut self) -> (f64, String) {
        let total: u64 = self.files.iter().map(|f| f.size).sum::<u64>().max(1);
        let done: u64 = self.files.iter().map(|f| f.downloaded.min(f.size)).sum();
        let now = Instant::now();
        let dt = now.duration_since(self.last_at).as_millis() as f64;
        if dt > 100.0 {
            self.speed = (done.saturating_sub(self.last_bytes) as f64) / dt;
            self.last_bytes = done;
            self.last_at = now;
        }
        let running: Vec<String> = self
            .files
            .iter()
            .filter(|f| f.running && f.downloaded < f.size)
            .map(|f| {
                format!(
                    "{} {}/{}",
                    basename(&f.name),
                    format_size(f.downloaded),
                    format_size(f.size)
                )
            })
            .collect();
        let html = format!(
            "<span class=\"d-single-stat\">{} / {} ({}/s)</span><div class=\"d-single-list\"><div class=\"d-single\">{}</div></div>",
            format_size(done),
            format_size(total),
            format_size((self.speed * 1000.0) as u64),
            running.join("</div><div class=\"d-single\">")
        );
        (20.0 + (done as f64 / total as f64) * 75.0, html)
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

fn apply_ipc_progress(handle: &ProgressHandle, p: &Value) {
    if let Some(idx) = p.get("chunk_index").and_then(|v| v.as_u64()) {
        let n = p
            .get("progress")
            .and_then(|v| v.as_u64())
            .or_else(|| p.get("payload").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        handle.set(idx as usize, n);
        return;
    }
    if let Some(n) = p.as_u64().or_else(|| p.as_f64().map(|f| f as u64)) {
        handle.set(0, n);
        return;
    }
    if let Some(arr) = p.as_array() {
        if let Some(n) = arr.first().and_then(|v| v.as_u64()) {
            handle.set(0, n);
        }
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
    patches: &[crate::session::plan::PatchInfo],
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

fn scan_progress_pair(p: &Value) -> (u64, u64) {
    if let Some(arr) = p.as_array() {
        return (
            arr.first().and_then(|v| v.as_u64()).unwrap_or(0),
            arr.get(1).and_then(|v| v.as_u64()).unwrap_or(1),
        );
    }
    (
        p.get(0)
            .or_else(|| p.get("0"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        p.get(1)
            .or_else(|| p.get("1"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1),
    )
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
    latest: &DfsMetadata,
    hash_key: HashKey,
    tasks: &[InstallTask],
    disk_files: &[LocalFile],
    source_ctx: &SourceCtx,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<Vec<IpcOperation>> {
    let local_files = Arc::new(config.embedded_files.clone().unwrap_or_default());
    let disk_files = Arc::new(disk_files.to_vec());
    let prog = Arc::new(Mutex::new(DownloadProg::from_tasks(tasks)));
    let has_error = Arc::new(AtomicBool::new(false));
    let sizes: Vec<u64> = tasks.iter().map(task_bytes).collect();
    let threshold = size_threshold(&sizes);
    let local_sem = Arc::new(Semaphore::new(16));
    let large_sem = Arc::new(Semaphore::new(5));
    let small_sem = Arc::new(Semaphore::new(11));

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
        futs.push(async move {
            let _permit = sem.acquire().await;
            if has_error.load(Ordering::Relaxed) {
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
                                        &all, source_ctx, mgr, &handle, &has_error,
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
                    let (pct, html) = g.render();
                    progress(ui, 2, pct, html);
                }
            }
            results = &mut download => break results,
        }
    };
    if let Ok(mut g) = prog.lock() {
        let (pct, html) = g.render();
        progress(ui, 2, pct, html);
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
    latest: &DfsMetadata,
    hash_key: HashKey,
    files: &[FilePos],
    failed: &[usize],
    source_ctx: &SourceCtx,
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
    latest: &DfsMetadata,
    hash_key: HashKey,
    item: &HashInfo,
    source_ctx: &SourceCtx,
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
    let err = last_err.unwrap_or_else(|| anyhow!("安装失败，请重试"));
    log_task(
        "direct",
        item.size,
        &file_name,
        last_insight.as_ref(),
        false,
        Some(&err.to_string()),
    );
    Err(crate::session::error::file_release(&file_name, &err))
}

struct MergedResult {
    op: IpcOperation,
    failed: Vec<usize>,
}

async fn install_merged(
    settings: &Settings,
    local_files: &[Embedded],
    latest: &DfsMetadata,
    hash_key: HashKey,
    files: &[FilePos],
    range: &str,
    start: usize,
    download_size: usize,
    source_ctx: &SourceCtx,
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
            mode: InstallFileMode::Direct {
                source: InstallFileSource::Url {
                    url: url.clone(),
                    offset: file.offset.saturating_sub(start),
                    size: file.size,
                    skip_decompress: false,
                },
            },
            target: join_install(&settings.install_path, &file.item.file_name),
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
    let mut failed = Vec::new();
    if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
        for (i, res) in results.iter().enumerate() {
            if res.get("Err").is_some() || res.get("error").is_some() {
                failed.push(i);
            }
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
    latest: &DfsMetadata,
    hash_key: HashKey,
    item: &HashInfo,
    source_ctx: &SourceCtx,
    skip_patch: bool,
) -> anyhow::Result<IpcOperation> {
    let target = join_install(&settings.install_path, &item.file_name);
    let hash =
        hash_of_item(item, hash_key).ok_or_else(|| anyhow!(crate::session::error::HASH_INVALID))?;
    let installer = item.installer.unwrap_or(false);
    if !skip_patch {
        if let Some(local) = local_files.iter().find(|l| l.name == hash) {
            return Ok(IpcOperation::InstallFile(InstallFileArgs {
                mode: InstallFileMode::Direct {
                    source: InstallFileSource::Local {
                        offset: local.offset,
                        size: local.size,
                        skip_decompress: false,
                    },
                },
                target,
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
    Ok(url_op(loc, &target, item, None, installer))
}

fn side_hash(side: &crate::session::plan::PatchSide, key: HashKey) -> Option<&str> {
    match key {
        HashKey::Md5 => side.md5.as_deref(),
        HashKey::Xxh => side.xxh.as_deref(),
    }
}

fn file_source(loc: FileLocation) -> InstallFileSource {
    if let Some(url) = loc.url {
        InstallFileSource::Url {
            url,
            offset: loc.offset,
            size: loc.size,
            skip_decompress: loc.skip_decompress,
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
    item: &HashInfo,
    diff_size: Option<usize>,
    installer: bool,
) -> IpcOperation {
    let source = file_source(loc);
    let mode = if let Some(diff_size) = diff_size {
        InstallFileMode::Patch { source, diff_size }
    } else {
        InstallFileMode::Direct { source }
    };
    IpcOperation::InstallFile(InstallFileArgs {
        mode,
        target: target.to_string(),
        md5: item.md5.clone(),
        xxh: item.xxh.clone(),
        clear_installer_index_mark: Some(installer),
    })
}

fn hybrid_op(local: &Embedded, loc: FileLocation, target: &str, item: &HashInfo) -> IpcOperation {
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
        md5: item.md5.clone(),
        xxh: item.xxh.clone(),
        clear_installer_index_mark: None,
    })
}

async fn install_runtimes(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<()> {
    let Some(runtimes) = project.runtimes.as_ref() else {
        return Ok(());
    };
    progress(ui, 3, 96.0, "安装运行库……");
    for tag in runtimes {
        let embed = config
            .embedded_files
            .as_ref()
            .and_then(|files| files.iter().find(|e| e.name == *tag));
        let name = runtime_name(tag);
        progress(ui, 3, 96.0, format!("安装{name}……"));
        let mut last_err = None;
        for _ in 0..3 {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
            let mut op_fut = Box::pin(run_op(
                mgr,
                settings.elevate,
                IpcOperation::InstallRuntime {
                    tag: tag.clone(),
                    offset: embed.map(|e| e.offset),
                    size: embed.map(|e| e.size),
                },
                progress_notify(move |p| {
                    let _ = tx.send(p);
                }),
            ));
            let res = loop {
                tokio::select! {
                    Some(p) = rx.recv() => {
                        let (cur, total) = scan_progress_pair(&p);
                        if total > 0 && cur + 1 < total {
                            progress(
                                ui,
                                3,
                                96.0,
                                format!(
                                    "下载 {name} ……<br>{} / {}",
                                    format_size(cur),
                                    format_size(total)
                                ),
                            );
                        } else {
                            progress(ui, 3, 96.0, format!("安装 {name} ……"));
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
                Err(err) => last_err = Some(err),
            }
        }
        if let Some(err) = last_err {
            tracing::error!("安装{name}失败: {err:#}，请手动安装");
            ui.alert("出错了", &format!("安装{name}失败: {err}，请手动安装"))
                .await;
        }
    }
    Ok(())
}

async fn finish_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    latest: Option<&DfsMetadata>,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<()> {
    let (program, desktop) = get_dirs(settings.elevate).await.into_anyhow()?;
    let exe_path = join_install(&settings.install_path, &project.exe_name);
    let program_lnk = format!(
        "{}\\{}\\{}.lnk",
        program, project.app_name, project.app_name
    );
    let desktop_lnk = format!("{}\\{}.lnk", desktop, project.app_name);
    let uninstall_lnk = format!(
        "{}\\{}\\卸载{}.lnk",
        program, project.app_name, project.app_name
    );
    if settings.create_lnk && !settings.is_update {
        let _ = run_op(
            mgr,
            settings.elevate,
            IpcOperation::CreateLnk(CreateLnkArgs {
                target: exe_path.clone(),
                lnk: desktop_lnk,
            }),
            progress_noop(),
        )
        .await;
    }
    if !settings.is_update {
        let _ = run_op(
            mgr,
            settings.elevate,
            IpcOperation::CreateLnk(CreateLnkArgs {
                target: exe_path.clone(),
                lnk: program_lnk,
            }),
            progress_noop(),
        )
        .await;
    }
    if !settings.is_update || config.install_path_source.starts_with("REG") {
        if let Err(err) = run_op(
            mgr,
            settings.elevate,
            IpcOperation::CreateUninstaller(CreateUninstallerArgs {
                source: settings.install_path.clone(),
                uninstaller_name: project.uninstall_name.clone(),
                updater_name: project.updater_name.clone(),
            }),
            progress_noop(),
        )
        .await
        {
            tracing::warn!("create uninstaller failed: {err:#}");
            ui.alert("出错了", &format!("创建卸载程序失败: {err}"))
                .await;
        }
        let _ = run_op(
            mgr,
            settings.elevate,
            IpcOperation::CreateLnk(CreateLnkArgs {
                target: join_install(&settings.install_path, &project.uninstall_name),
                lnk: uninstall_lnk,
            }),
            progress_noop(),
        )
        .await;
    }
    if let Some(latest) = latest {
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
            ui.alert("出错了", &format!("写入注册表失败: {err}")).await;
        }
    }
    emit_insight(ui, project, settings, config, "finish", None, false);
    Ok(())
}

async fn run_mirrorc(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    let cdk = settings
        .mirrorc_cdk
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("需要 Mirror酱 CDK"))?; // GUI already prompts for CDK before start
    let parsed = parse_source(&settings.source_uri)?;
    let ParsedSource::Mirrorc {
        resource_id,
        channel,
        arch,
        os,
    } = parsed
    else {
        return Err(anyhow!(format!(
            "无法获取Mirror酱数据，安装包可能已经损坏：{}",
            settings.source_uri
        )));
    };
    progress(ui, 0, 2.0, "从 Mirror酱 获取最新版本");
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
    .into_anyhow()?;
    if let Some((msg, reopen)) = mirrorc_error(&status) {
        if reopen {
            ui.reopen_source();
        }
        return Err(anyhow!(msg));
    }
    let version_name = status
        .pointer("/data/version_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if version_name == current_version {
        finish_install(settings, config, project, None, ui, mgr).await?;
        return Ok(SessionResult::install(true, settings.is_update));
    }
    emit_insight(
        ui,
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
        return Ok(SessionResult::cancelled(settings.is_update));
    }
    let url = status
        .pointer("/data/url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("从Mirror酱获取更新失败: 下载地址为空，请联系Mirror酱客服"))?;
    let sha256 = status
        .pointer("/data/sha256")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("从Mirror酱获取更新失败: 校验数据为空，请联系Mirror酱客服"))?;
    let zip_path = join_install(
        &settings.install_path,
        &format!("KachinaInstaller_Mirrorc_{sha256}.zip"),
    );
    progress(ui, 1, 5.0, "准备从Mirror酱下载……");
    run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::RunMirrorcDownload {
            url: url.to_string(),
            zip_path: zip_path.clone(),
        },
        ui,
        |ui, p| {
            if p.get("type").and_then(|v| v.as_str()) == Some("download") {
                let downloaded = p.get("downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                let total = p.get("total").and_then(|v| v.as_u64()).unwrap_or(1).max(1);
                progress(
                    ui,
                    1,
                    5.0 + (downloaded as f64 / total as f64) * 65.0,
                    format!("{} / {}", format_size(downloaded), format_size(total)),
                );
            }
        },
    )
    .await?;
    progress(ui, 2, 70.0, "检查压缩包……");
    let installed = run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::RunMirrorcInstall {
            zip_path,
            target_path: settings.install_path.clone(),
        },
        ui,
        |ui, p| match p.get("type").and_then(|v| v.as_str()) {
            Some("extract") => {
                let file = p.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let count = p.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                let total = p.get("total").and_then(|v| v.as_u64()).unwrap_or(1).max(1);
                progress(
                    ui,
                    2,
                    70.0 + (count as f64 / total as f64) * 25.0,
                    format!("<div class=\"d-single-stat\">解压 {file}</div>"),
                );
            }
            Some("delete") => {
                let file = p.get("file").and_then(|v| v.as_str()).unwrap_or("");
                progress(
                    ui,
                    2,
                    97.0,
                    format!("<div class=\"d-single-stat\">删除 {file}</div>"),
                );
            }
            _ => {}
        },
    )
    .await?;
    let meta = installed
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| serde_json::from_value::<DfsMetadata>(v.clone()).ok());
    install_runtimes(settings, config, project, ui, mgr).await?;
    finish_install(settings, config, project, meta.as_ref(), ui, mgr).await?;
    progress(ui, 3, 100.0, "安装完成");
    Ok(SessionResult::install(false, settings.is_update))
}

fn mirrorc_error(status: &Value) -> Option<(String, bool)> {
    let code = status.get("code").and_then(|v| v.as_i64())?;
    if code == 0 {
        return None;
    }
    let (msg, reopen) = match code {
        1001 | 8002 | 8003 | 8004 => ("Mirror酱参数错误，请检查打包配置", false),
        8001 => ("从Mirror酱获取更新失败，请检查打包配置", false),
        7001 => ("Mirror酱 CDK 已过期", true),
        7002 => ("Mirror酱 CDK 错误，请检查设置的 CDK 是否正确", true),
        7003 => (
            "Mirror酱 CDK 今日下载次数已达上限，请更换 CDK 或明天再试",
            false,
        ),
        7004 => (
            "Mirror酱 CDK 类型和待下载的资源不匹配，请检查设置的 CDK 是否正确",
            true,
        ),
        7005 => ("Mirror酱 CDK 已被封禁，请更换 CDK", true),
        _ => {
            let detail = status
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("未知错误");
            return Some((
                format!("从Mirror酱获取更新失败: {detail}，请联系Mirror酱客服"),
                false,
            ));
        }
    };
    Some((msg.to_string(), reopen))
}

pub async fn run_uninstall(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    progress(ui, 0, 10.0, "准备卸载……");
    let meta = read_uninstall_metadata_raw(&project.reg_name, Some(settings.install_path.as_str()))
        .map_err(|e| {
            crate::session::error::hide(crate::session::error::UNINSTALL_META_MISSING, e)
        })?;
    let hashed: Vec<HashInfo> = meta
        .get("hashed")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value::<HashInfo>(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let deletes: Vec<String> = meta
        .get("deletes")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let mut files: Vec<String> = hashed.into_iter().map(|e| e.file_name).collect();
    files.extend(deletes);
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
    progress(ui, 1, 40.0, "正在卸载……");
    run_op(
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
    progress(ui, 2, 100.0, "卸载完成");
    let _ = config;
    Ok(SessionResult::uninstall())
}

pub async fn silent_main(args: crate::cli::arg::InstallArgs) -> anyhow::Result<()> {
    let temp_dir = std::env::temp_dir();
    if std::env::set_current_dir(&temp_dir).is_err() {
        return Err(anyhow!(crate::session::error::TEMP_DIR));
    }
    let config = crate::installer::config::resolve_installer_config(args.clone(), true).await?;
    let project = match config.embedded_config.as_ref() {
        Some(value) => ProjectConfig::from_value(value)?,
        None => {
            tracing::error!(
                "embedded config missing (embedded_files={})",
                config.embedded_files.as_ref().map(|f| f.len()).unwrap_or(0)
            );
            return Err(anyhow!(crate::session::error::PKG_BROKEN));
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
        emit_insight(&ui, &project, &settings, &config, "uninstall", None, true);
        if let Err(err) = run_uninstall(&settings, &config, &project, &ui, &mgr).await {
            emit_insight(
                &ui,
                &project,
                &settings,
                &config,
                "error",
                Some(json!({ "error": format!("{err:#}") })),
                true,
            );
            return Err(err);
        }
        return Ok(());
    }
    if needs_js_plugin(&settings.source_uri) {
        if crate::host::webview_version().is_err() {
            return Err(anyhow!(crate::session::error::PLUGIN_NEED_WEBVIEW2));
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
    if let Err(err) = run_install(settings, config, project, ui, &mgr).await {
        emit_insight(
            ui,
            project,
            settings,
            config,
            "error",
            Some(json!({ "error": format!("{err:#}") })),
            false,
        );
        return Err(err);
    }
    Ok(())
}
