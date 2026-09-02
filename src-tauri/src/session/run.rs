use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail};
use futures::future::join_all;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::dfs::InsightItem;
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
use crate::ipc::{progress_noop, progress_notify, IpcResult, Progress, ProgressNotify};
use crate::local::Embedded;
use crate::session::commands::SessionState;
use crate::session::dump::session_dump;
use crate::session::merge::{dfs2_ranges, file_mode, plan_tasks, FileMode, FilePos, InstallTask};
use crate::session::plan::{
    build_plan, collect_skip_hash, files_to_probe_writable, find_local, join_install,
    mark_unwritable, strip_install_prefix, HashKey, InstallPlan, LocalFile, PlanAction, PlanInput,
    SkipReason,
};
use crate::session::source::{
    cleanup_dfs2, ensure_dfs2_session, fetch_metadata, hash_of_item, needs_js_plugin, parse_source,
    prefetch_chunk_urls, resolve_file_location, resolve_range_url, FileLocation, ParsedSource,
    SourceCtx,
};
use crate::session::state::Prompt;
use crate::session::types::{
    version_gt, ProjectConfig, SessionResult, Settings, SourceField,
};
use crate::session::ui::{send_ev_insight, SessionUi, SilentPluginUi};
use crate::utils::code::{
    attach_download, attach_metadata, code_for_mirrorc_status, fail_kind, Attach, Coded,
    FILE_IO_FAILED, HASH_ALGORITHM_UNSUPPORTED, METADATA_UNREACHABLE, MIRRORC_CDK_MISSING,
    MIRRORC_FAILED, MIRRORC_UNREACHABLE, NO_DOWNLOAD_NODE, PKG_BROKEN, PROCESS_KILL_FAILED,
    REGISTRY_WRITE_FAILED, RUNTIME_INSTALL_FAILED, SHORTCUT_FAILED, TEMP_DIR_UNAVAILABLE,
    UNINSTALL_INFO_MISSING, WEBVIEW2_REQUIRED,
};
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
    ui: &dyn SessionUi,
    mut on_ui: impl FnMut(&dyn SessionUi, &Progress),
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

fn progress(
    ui: &dyn SessionUi,
    sub_step: u32,
    percent: f64,
    stage: &'static str,
    subject: Option<&str>,
    done: Option<u64>,
    total: Option<u64>,
) {
    ui.progress(sub_step, percent, stage, subject, done, total);
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
    _ui: &dyn SessionUi,
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
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    log_session_start("install", settings, config, project);
    let txn = crate::utils::sentry::Transaction::start(
        if settings.is_update {
            "update"
        } else {
            "install"
        },
        "session",
    );
    let result = if settings.source_uri.starts_with("mirrorc://") {
        run_mirrorc(settings, config, project, ui, mgr).await
    } else {
        run_dfs_install(settings, config, project, ui, mgr, &txn).await
    };
    txn.finish(txn_status(&result));
    if let Err(err) = &result {
        // fail counter：低基数分类维度，不携带自由文本（见遥测通道职责收敛）
        emit_insight(
            ui,
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
    ui: &dyn SessionUi,
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
        tracing::info!("install cancelled at process-running prompt");
        return Ok(SessionResult::cancelled(settings.is_update));
    }

    let hash_key = latest.hash_key()?;
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
    let local = txn
        .timed(
            "hash-scan",
            scan_local(
                settings,
                project,
                &latest,
                hash_key,
                &ignore_nonempty,
                ui,
                mgr,
            ),
        )
        .await?;
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
        txn.timed(
            "finalize",
            finish_install(settings, config, project, Some(&latest), ui, mgr),
        )
        .await?;
        progress(ui, 2, 100.0, "done", None, None, None);
        return Ok(SessionResult::install(true, settings.is_update));
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
            return Ok(SessionResult::cancelled(settings.is_update));
        }
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
    txn.set_measurement("download_files", install_items.len() as f64, "none");
    txn.set_measurement(
        "download_bytes",
        install_items.iter().map(|i| i.item.size as f64).sum(),
        "byte",
    );

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
        return Err(err.attach(NO_DOWNLOAD_NODE));
    }
    log_task_plan(&tasks, &ranges);
    prefetch_chunk_urls(&source_ctx, ranges).await;

    progress(ui, 2, 20.0, "download", None, None, None);
    let ops = txn
        .timed(
            "download",
            install_files(
                settings,
                config,
                &latest,
                hash_key,
                &tasks,
                &local,
                &source_ctx,
                ui,
                mgr,
            ),
        )
        .await;
    cleanup_dfs2(&mut source_ctx).await;
    #[cfg_attr(not(debug_assertions), allow(unused_variables))]
    let ops = ops?;
    tracing::info!(
        "All tasks completed successfully: files={} ops={}",
        to_install.len(),
        ops.len()
    );
    session_dump!(settings.dump_dir.as_deref(), "04-install-ops.json", ops);

    if !plan.deletes.is_empty() {
        progress(ui, 2, 95.0, "delete", None, None, None);
        let list: Vec<String> = plan
            .deletes
            .iter()
            .map(|e| join_install(&settings.install_path, e))
            .collect();
        let _ = run_op(
            mgr,
            settings.elevate,
            IpcOperation::RmList(list),
            progress_noop(),
        )
        .await;
    }

    txn.timed(
        "runtimes",
        install_runtimes(settings, config, project, ui, mgr),
    )
    .await?;
    progress(ui, 3, 98.0, "finalize", None, None, None);
    txn.timed(
        "finalize",
        finish_install(settings, config, project, Some(&latest), ui, mgr),
    )
    .await?;
    progress(ui, 3, 100.0, "done", None, None, None);
    Ok(SessionResult::install(false, settings.is_update))
}

