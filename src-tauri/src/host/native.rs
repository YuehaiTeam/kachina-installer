use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, IDOK};

use crate::cli::arg::InstallArgs;
use crate::host::HwndParent;
use crate::installer::config::{resolve_installer_config, InstallerConfig};
use crate::ipc::manager::ManagedElevate;
use crate::session::commands::{mirrorc_target, settings_from_input, visible_sources};
use crate::session::run::{run_install, run_uninstall};
use crate::session::source::needs_js_plugin;
use crate::session::state::{
    CdkStatus, Intent, Mode, Options, Phase, Progress, Prompt, Renderer, UiSession, UiState,
    BYTE_STAGES,
};
use crate::session::types::{settings_from_cli, SessionInput};
use crate::session::ui::SessionUi;
use crate::session::ProjectConfig;
use crate::utils::code::{
    coded_from_error, extract, should_report_error, Coded, Extracted, MIRRORC_CDK_BANNED,
    MIRRORC_CDK_EXPIRED, MIRRORC_CDK_INVALID, MIRRORC_CDK_MISMATCH, MIRRORC_CDK_MISSING,
    PKG_BROKEN, TEMP_DIR_UNAVAILABLE, WEBVIEW2_REQUIRED,
};
use crate::utils::i18n;
use crate::utils::taskdialog::{
    prompt_text, show_error, show_error_coded, show_ready, task_dialog, CommandLink,
    ErrorDialog, ProgressDialog, ProgressHwnd, ReadySpec, TaskDialogRequest, ID_ADVANCED,
    ID_CHANGE_PATH, ID_CLOSE, ID_INSTALL, ID_LAUNCH, ID_RADIO_BASE,
};

pub enum NativeOutcome {
    Exit,
    Again {
        reopen_source: bool,
    },
    Web {
        args: InstallArgs,
        preset: SessionInput,
    },
}

pub async fn run(args: InstallArgs) -> anyhow::Result<NativeOutcome> {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
    }
    let temp_dir = std::env::temp_dir();
    if std::env::set_current_dir(&temp_dir).is_err() {
        show_error(ErrorDialog::code(TEMP_DIR_UNAVAILABLE), desktop_hwnd());
        return Ok(NativeOutcome::Exit);
    }

    let config = resolve_installer_config(args.clone(), true).await?;
    let project = match config.embedded_config.as_ref() {
        Some(value) => ProjectConfig::from_value(value)?,
        None => {
            show_error(ErrorDialog::code(PKG_BROKEN), desktop_hwnd());
            return Ok(NativeOutcome::Exit);
        }
    };

    let mut sess = ui_session_from(&args, &config, &project).await?;
    if args.non_interactive {
        let input = options_to_input(&sess.state.options);
        return match finish_action(Intent::Start, input, args, &config, &project, &mut sess).await?
        {
            NativeOutcome::Again { .. } => Ok(NativeOutcome::Exit),
            other => Ok(other),
        };
    }

    loop {
        let action = match show_ready_page(&project, &mut sess).await? {
            None => return Ok(NativeOutcome::Exit),
            Some(intent) => intent,
        };

        if matches!(action, Intent::Start)
            && !project.need_web_view2
            && needs_js_plugin(&sess.state.options.source_uri)
        {
            show_error(ErrorDialog::code(WEBVIEW2_REQUIRED), desktop_hwnd());
            continue;
        }

        if matches!(action, Intent::Start) {
            sess.apply(Intent::Start);
            if cdk_missing(&sess) {
                sess.apply(Intent::Dismiss);
                if !ensure_mirrorc_cdk(&project, &mut sess, false).await {
                    continue;
                }
                sess.apply(Intent::Start);
                if cdk_missing(&sess) {
                    continue;
                }
            } else if let Phase::Failed(c) = &sess.state.phase {
                let coded = c.clone();
                show_error_coded(&coded, desktop_hwnd());
                sess.apply(Intent::Dismiss);
                continue;
            }
        }

        let input = options_to_input(&sess.state.options);
        match finish_action(action, input, args.clone(), &config, &project, &mut sess).await? {
            NativeOutcome::Again { reopen_source } => {
                if reopen_source {
                    let _ = ensure_mirrorc_cdk(&project, &mut sess, true).await;
                }
                continue;
            }
            other => return Ok(other),
        }
    }
}

