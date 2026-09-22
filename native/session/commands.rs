use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::Value;

use crate::cli::arg::InstallArgs;
use crate::host::HostHandle;
use crate::installer::config::{resolve_installer_config, InstallerConfig};
use crate::session::run::{run_install, run_uninstall};
use crate::session::source::{needs_js_plugin, parse_source, ParsedSource};
use crate::session::state::{
    CdkStatus, Intent, Mode, Options, PathState, PathWritable, Phase, Progress, ProjectView,
    Renderer, SourceItem, Theme, UiSession, UiState,
};
use crate::session::types::{elevate_from_state, SessionInput, Settings, SourceField};
use crate::session::ui::{GuiUi, PluginHub, PromptHub};
use crate::session::ProjectConfig;
use crate::utils::code::{
    coded_for_mirrorc_response, coded_from_error, Attach, Coded, MIRRORC_CONFIG_INVALID,
    MIRRORC_UNREACHABLE, PKG_BROKEN, UNINSTALL_INFO_MISSING,
};
use crate::utils::error::{IntoAnyhow, TACommandError, TAResult};

#[derive(Clone, Default)]
pub struct SessionState {
    pub prompts: Arc<PromptHub>,
    pub plugins: Arc<PluginHub>,
}

pub struct GuiRuntime {
    pub session: Arc<Mutex<UiSession>>,
    pub config: Arc<InstallerConfig>,
    pub project: Option<Arc<ProjectConfig>>,
    pub running: AtomicBool,
    /// Startup already failed (broken package, missing uninstall metadata):
    /// there is no usable Ready page, so `Dismiss` closes the window instead.
    pub fatal: bool,
    /// Token of the running session; replaced on every `Start`.
    pub cancel: Mutex<tokio_util::sync::CancellationToken>,
    /// Bumped when the user cancels a CDK check or switches source so an
    /// in-flight verify cannot commit.
    cdk_epoch: AtomicU64,
    cdk_restore: Mutex<Option<CdkStatus>>,
}

impl GuiRuntime {
    pub fn cancel_running(&self) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Phase::Running(progress) = &mut session.state.phase {
            if progress.cancel == crate::session::state::CancelState::Available {
                self.cancel
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .cancel();
                progress.apply_user_cancel(true);
            }
        }
    }

    fn fresh_cancel(&self) -> tokio_util::sync::CancellationToken {
        let token = tokio_util::sync::CancellationToken::new();
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = token.clone();
        token
    }
    pub fn snapshot(&self) -> UiState {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .clone()
    }

    pub fn emit(&self, handle: &HostHandle) {
        handle.emit("ui-state", self.snapshot());
    }
}

pub async fn prepare_gui(args: InstallArgs, preset: Option<SessionInput>) -> Arc<GuiRuntime> {
    let config = match resolve_installer_config(args.clone(), true).await {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("resolve installer config failed: {err:#}");
            return failed_runtime(args, Coded::bare(PKG_BROKEN));
        }
    };
    let project = match config.embedded_config.as_ref() {
        Some(value) => match ProjectConfig::from_value(value) {
            Ok(p) => p,
            Err(err) => {
                tracing::error!("embedded config parse failed: {err:#}");
                return failed_runtime_with_config(config, Coded::bare(PKG_BROKEN));
            }
        },
        None => return failed_runtime_with_config(config, Coded::bare(PKG_BROKEN)),
    };

    if let (Some(index), Some(files)) = (&config.embedded_index, &config.embedded_files) {
        let mut broken = false;
        for i in index {
            match files.iter().find(|e| e.name == i.name) {
                None => broken = true,
                Some(target) if target.offset != i.offset || target.raw_offset != i.raw_offset => {
                    broken = true;
                }
                _ => {}
            }
        }
        if broken {
            tracing::error!("embedded index does not match packed files");
            return ready_runtime(args, preset, config, project, Some(Coded::bare(PKG_BROKEN)))
                .await;
        }
    }

    let is_uninstall = config.is_uninstall || args.uninstall;
    if is_uninstall {
        match crate::installer::registry::read_uninstall_metadata(project.reg_name.clone()).await {
            Ok(_) => {}
            Err(err) => {
                tracing::error!("uninstall metadata missing: {err}");
                return ready_runtime(
                    args,
                    preset,
                    config,
                    project,
                    Some(Coded::bare(UNINSTALL_INFO_MISSING)),
                )
                .await;
            }
        }
    }

    ready_runtime(args, preset, config, project, None).await
}