struct InstallItem {
    item: FileMeta,
}

async fn pick_metadata(
    settings: &Settings,
    config: &InstallerConfig,
    ui: &dyn SessionUi,
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
    ui: &dyn SessionUi,
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
    let IpcResult::CheckLocalFiles(scanned) = raw else {
        bail!("IPC_SHAPE_ERR");
    };
    Ok(scanned
        .into_iter()
        .map(|e| {
            let file_name = strip_install_prefix(&e.file_name, &settings.install_path);
            LocalFile {
                file_name,
                hash: e.hash,
                size: e.size,
                unwritable: e.unwritable,
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
                    let (pct, subject, done, total) = g.render();
                    progress(ui, 2, pct, "download", subject.as_deref(), Some(done), Some(total));
                }
            }
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
    latest: &RepoMetadata,
    hash_key: HashKey,
    item: &FileMeta,
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
    let err = last_err.unwrap_or_else(|| anyhow::Error::from(Coded::bare(FILE_IO_FAILED)));
    log_task(
        "direct",
        item.size,
        &file_name,
        last_insight.as_ref(),
        false,
        Some(&err.to_string()),
    );
    Err(attach_download(err, Some(&file_name), None).attach_with(FILE_IO_FAILED, file_name))
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
    skip_patch: bool,
) -> anyhow::Result<IpcOperation> {
    let target = join_install(&settings.install_path, &item.file_name);
    let hash =
        hash_of_item(item, hash_key).ok_or_else(|| anyhow::Error::from(Coded::bare(HASH_ALGORITHM_UNSUPPORTED)))?;
    let installer = item.installer.unwrap_or(false);
    if !skip_patch {
        if let Some(local) = local_files.iter().find(|l| l.name == hash) {
            return Ok(IpcOperation::InstallFile(InstallFileArgs {
                mode: InstallFileMode::Direct(InstallFileSource::Local {
                    offset: local.offset,
                    size: local.size,
                    skip_decompress: false,
                }),
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

fn side_hash(side: &crate::utils::metadata::PatchSide, key: HashKey) -> Option<&str> {
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
    tracing::info!("latest_meta.runtimes {runtimes:?}");
    progress(ui, 3, 96.0, "runtime_install", None, None, None);
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
            let mut coded = Coded::bare_with(RUNTIME_INSTALL_FAILED, name.to_string());
            coded.detail = Some(format!("{err:#}"));
            ui.notify(&coded);
        }
    }
    Ok(())
}

async fn finish_install(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    latest: Option<&RepoMetadata>,
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
            let mut coded = Coded::bare(SHORTCUT_FAILED);
            coded.detail = Some(format!("{err:#}"));
            ui.notify(&coded);
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
            let mut coded = Coded::bare(REGISTRY_WRITE_FAILED);
            coded.detail = Some(format!("{err:#}"));
            ui.notify(&coded);
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
        .ok_or_else(|| Coded::bare(MIRRORC_CDK_MISSING))?; // GUI already prompts for CDK before start
    let parsed = parse_source(&settings.source_uri)?;
    let ParsedSource::Mirrorc {
        resource_id,
        channel,
        arch,
        os,
    } = parsed
    else {
        return Err(anyhow::Error::from(Coded::bare(MIRRORC_UNREACHABLE)));
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
    if let Some(coded) = mirrorc_error(&status) {
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
    if version_name == current_version {
        tracing::info!("already latest, tag={version_name}");
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
        .ok_or_else(|| Coded::bare(MIRRORC_FAILED))?;
    tracing::info!("Mirrorc URL {url}");
    let sha256 = status
        .pointer("/data/sha256")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Coded::bare(MIRRORC_FAILED))?;
    let zip_path = join_install(
        &settings.install_path,
        &format!("KachinaInstaller_Mirrorc_{sha256}.zip"),
    );
    progress(ui, 1, 5.0, "mirrorc_download", None, None, None);
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
    )
    .await?;
    progress(ui, 2, 70.0, "mirrorc_verify", None, None, None);
    let installed = run_op_with_ui(
        mgr,
        settings.elevate,
        IpcOperation::RunMirrorcInstall {
            zip_path,
            target_path: settings.install_path.clone(),
        },
        ui,
        |ui, p| match p {
            Progress::Extract {
                file,
                done: count,
                total,
            } => {
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
            Progress::Delete(file) => {
                progress(
                    ui,
                    2,
                    97.0,
                    "delete",
                    Some(file),
                    None,
                    None,
                );
            }
            _ => {}
        },
    )
    .await?;
    let IpcResult::RunMirrorcInstall(meta) = installed else {
        bail!("IPC_SHAPE_ERR");
    };
    let meta: Option<RepoMetadata> = match meta.as_deref() {
        Some(text) => Some(
            serde_json::from_str(text)
                .map_err(|e| attach_metadata(e.into()))?,
        ),
        None => None,
    };
    install_runtimes(settings, config, project, ui, mgr).await?;
    finish_install(settings, config, project, meta.as_ref(), ui, mgr).await?;
    progress(ui, 3, 100.0, "done", None, None, None);
    Ok(SessionResult::install(false, settings.is_update))
}

fn mirrorc_error(status: &Value) -> Option<Coded> {
    let code = status.get("code").and_then(|v| v.as_i64())?;
    let mapped = code_for_mirrorc_status(code)?;
    let detail = status
        .get("msg")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let mut coded = Coded::bare(mapped);
    coded.detail = detail;
    Some(coded)
}

pub async fn run_uninstall(
    settings: &Settings,
    config: &InstallerConfig,
    project: &ProjectConfig,
    ui: &dyn SessionUi,
    mgr: &ManagedElevate,
) -> anyhow::Result<SessionResult> {
    let txn = crate::utils::sentry::Transaction::start("uninstall", "session");
    emit_insight(ui, project, settings, config, "uninstall", None, true);
    let result = run_uninstall_inner(settings, config, project, ui, mgr).await;
    txn.finish(txn_status(&result));
    if let Err(err) = &result {
        emit_insight(
            ui,
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
    ui: &dyn SessionUi,
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
    progress(ui, 2, 100.0, "done", None, None, None);
    let _ = config;
    Ok(SessionResult::uninstall())
}

pub async fn silent_main(args: crate::cli::arg::InstallArgs) -> anyhow::Result<()> {
    let temp_dir = std::env::temp_dir();
    if std::env::set_current_dir(&temp_dir).is_err() {
        return Err(anyhow::Error::from(Coded::bare(TEMP_DIR_UNAVAILABLE)));
    }
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
        return run_uninstall(&settings, &config, &project, &ui, &mgr)
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
    run_install(settings, config, project, ui, &mgr)
        .await
        .map(|_| ())
}