async fn ui_session_from(
    args: &InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
) -> anyhow::Result<UiSession> {
    let settings = settings_from_cli(args, config, project).await?;
    let mut cdk = settings.mirrorc_cdk;
    if cdk.is_none() && settings.source_uri.starts_with("mirrorc://") {
        cdk = crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name)).ok();
    }
    let is_uninstall = config.is_uninstall || args.uninstall;
    let install_path = if is_uninstall {
        config.install_path.clone()
    } else {
        settings.install_path.clone()
    };
    let cdk_status = if cdk.as_deref().unwrap_or("").is_empty() {
        CdkStatus::Idle
    } else {
        CdkStatus::Ok
    };
    let sources = visible_sources(project, &settings.source_uri);
    let state = UiState {
        phase: Phase::Ready,
        mode: if is_uninstall {
            Mode::Uninstall
        } else if settings.is_update {
            Mode::Update
        } else {
            Mode::Install
        },
        project: crate::session::state::ProjectView {
            window_title: project.window_title.clone(),
            title: project.title.clone(),
            description: project.description.clone(),
            borderless: project.window_borderless.unwrap_or(false),
            lang: i18n::lang().to_string(),
        },
        options: Options {
            install_path: install_path.clone(),
            source_uri: settings.source_uri,
            create_lnk: settings.create_lnk,
            delete_user_data: settings.delete_user_data,
            mirrorc_cdk: cdk.filter(|s| !s.is_empty()),
        },
        sources,
        path: crate::session::state::PathState {
            writable: crate::session::state::PathWritable::Writable,
            exists: false,
            upgrade: false,
        },
        needs_elevate: settings.elevate,
        cdk: cdk_status,
        theme: crate::session::state::Theme::None,
        pending: None,
    };
    let mut sess = UiSession::with_project(
        state,
        Renderer::Native,
        project.exe_name.clone(),
        project.app_name.clone(),
        project.uac_strategy.clone(),
    );
    sess.apply(Intent::SetPath { path: install_path });
    if is_uninstall {
        sess.state.mode = Mode::Uninstall;
    }
    Ok(sess)
}

fn options_to_input(options: &Options) -> SessionInput {
    SessionInput {
        install_path: options.install_path.clone(),
        source_uri: options.source_uri.clone(),
        create_lnk: options.create_lnk,
        delete_user_data: options.delete_user_data,
        mirrorc_cdk: options.mirrorc_cdk.clone(),
    }
}

fn t(state: &UiState, key: &str) -> String {
    i18n::catalog().t(&state.project.lang, key, &[])
}

fn desktop_hwnd() -> HWND {
    unsafe { GetDesktopWindow() }
}

fn cdk_missing(sess: &UiSession) -> bool {
    matches!(&sess.state.phase, Phase::Failed(c) if c.code == MIRRORC_CDK_MISSING)
}

fn apply_preset_to_args(args: &mut InstallArgs, project: &ProjectConfig, input: &SessionInput) {
    args.target = Some(PathBuf::from(&input.install_path));
    args.mirrorc_cdk = input.mirrorc_cdk.clone();
    if let crate::session::types::SourceField::List(list) = &project.source {
        if let Some(item) = list.iter().find(|s| s.uri == input.source_uri) {
            args.source = Some(item.id.clone());
        }
    }
}