async fn ready_runtime(
    args: InstallArgs,
    preset: Option<SessionInput>,
    config: InstallerConfig,
    project: ProjectConfig,
    failed: Option<Coded>,
) -> Arc<GuiRuntime> {
    let theme = apply_theme(config.embedded_image.as_deref());
    let mut source_uri = if let Some(p) = preset.as_ref() {
        p.source_uri.clone()
    } else {
        project
            .source_uri(args.source.as_deref())
            .unwrap_or_default()
    };
    let all_sources = visible_sources(&project, &source_uri);
    if !all_sources.is_empty() && !all_sources.iter().any(|s| s.uri == source_uri) {
        source_uri = all_sources
            .iter()
            .find(|s| !s.hidden)
            .or(all_sources.first())
            .unwrap()
            .uri
            .clone();
    }
    let is_uninstall = config.is_uninstall || args.uninstall;
    let install_path = if let Some(p) = preset.as_ref() {
        p.install_path.clone()
    } else if let Some(t) = args.target.as_ref() {
        t.to_string_lossy().into_owned()
    } else {
        config.install_path.clone()
    };
    let create_lnk = preset.as_ref().map(|p| p.create_lnk).unwrap_or(true);
    let delete_user_data = preset.as_ref().map(|p| p.delete_user_data).unwrap_or(false);
    let mut cdk = preset
        .as_ref()
        .and_then(|p| p.mirrorc_cdk.clone())
        .or(args.mirrorc_cdk.clone());
    if cdk.is_none() {
        cdk = crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name)).ok();
    }
    let cdk_status = if cdk.as_deref().unwrap_or("").is_empty() {
        CdkStatus::Idle
    } else {
        CdkStatus::Ok
    };

    let state = UiState {
        phase: failed.clone().map(Phase::Failed).unwrap_or(Phase::Ready),
        mode: if is_uninstall {
            Mode::Uninstall
        } else {
            Mode::Install
        },
        project: ProjectView {
            window_title: project.window_title.clone(),
            title: project.title.clone(),
            description: project.description.clone(),
            borderless: project.window_borderless.unwrap_or(false),
            lang: crate::utils::i18n::lang().to_string(),
        },
        options: Options {
            install_path: install_path.clone(),
            source_uri,
            create_lnk,
            delete_user_data,
            mirrorc_cdk: cdk.filter(|s| !s.is_empty()),
        },
        sources: all_sources.clone(),
        offline: config.embedded_index.is_some(),
        path: PathState {
            writable: PathWritable::Writable,
            exists: false,
            upgrade: false,
        },
        needs_elevate: false,
        cdk: cdk_status,
        theme,
        pending: None,
    };

    let mut sess = UiSession::with_project(
        state,
        Renderer::WebView,
        project.exe_name.clone(),
        project.app_name.clone(),
        project.uac_strategy.clone(),
    );
    sess.apply(Intent::SetPath { path: install_path });
    if is_uninstall {
        sess.state.mode = Mode::Uninstall;
    }
    let fatal = failed.is_some();
    if let Some(coded) = failed {
        sess.state.phase = Phase::Failed(coded);
    }

    Arc::new(GuiRuntime {
        session: Arc::new(Mutex::new(sess)),
        config: Arc::new(config),
        project: Some(Arc::new(project)),
        running: AtomicBool::new(false),
        fatal,
        cancel: Mutex::new(tokio_util::sync::CancellationToken::new()),
        cdk_epoch: AtomicU64::new(0),
        cdk_restore: Mutex::new(None),
    })
}

fn failed_runtime(args: InstallArgs, coded: Coded) -> Arc<GuiRuntime> {
    let mut state = UiState::default();
    state.phase = Phase::Failed(coded);
    state.project.lang = crate::utils::i18n::lang().to_string();
    let config = InstallerConfig {
        install_path: String::new(),
        install_path_exists: false,
        install_path_source: "",
        is_uninstall: args.uninstall,
        embedded_files: None,
        embedded_index: None,
        embedded_config: None,
        enbedded_metadata: None,
        embedded_image: None,
        exe_path: String::new(),
        args,
        elevated: false,
        preset: None,
    };
    Arc::new(GuiRuntime {
        session: Arc::new(Mutex::new(UiSession::with_renderer(
            state,
            Renderer::WebView,
        ))),
        config: Arc::new(config),
        project: None,
        running: AtomicBool::new(false),
        fatal: true,
        cancel: Mutex::new(tokio_util::sync::CancellationToken::new()),
        cdk_epoch: AtomicU64::new(0),
        cdk_restore: Mutex::new(None),
    })
}

