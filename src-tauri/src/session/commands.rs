use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::path::Path;

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
use crate::session::types::{
    elevate_from_state, SessionInput, Settings, SourceField,
};
use crate::session::ui::{GuiUi, PluginHub, PromptHub};
use crate::session::ProjectConfig;
use crate::utils::code::{
    extract, code_for_mirrorc_status, Coded, Extracted, INTERNAL_ERROR,
    PKG_BROKEN, UNINSTALL_INFO_MISSING,
};
use crate::utils::error::{TACommandError, TAResult};

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
}

impl GuiRuntime {
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
    crate::APP_BOOT_SIGNAL.store(true, Ordering::SeqCst);
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
        project.source_uri(args.source.as_deref()).unwrap_or_default()
    };
    let all_sources = visible_sources(&project, &source_uri);
    if !all_sources.is_empty() && !all_sources.iter().any(|s| s.uri == source_uri) {
        source_uri = all_sources[0].uri.clone();
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
    if cdk.is_none() && source_uri.starts_with("mirrorc://") {
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
    sess.apply(Intent::SetPath {
        path: install_path,
    });
    if is_uninstall {
        sess.state.mode = Mode::Uninstall;
    }
    if let Some(coded) = failed {
        sess.state.phase = Phase::Failed(coded);
    }

    Arc::new(GuiRuntime {
        session: Arc::new(Mutex::new(sess)),
        config: Arc::new(config),
        project: Some(Arc::new(project)),
        running: AtomicBool::new(false),
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
        session: Arc::new(Mutex::new(UiSession::with_renderer(state, Renderer::WebView))),
        config: Arc::new(config),
        project: None,
        running: AtomicBool::new(false),
    })
}

fn failed_runtime_with_config(config: InstallerConfig, coded: Coded) -> Arc<GuiRuntime> {
    let mut state = UiState::default();
    state.phase = Phase::Failed(coded);
    state.project.lang = crate::utils::i18n::lang().to_string();
    Arc::new(GuiRuntime {
        session: Arc::new(Mutex::new(UiSession::with_renderer(state, Renderer::WebView))),
        config: Arc::new(config),
        project: None,
        running: AtomicBool::new(false),
    })
}

pub(crate) fn visible_sources(project: &ProjectConfig, current_uri: &str) -> Vec<SourceItem> {
    match &project.source {
        SourceField::Single(uri) => vec![SourceItem {
            id: "default".into(),
            name: String::new(),
            uri: uri.clone(),
            icon: None,
            requires_webview: needs_js_plugin(uri),
        }],
        SourceField::List(list) => list
            .iter()
            .filter(|s| !s.hidden || s.uri == current_uri)
            .map(|s| SourceItem {
                id: s.id.clone(),
                name: s.name.clone(),
                uri: s.uri.clone(),
                icon: s.icon.clone(),
                requires_webview: needs_js_plugin(&s.uri),
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
            .ok_or_else(|| anyhow::Error::from(Coded::bare(crate::utils::code::INSTALL_PATH_INVALID)))?;
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
        Intent::SetCdk { cdk } => {
            handle_set_cdk(gui, cdk, handle).await;
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
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.apply(Intent::Start);
        if matches!(sess.state.phase, Phase::Failed(_)) {
            drop(sess);
            gui.running.store(false, Ordering::SeqCst);
            gui.emit(handle);
            return ok(());
        }
        sess.state.phase = Phase::Running(Progress {
            sub_step: 0,
            percent: 0.0,
            stage: "prepare",
            subject: None,
            done: None,
            total: None,
        });
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
    let uninstall = matches!(gui.snapshot().mode, Mode::Uninstall);
    let ui = GuiUi::new(
        handle.clone(),
        ctx.session.prompts.clone(),
        ctx.session.plugins.clone(),
        settings.auto_answer,
        gui.session.clone(),
    );
    let result = if uninstall {
        run_uninstall(&settings, &gui.config, &project, &ui, &ctx.elevate).await
    } else {
        run_install(&settings, &gui.config, &project, &ui, &ctx.elevate).await
    };
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(r) if r.cancelled => {
                sess.state.phase = Phase::Ready;
                sess.state.pending = None;
            }
            Ok(r) => {
                sess.state.phase = Phase::Done(r);
                sess.state.pending = None;
            }
            Err(err) => {
                let coded = coded_from_error(&err);
                TACommandError::new(err).report_if_needed();
                sess.state.phase = Phase::Failed(coded);
            }
        }
    }
    gui.running.store(false, Ordering::SeqCst);
    gui.emit(handle);
    ok(())
}

async fn handle_set_cdk(gui: Arc<GuiRuntime>, cdk: String, handle: &HostHandle) {
    apply_locked(&gui, Intent::SetCdk { cdk: cdk.clone() });
    let uri = gui.snapshot().options.source_uri;
    if cdk.is_empty() {
        if let Some(project) = gui.project.as_ref() {
            let _ = crate::utils::wincred::wincred_delete(&mirrorc_target(&project.app_name));
        }
        {
            let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
            sess.state.cdk = CdkStatus::Idle;
        }
        gui.emit(handle);
        return;
    }
    if !uri.starts_with("mirrorc://") {
        gui.emit(handle);
        return;
    }
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.state.cdk = CdkStatus::Checking;
    }
    gui.emit(handle);
    let parsed = parse_source(&uri);
    let status = match parsed {
        Ok(ParsedSource::Mirrorc {
            resource_id,
            channel,
            arch,
            os,
        }) => {
            crate::thirdparty::mirrorc::get_mirrorc_status(
                &resource_id,
                "",
                &cdk,
                &channel,
                arch.as_deref(),
                os.as_deref(),
            )
            .await
        }
        _ => Err(TACommandError::new(anyhow::anyhow!("invalid mirrorc uri"))),
    };
    {
        let mut sess = gui.session.lock().unwrap_or_else(|e| e.into_inner());
        match status {
            Ok(value) => {
                if let Some(coded) = mirrorc_status_coded(&value) {
                    sess.state.cdk = CdkStatus::Invalid(coded);
                } else {
                    sess.state.cdk = CdkStatus::Ok;
                    if let Some(project) = gui.project.as_ref() {
                        let _ = crate::utils::wincred::wincred_write(
                            &mirrorc_target(&project.app_name),
                            &cdk,
                            "MirrorChyan CDK",
                        );
                    }
                }
            }
            Err(err) => {
                let mut coded = Coded::bare(crate::utils::code::MIRRORC_UNREACHABLE);
                coded.detail = Some(format!("{:#}", err.error));
                sess.state.cdk = CdkStatus::Invalid(coded);
            }
        }
    }
    gui.emit(handle);
}

fn mirrorc_status_coded(status: &Value) -> Option<Coded> {
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

fn coded_from_error(err: &anyhow::Error) -> Coded {
    match extract(err) {
        Extracted::Coded(c) => c.clone(),
        Extracted::Cancelled => Coded::bare("cancelled"),
        Extracted::Uncoded { detail } => {
            let mut c = Coded::bare(INTERNAL_ERROR);
            if !detail.is_empty() {
                c.detail = Some(detail);
            }
            c
        }
    }
}

fn ok<T: serde::Serialize>(value: T) -> TAResult<Value> {
    serde_json::to_value(value).map_err(|e| TACommandError::new(anyhow::anyhow!(e)))
}