async fn show_ready_page(
    project: &ProjectConfig,
    sess: &mut UiSession,
) -> anyhow::Result<Option<Intent>> {
    loop {
        let sources = sess.state.sources.clone();
        if sources.len() > 1
            && !sources
                .iter()
                .any(|s| s.uri == sess.state.options.source_uri)
        {
            sess.apply(Intent::SetSource {
                uri: sources[0].uri.clone(),
            });
        }
        let show_radios = sources.len() > 1;
        let default_radio = if show_radios {
            sources
                .iter()
                .position(|s| s.uri == sess.state.options.source_uri)
                .map(|i| ID_RADIO_BASE + i as i32)
                .unwrap_or(0)
        } else {
            0
        };
        let radios = if show_radios {
            sources
                .iter()
                .enumerate()
                .map(|(i, s)| CommandLink {
                    id: ID_RADIO_BASE + i as i32,
                    text: s.name.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };

        let verb_key = match sess.state.mode {
            Mode::Uninstall => "ready.uninstall",
            Mode::Update => "ready.update",
            Mode::Install => "ready.install",
        };
        let dest_key = match sess.state.mode {
            Mode::Uninstall => "ready.uninstall_from",
            Mode::Update => "ready.update_to",
            Mode::Install => "ready.install_to",
        };
        let dest = format!(
            "{} {}",
            t(&sess.state, dest_key),
            sess.state.options.install_path
        );
        let mut content = sess.state.project.description.clone();
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(&dest);

        let verb = t(&sess.state, verb_key);
        let mut links = vec![CommandLink {
            id: ID_INSTALL,
            text: if matches!(sess.state.mode, Mode::Uninstall) {
                verb
            } else {
                format!("{}\n{}", verb, t(&sess.state, "ready.continue"))
            },
        }];
        if !matches!(sess.state.mode, Mode::Uninstall) {
            links.push(CommandLink {
                id: ID_CHANGE_PATH,
                text: t(&sess.state, "ready.change_path"),
            });
        }
        links.push(CommandLink {
            id: ID_ADVANCED,
            text: format!(
                "{}\n{}",
                t(&sess.state, "ready.advanced"),
                t(&sess.state, "ready.advanced_hint")
            ),
        });

        let verification = match sess.state.mode {
            Mode::Uninstall => Some(t(&sess.state, "ready.delete_user_data")),
            Mode::Install => Some(t(&sess.state, "ready.create_lnk")),
            Mode::Update => None,
        };
        let verification_checked = match sess.state.mode {
            Mode::Uninstall => sess.state.options.delete_user_data,
            _ => sess.state.options.create_lnk,
        };

        let spec = ReadySpec {
            title: sess.state.project.window_title.clone(),
            instruction: sess.state.project.title.clone(),
            content,
            links,
            radios,
            default_radio,
            verification,
            verification_checked,
        };
        let result = tokio::task::spawn_blocking(move || show_ready(spec))
            .await
            .context("ready dialog")??;

        if show_radios {
            if let Some(src) = sources
                .iter()
                .enumerate()
                .find(|(i, _)| ID_RADIO_BASE + *i as i32 == result.radio)
                .map(|(_, s)| s)
            {
                sess.apply(Intent::SetSource {
                    uri: src.uri.clone(),
                });
            }
        }
        match sess.state.mode {
            Mode::Uninstall => sess.apply(Intent::SetDeleteUserData {
                value: result.verified,
            }),
            Mode::Install => sess.apply(Intent::SetCreateLnk {
                value: result.verified,
            }),
            Mode::Update => {}
        }

        match result.button {
            ID_INSTALL => return Ok(Some(Intent::Start)),
            ID_ADVANCED => return Ok(Some(Intent::Advanced)),
            ID_CHANGE_PATH => {
                if let Some(path) = pick_path(
                    &sess.state.options.install_path,
                    &project.exe_name,
                    &project.app_name,
                )
                .await
                {
                    sess.apply(Intent::SetPath { path });
                }
            }
            _ => return Ok(None),
        }
    }
}

async fn pick_path(current: &str, exe_name: &str, app_name: &str) -> Option<String> {
    let parent = HwndParent::from_hwnd(unsafe { GetDesktopWindow() });
    crate::installer::pick_install_path(current, exe_name, app_name, parent).await
}

async fn ensure_mirrorc_cdk(project: &ProjectConfig, sess: &mut UiSession, force: bool) -> bool {
    if !force {
        if sess
            .state
            .options
            .mirrorc_cdk
            .as_deref()
            .unwrap_or("")
            .is_empty()
        {
            if let Ok(cdk) = crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name))
            {
                sess.apply(Intent::SetCdk { cdk });
            }
        }
        if !sess
            .state
            .options
            .mirrorc_cdk
            .as_deref()
            .unwrap_or("")
            .is_empty()
        {
            sess.state.cdk = CdkStatus::Ok;
            return true;
        }
    }

    let initial = sess.state.options.mirrorc_cdk.clone().unwrap_or_default();
    let title = t(&sess.state, "dialog.mirrorc_cdk_title");
    let prompt = t(&sess.state, "dialog.mirrorc_cdk_placeholder");
    let key = tokio::task::spawn_blocking(move || prompt_text(&title, &prompt, &initial))
        .await
        .ok()
        .flatten();

    match key {
        Some(key) if key.is_empty() => {
            let _ = crate::utils::wincred::wincred_delete(&mirrorc_target(&project.app_name));
            sess.apply(Intent::SetCdk { cdk: String::new() });
            sess.state.cdk = CdkStatus::Idle;
            false
        }
        Some(key) => {
            let _ = crate::utils::wincred::wincred_write(
                &mirrorc_target(&project.app_name),
                &key,
                "MirrorChyan CDK",
            );
            sess.apply(Intent::SetCdk { cdk: key });
            sess.state.cdk = CdkStatus::Ok;
            true
        }
        None => {
            if sess
                .state
                .options
                .mirrorc_cdk
                .as_deref()
                .unwrap_or("")
                .is_empty()
            {
                false
            } else {
                sess.state.cdk = CdkStatus::Ok;
                true
            }
        }
    }
}