fn failed_runtime_with_config(config: InstallerConfig, coded: Coded) -> Arc<GuiRuntime> {
    let mut state = UiState::default();
    state.phase = Phase::Failed(coded);
    state.project.lang = crate::utils::i18n::lang().to_string();
    Arc::new(GuiRuntime {
        session: Arc::new(Mutex::new(UiSession::with_renderer(
            state,
            Renderer::WebView,
        ))),
        config: Arc::new(config),
        project: None,
        running: AtomicBool::new(false),
        fatal: true,
        cancel: Mutex::new(tokio_util::sync::CancellationToken::new()),
        cdk_epoch: AtomicU64::new(0),
        cdk_restore: Mutex::new(None),
    })
}

pub(crate) fn visible_sources(project: &ProjectConfig, _current_uri: &str) -> Vec<SourceItem> {
    match &project.source {
        SourceField::Single(uri) => vec![SourceItem {
            id: "default".into(),
            name: String::new(),
            uri: uri.clone(),
            icon: None,
            requires_webview: needs_js_plugin(uri),
            hidden: false,
        }],
        SourceField::List(list) => list
            .iter()
            .map(|s| SourceItem {
                id: s.id.clone(),
                name: s.name.clone(),
                uri: s.uri.clone(),
                icon: s.icon.clone(),
                requires_webview: needs_js_plugin(&s.uri),
                hidden: s.hidden,
            })
            .collect(),
    }
}

fn apply_theme(image_b64: Option<&str>) -> Theme {
    let Some(b64) = image_b64.filter(|s| !s.is_empty()) else {
        return Theme::None;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return Theme::None;
    };
    identify_and_install_theme(&bytes)
}

fn identify_and_install_theme(bytes: &[u8]) -> Theme {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        crate::host::assets::set_theme_webp(bytes.to_vec());
        return Theme::Image;
    }
    if bytes.len() >= 4 && bytes[0..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        if let Ok(decoded) = zstd::decode_all(bytes) {
            return install_decoded_theme(decoded);
        }
    }
    let n = bytes.len().min(16);
    if n > 0
        && bytes[..n]
            .iter()
            .all(|b| (0x20..=0x7e).contains(b) || matches!(b, b'\n' | b'\r' | b'\t'))
    {
        crate::host::assets::set_theme_css(bytes.to_vec());
        return Theme::Css;
    }
    crate::host::assets::set_theme_webp(bytes.to_vec());
    Theme::Image
}

fn install_decoded_theme(decoded: Vec<u8>) -> Theme {
    let trimmed = decoded
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|i| decoded[i])
        .unwrap_or(0);
    if trimmed == b'<' {
        crate::host::assets::set_html_override(decoded);
        Theme::Html
    } else {
        crate::host::assets::set_theme_css(decoded);
        Theme::Css
    }
}

pub fn mirrorc_target(app_name: &str) -> String {
    format!("KachinaInstaller_MirrorChyanCDK_{app_name}")
}

pub(crate) async fn settings_from_input(
    input: &SessionInput,
    args: &InstallArgs,
    config: &InstallerConfig,
) -> anyhow::Result<(Settings, ProjectConfig)> {
    let project = config
        .embedded_config
        .as_ref()
        .ok_or_else(|| anyhow::Error::from(Coded::bare(PKG_BROKEN)))
        .and_then(ProjectConfig::from_value)?;
    let inspected =
        crate::installer::inspect_dir(input.install_path.clone(), project.exe_name.clone())
            .await
            .ok_or_else(|| {
                anyhow::Error::from(Coded::bare(crate::utils::code::INSTALL_PATH_INVALID))
            })?;
    Ok((
        Settings {
            install_path: input.install_path.clone(),
            source_uri: input.source_uri.clone(),
            create_lnk: input.create_lnk,
            delete_user_data: input.delete_user_data,
            mirrorc_cdk: input.mirrorc_cdk.clone().or(args.mirrorc_cdk.clone()),
            online: args.online,
            silent: args.silent,
            non_interactive: args.non_interactive,
            dump_dir: args.dump_dir.clone(),
            dfs_extras: args.dfs_extras.clone(),
            elevate: elevate_from_state(&inspected.state, &project.uac_strategy),
            is_update: inspected.upgrade,
            auto_answer: args.silent || args.non_interactive,
        },
        project,
    ))
}

fn settings_from_gui(gui: &GuiRuntime, args: &InstallArgs) -> Settings {
    let sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
    let st = &sess.state;
    Settings {
        install_path: st.options.install_path.clone(),
        source_uri: st.options.source_uri.clone(),
        create_lnk: st.options.create_lnk,
        delete_user_data: st.options.delete_user_data,
        mirrorc_cdk: st.options.mirrorc_cdk.clone(),
        online: args.online,
        silent: args.silent,
        non_interactive: args.non_interactive,
        dump_dir: args.dump_dir.clone(),
        dfs_extras: args.dfs_extras.clone(),
        elevate: st.needs_elevate,
        is_update: matches!(st.mode, Mode::Update),
        auto_answer: args.silent || args.non_interactive,
    }
}

fn apply_locked(gui: &GuiRuntime, intent: Intent) {
    gui.session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .apply(intent);
}

pub async fn handle_intent(
    intent: Intent,
    ctx: &crate::host::HostCtx,
    handle: &HostHandle,
) -> TAResult<Value> {
    let gui = ctx
        .gui
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| TACommandError::new(anyhow::anyhow!("gui session not ready")))?;
    match intent {
        Intent::Start => handle_start(gui, ctx, handle).await,
        Intent::SetCdk { cdk, uri } => {
            handle_set_cdk(gui, cdk, uri, handle).await;
            ok(())
        }
        Intent::CancelCdk => {
            handle_cancel_cdk(&gui, handle);
            ok(())
        }
        Intent::SetSource { uri } => {
            restore_abandoned_cdk(&gui);
            apply_locked(&gui, Intent::SetSource { uri });
            gui.emit(handle);
            ok(())
        }
        Intent::Answer { id, ok: accepted } => {
            ctx.session.prompts.answer(&id, accepted).await;
            apply_locked(&gui, Intent::Answer { id, ok: accepted });
            gui.emit(handle);
            ok(())
        }
        Intent::Launch => {
            if let Some(project) = gui.project.as_ref() {
                let path = gui.snapshot().options.install_path;
                let full = Path::new(&path).join(&project.exe_name);
                crate::installer::launch(full.to_string_lossy().into_owned()).await;
            }
            handle.close();
            ok(())
        }
        Intent::Close => {
            handle.close();
            ok(())
        }
        Intent::Dismiss if gui.fatal => {
            handle.close();
            ok(())
        }
        Intent::Cancel => {
            if gui.running.load(Ordering::SeqCst) {
                gui.cancel_running();
                gui.emit(handle);
            }
            ok(())
        }
        Intent::SetPath { path } => {
            apply_locked(&gui, Intent::SetPath { path });
            gui.emit(handle);
            ok(())
        }
        other => {
            apply_locked(&gui, other);
            gui.emit(handle);
            ok(())
        }
    }
}