async fn finish_action(
    action: Intent,
    input: SessionInput,
    mut args: InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
    sess: &mut UiSession,
) -> anyhow::Result<NativeOutcome> {
    let to_web = project.need_web_view2 || matches!(action, Intent::Advanced);
    if to_web {
        if crate::module::wv2::install_webview2().await.is_err() {
            return Ok(NativeOutcome::Exit);
        }
        apply_preset_to_args(&mut args, project, &input);
        args.non_interactive = !matches!(action, Intent::Advanced);
        args.uninstall = matches!(sess.state.mode, Mode::Uninstall);
        return Ok(NativeOutcome::Web {
            args,
            preset: input,
        });
    }

    match action {
        Intent::Advanced => unreachable!(),
        Intent::Start => native_session(input, args, config, project, sess).await,
        _ => Ok(NativeOutcome::Exit),
    }
}

async fn native_session(
    input: SessionInput,
    args: InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
    sess: &mut UiSession,
) -> anyhow::Result<NativeOutcome> {
    let (settings, _) = settings_from_input(&input, &args, config).await?;
    let heading = match sess.state.mode {
        Mode::Uninstall => t(&sess.state, "ready.uninstalling"),
        Mode::Update => t(&sess.state, "ready.updating"),
        Mode::Install => t(&sess.state, "ready.installing"),
    };
    let prepare = t(&sess.state, "progress.prepare");
    sess.state.phase = Phase::Running(Progress {
        sub_step: 0,
        percent: 0.0,
        stage: "prepare",
        subject: None,
        done: None,
        total: None,
    });
    let dialog = ProgressDialog::show(&project.window_title, &heading, &prepare, false).await?;
    let ui = NativeUi::new(dialog.hwnd_arc());
    ui.state(&sess.state);
    let mgr = ManagedElevate::new();
    let uninstall = matches!(sess.state.mode, Mode::Uninstall);
    let result = if uninstall {
        run_uninstall(&settings, config, project, &ui, &sess.state, &mgr).await
    } else {
        run_install(&settings, config, project, &ui, &sess.state, &mgr).await
    };
    dialog.close().await;

    match result {
        Ok(result) if result.cancelled => {
            sess.state.phase = Phase::Ready;
            Ok(NativeOutcome::Again {
                reopen_source: false,
            })
        }
        Ok(result) => {
            sess.state.phase = Phase::Done(result);
            show_finish(&sess.state, &project.exe_name).await;
            Ok(NativeOutcome::Exit)
        }
        Err(err) => {
            let event_id = if should_report_error(&err) {
                Some(crate::utils::sentry::capture_anyhow(&err))
            } else {
                None
            };
            let reopen = cdk_should_reopen(&err);
            if let Some(mut coded) = coded_from_error(&err) {
                coded.event_id = event_id;
                show_error_coded(&coded, desktop_hwnd());
                sess.state.phase = Phase::Failed(coded);
                sess.apply(Intent::Dismiss);
            } else {
                sess.state.phase = Phase::Ready;
            }
            Ok(NativeOutcome::Again {
                reopen_source: reopen,
            })
        }
    }
}

async fn show_finish(state: &UiState, exe_name: &str) {
    let Phase::Done(result) = &state.phase else {
        return;
    };
    if result.cancelled {
        return;
    }
    let (instruction_key, launch) = if result.is_uninstall {
        ("done.uninstall", false)
    } else if result.already_latest {
        ("done.latest", true)
    } else if result.is_update {
        ("done.update", true)
    } else {
        ("done.install", true)
    };
    let mut links = Vec::new();
    if launch {
        links.push(CommandLink {
            id: ID_LAUNCH,
            text: t(state, "done.launch"),
        });
    }
    links.push(CommandLink {
        id: ID_CLOSE,
        text: t(state, "done.close"),
    });
    let spec = ReadySpec {
        title: state.project.window_title.clone(),
        instruction: t(state, instruction_key),
        content: String::new(),
        links,
        radios: Vec::new(),
        default_radio: 0,
        verification: None,
        verification_checked: false,
    };
    let result = tokio::task::spawn_blocking(move || show_ready(spec))
        .await
        .ok()
        .and_then(|r| r.ok());
    if result.is_some_and(|r| r.button == ID_LAUNCH) {
        let path = PathBuf::from(&state.options.install_path).join(exe_name);
        crate::installer::launch(path.to_string_lossy().to_string()).await;
    }
}

struct NativeUi {
    hwnd: Arc<ProgressHwnd>,
}

impl NativeUi {
    fn new(hwnd: Arc<ProgressHwnd>) -> Self {
        Self { hwnd }
    }

    fn parent(&self) -> HwndParent {
        HwndParent::from_hwnd(
            self.hwnd
                .get()
                .unwrap_or_else(|| unsafe { GetDesktopWindow() }),
        )
    }
}

fn cdk_should_reopen(err: &anyhow::Error) -> bool {
    match extract(err) {
        Extracted::Coded(c) => matches!(
            c.code,
            MIRRORC_CDK_EXPIRED | MIRRORC_CDK_INVALID | MIRRORC_CDK_MISMATCH | MIRRORC_CDK_BANNED
        ),
        _ => false,
    }
}

#[async_trait]
impl SessionUi for NativeUi {
    fn state(&self, state: &UiState) {
        if let Phase::Running(p) = &state.phase {
            if let Some(hwnd) = self.hwnd.get() {
                set_progress_hwnd(hwnd, p.percent, &progress_current(p));
            }
        }
    }

    async fn confirm(&self, prompt: Prompt) -> bool {
        let (title, message) = prompt_copy(&prompt);
        let parent = self.parent();
        let ok = i18n::t("dialog.ok", &[]);
        let cancel = i18n::t("dialog.cancel", &[]);
        tokio::task::spawn_blocking(move || {
            task_dialog(
                TaskDialogRequest {
                    title,
                    content: message,
                    expanded: None,
                    footer: None,
                    buttons: vec![
                        CommandLink {
                            id: IDOK.0,
                            text: ok,
                        },
                        CommandLink {
                            id: ID_CLOSE,
                            text: cancel,
                        },
                    ],
                },
                parent.hwnd(),
            ) == IDOK.0
        })
        .await
        .unwrap_or(false)
    }

    fn notify(&self, coded: &Coded) {
        let coded = coded.clone();
        let parent = self.parent();
        tokio::task::spawn_blocking(move || {
            show_error_coded(&coded, parent.hwnd());
        });
    }
}

/// Copy line from the table, plus a `done / total` line when the stage reports
/// one (bytes for `BYTE_STAGES`, item counts otherwise).
fn progress_current(p: &Progress) -> String {
    let subject = p.subject.clone().unwrap_or_default();
    let mut text = i18n::t(
        &format!("progress.{}", p.stage),
        &[("subject", subject.as_str())],
    );
    if let (Some(done), Some(total)) = (p.done, p.total) {
        let fmt = |n: u64| {
            if BYTE_STAGES.contains(&p.stage) {
                i18n::format_size(n)
            } else {
                n.to_string()
            }
        };
        text.push_str(&format!("\n{} / {}", fmt(done), fmt(total)));
    }
    text
}

fn prompt_copy(prompt: &Prompt) -> (String, String) {
    let items = prompt.items.join("\n");
    let mut owned: Vec<(String, String)> = vec![("items".into(), items)];
    for (k, v) in &prompt.params {
        owned.push(((*k).to_string(), v.clone()));
    }
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    (
        i18n::t(&format!("prompt.{}.title", prompt.kind), &params),
        i18n::t(&format!("prompt.{}.message", prompt.kind), &params),
    )
}

fn set_progress_hwnd(hwnd: HWND, percent: f64, text: &str) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Controls::{
        TDE_CONTENT, TDM_SET_PROGRESS_BAR_POS, TDM_UPDATE_ELEMENT_TEXT,
    };
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let pos = percent.round().clamp(0.0, 100.0) as usize;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            TDM_UPDATE_ELEMENT_TEXT.0 as u32,
            Some(WPARAM(TDE_CONTENT.0 as usize)),
            Some(LPARAM(wide.as_ptr() as isize)),
        );
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            TDM_SET_PROGRESS_BAR_POS.0 as u32,
            Some(WPARAM(pos)),
            Some(LPARAM(0)),
        );
    }
}