async fn handle_start(
    gui: Arc<GuiRuntime>,
    ctx: &crate::host::HostCtx,
    handle: &HostHandle,
) -> TAResult<Value> {
    if gui.running.swap(true, Ordering::SeqCst) {
        return ok(());
    }
    let cancel = gui.fresh_cancel();
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.apply(Intent::Start);
        if matches!(sess.state.phase, Phase::Failed(_)) {
            drop(sess);
            gui.running.store(false, Ordering::SeqCst);
            gui.emit(handle);
            return ok(());
        }
        sess.state.phase = Phase::Running(Progress::new(
            crate::session::state::ProgressStage::Prepare,
            Some(0),
            Some(0.0),
        ));
    }
    gui.emit(handle);

    let Some(project) = gui.project.clone() else {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.state.phase = Phase::Failed(Coded::bare(PKG_BROKEN));
        drop(sess);
        gui.running.store(false, Ordering::SeqCst);
        gui.emit(handle);
        return ok(());
    };

    let settings = settings_from_gui(&gui, &ctx.args);
    let base = gui.snapshot();
    let uninstall = matches!(base.mode, Mode::Uninstall);
    let ui = GuiUi::new(
        handle.clone(),
        ctx.session.prompts.clone(),
        ctx.session.plugins.clone(),
        settings.auto_answer,
        gui.session.clone(),
        cancel,
    );
    let result = if uninstall {
        run_uninstall(&settings, &gui.config, &project, &ui, &base, &ctx.elevate).await
    } else {
        run_install(&settings, &gui.config, &project, &ui, &base, &ctx.elevate).await
    };
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.state.pending = None;
        match result {
            Ok(r) if r.cancelled => sess.state.phase = Phase::Ready,
            Ok(r) => sess.state.phase = Phase::Done(r),
            Err(err) => {
                let coded = coded_from_error(&err);
                let event_id = TACommandError::new(err).report_if_needed();
                sess.state.phase = match coded {
                    Some(mut coded) => {
                        coded.event_id = event_id;
                        Phase::Failed(coded)
                    }
                    None => Phase::Ready,
                };
            }
        }
    }
    gui.running.store(false, Ordering::SeqCst);
    gui.emit(handle);
    ok(())
}

/// Check a CDK against a Mirror酱 URI without committing options.
pub(crate) async fn verify_mirrorc_cdk(uri: &str, cdk: &str) -> Result<(), Coded> {
    let status: anyhow::Result<Value> = match parse_source(uri) {
        Ok(ParsedSource::Mirrorc {
            resource_id,
            channel,
            arch,
            os,
        }) => crate::thirdparty::mirrorc::get_mirrorc_status(
            &resource_id,
            "",
            cdk,
            &channel,
            arch.as_deref(),
            os.as_deref(),
        )
        .await
        .into_anyhow()
        .map_err(|e| e.attach(MIRRORC_UNREACHABLE)),
        Ok(_) => Err(anyhow::Error::from(Coded::bare_with(
            MIRRORC_CONFIG_INVALID,
            uri,
        ))),
        Err(err) => Err(err),
    };
    match status {
        Ok(value) => {
            if let Some(coded) = coded_for_mirrorc_response(&value) {
                Err(coded)
            } else {
                Ok(())
            }
        }
        Err(err) => Err(coded_from_error(&err).unwrap_or_else(|| Coded::bare(MIRRORC_UNREACHABLE))),
    }
}

async fn handle_set_cdk(
    gui: Arc<GuiRuntime>,
    cdk: String,
    uri: Option<String>,
    handle: &HostHandle,
) {
    let committed = gui.snapshot().options.source_uri;
    let verify_uri = uri.filter(|u| !u.is_empty()).unwrap_or_else(|| committed);
    if cdk.is_empty() {
        commit_empty_cdk(&gui, &verify_uri);
        gui.emit(handle);
        return;
    }
    if !verify_uri.starts_with("mirrorc://") {
        gui.emit(handle);
        return;
    }
    let epoch = gui.cdk_epoch.load(Ordering::SeqCst);
    let current = gui.snapshot().cdk;
    remember_cdk_snapshot(&gui, &current);
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.state.cdk = CdkStatus::Checking;
    }
    gui.emit(handle);
    let status = verify_mirrorc_cdk(&verify_uri, &cdk).await;
    commit_verified_cdk(&gui, epoch, &verify_uri, &cdk, status);
    gui.emit(handle);
}

/// 空 CDK 仍提交 Mirror 源，并清掉凭据。世代号先增加，再按与取消相同的锁顺序写入。
fn commit_empty_cdk(gui: &GuiRuntime, verify_uri: &str) {
    gui.cdk_epoch.fetch_add(1, Ordering::SeqCst);
    let mut restore = gui.cdk_restore.lock().unwrap_or_else(|e| e.into_inner());
    *restore = None;
    let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
    if verify_uri.starts_with("mirrorc://") {
        sess.apply(Intent::SetSource {
            uri: verify_uri.to_string(),
        });
    }
    sess.apply(Intent::SetCdk {
        cdk: String::new(),
        uri: None,
    });
    sess.state.cdk = CdkStatus::Idle;
    if let Some(project) = gui.project.as_ref() {
        let _ = crate::utils::wincred::wincred_delete(&mirrorc_target(&project.app_name));
    }
}

/// 持有 `cdk_restore` 再持有 `session` 时才读世代号。对不上则不改源、不写凭据。
/// 成功时在释放这两把锁之前清掉快照，之后的取消不能撤掉这次提交。
fn commit_verified_cdk(
    gui: &GuiRuntime,
    epoch: u64,
    verify_uri: &str,
    cdk: &str,
    status: Result<(), Coded>,
) -> bool {
    let mut restore = gui.cdk_restore.lock().unwrap_or_else(|e| e.into_inner());
    let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
    if gui.cdk_epoch.load(Ordering::SeqCst) != epoch {
        return false;
    }
    match status {
        Ok(()) => {
            sess.apply(Intent::SetSource {
                uri: verify_uri.to_string(),
            });
            sess.apply(Intent::SetCdk {
                cdk: cdk.to_string(),
                uri: None,
            });
            sess.state.cdk = CdkStatus::Ok;
            *restore = None;
            if let Some(project) = gui.project.as_ref() {
                let _ = crate::utils::wincred::wincred_write(
                    &mirrorc_target(&project.app_name),
                    cdk,
                    "MirrorChyan CDK",
                );
            }
            true
        }
        Err(coded) => {
            sess.state.cdk = CdkStatus::Invalid(coded);
            false
        }
    }
}

/// Keep the last committed CDK status for this edit so cancel can put it back.
/// `Checking` is not committed; a later retry must not replace `Ok` with `Invalid`.
fn remember_cdk_snapshot(gui: &GuiRuntime, current: &CdkStatus) {
    if matches!(current, CdkStatus::Checking) {
        return;
    }
    let mut restore = gui.cdk_restore.lock().unwrap_or_else(|e| e.into_inner());
    if restore.is_none() {
        *restore = Some(current.clone());
    }
}

/// Invalidate an in-flight CDK verify and restore the committed status, if any.
fn restore_abandoned_cdk(gui: &GuiRuntime) {
    gui.cdk_epoch.fetch_add(1, Ordering::SeqCst);
    let restore = gui
        .cdk_restore
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(prev) = restore {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.state.cdk = prev;
    }
}

fn handle_cancel_cdk(gui: &GuiRuntime, handle: &HostHandle) {
    restore_abandoned_cdk(gui);
    gui.emit(handle);
}

fn ok<T: serde::Serialize>(value: T) -> TAResult<Value> {
    serde_json::to_value(value).map_err(|e| TACommandError::new(anyhow::anyhow!(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_config() -> InstallerConfig {
        InstallerConfig {
            install_path: String::new(),
            install_path_exists: false,
            install_path_source: "",
            is_uninstall: false,
            embedded_files: None,
            embedded_index: None,
            embedded_config: None,
            enbedded_metadata: None,
            embedded_image: None,
            exe_path: String::new(),
            args: InstallArgs::default(),
            elevated: false,
            preset: None,
        }
    }

    fn gui_with(cdk: CdkStatus, committed: Option<&str>, uri: &str) -> Arc<GuiRuntime> {
        let mut state = UiState::default();
        state.cdk = cdk;
        state.options.mirrorc_cdk = committed.map(str::to_string);
        state.options.source_uri = uri.into();
        Arc::new(GuiRuntime {
            session: Arc::new(Mutex::new(UiSession::with_renderer(
                state,
                Renderer::WebView,
            ))),
            config: Arc::new(dummy_config()),
            project: None,
            running: AtomicBool::new(false),
            fatal: false,
            cancel: Mutex::new(tokio_util::sync::CancellationToken::new()),
            cdk_epoch: AtomicU64::new(0),
            cdk_restore: Mutex::new(None),
        })
    }

    #[test]
    fn cancel_after_failed_verify_restores_committed_cdk() {
        let gui = gui_with(CdkStatus::Ok, Some("old"), "mirrorc://rid");
        remember_cdk_snapshot(&gui, &CdkStatus::Ok);
        {
            let mut sess = gui.session.lock().unwrap();
            sess.state.cdk = CdkStatus::Invalid(Coded::bare("MIRRORC_CDK_INVALID"));
        }
        restore_abandoned_cdk(&gui);
        let snap = gui.snapshot();
        assert_eq!(snap.cdk, CdkStatus::Ok);
        assert_eq!(snap.options.mirrorc_cdk.as_deref(), Some("old"));
    }

    #[test]
    fn source_change_drops_in_flight_verify() {
        let gui = gui_with(CdkStatus::Ok, Some("old"), "mirrorc://rid");
        let epoch = gui.cdk_epoch.load(Ordering::SeqCst);
        remember_cdk_snapshot(&gui, &CdkStatus::Ok);
        {
            let mut sess = gui.session.lock().unwrap();
            sess.state.cdk = CdkStatus::Checking;
        }
        restore_abandoned_cdk(&gui);
        assert_ne!(gui.cdk_epoch.load(Ordering::SeqCst), epoch);
        assert_eq!(gui.snapshot().cdk, CdkStatus::Ok);
        assert_eq!(gui.snapshot().options.mirrorc_cdk.as_deref(), Some("old"));
    }

    #[test]
    fn retry_keeps_the_original_committed_snapshot() {
        let gui = gui_with(CdkStatus::Ok, Some("old"), "mirrorc://rid");
        remember_cdk_snapshot(&gui, &CdkStatus::Ok);
        remember_cdk_snapshot(
            &gui,
            &CdkStatus::Invalid(Coded::bare("MIRRORC_CDK_INVALID")),
        );
        restore_abandoned_cdk(&gui);
        assert_eq!(gui.snapshot().cdk, CdkStatus::Ok);
    }

    #[test]
    fn stale_verify_does_not_commit_source() {
        let gui = gui_with(CdkStatus::Ok, Some("old"), "https://example.com/app.json");
        let epoch = gui.cdk_epoch.load(Ordering::SeqCst);
        remember_cdk_snapshot(&gui, &CdkStatus::Ok);
        restore_abandoned_cdk(&gui);
        let committed = commit_verified_cdk(&gui, epoch, "mirrorc://rid", "new", Ok(()));
        assert!(!committed);
        let snap = gui.snapshot();
        assert_eq!(snap.options.source_uri, "https://example.com/app.json");
        assert_eq!(snap.options.mirrorc_cdk.as_deref(), Some("old"));
        assert_eq!(snap.cdk, CdkStatus::Ok);
    }

    #[test]
    fn fresh_verify_commits_source_and_ignores_a_later_cancel() {
        let gui = gui_with(CdkStatus::Ok, Some("old"), "https://example.com/app.json");
        remember_cdk_snapshot(&gui, &CdkStatus::Ok);
        let epoch = gui.cdk_epoch.load(Ordering::SeqCst);
        assert!(commit_verified_cdk(
            &gui,
            epoch,
            "mirrorc://rid",
            "new",
            Ok(())
        ));
        restore_abandoned_cdk(&gui);
        let snap = gui.snapshot();
        assert_eq!(snap.options.source_uri, "mirrorc://rid");
        assert_eq!(snap.options.mirrorc_cdk.as_deref(), Some("new"));
        assert_eq!(snap.cdk, CdkStatus::Ok);
    }

    #[test]
    fn checking_is_not_stored_as_the_committed_snapshot() {
        let gui = gui_with(CdkStatus::Checking, Some("old"), "mirrorc://rid");
        remember_cdk_snapshot(&gui, &CdkStatus::Checking);
        assert!(gui.cdk_restore.lock().unwrap().is_none());
    }

    #[test]
    fn visible_sources_keeps_hidden_entries() {
        let project = ProjectConfig::from_value(&serde_json::json!({
            "source": [
                {"id": "http", "name": "HTTP", "uri": "https://example.com/a.json"},
                {
                    "id": "stub",
                    "name": "Stub",
                    "uri": "plugin-stub+http://x",
                    "hidden": true
                }
            ],
            "appName": "A",
            "publisher": "P",
            "regName": "A",
            "exeName": "a.exe",
            "uninstallName": "uninst.exe",
            "updaterName": "update.exe",
            "programFilesPath": "A",
            "title": "T",
            "description": "D",
            "windowTitle": "W"
        }))
        .unwrap();
        let sources = visible_sources(&project, "");
        assert_eq!(sources.len(), 2);
        assert!(sources[1].hidden);
        assert!(!sources[0].hidden);
    }
}
