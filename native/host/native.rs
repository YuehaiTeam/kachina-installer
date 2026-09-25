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
use crate::session::commands::{
    mirrorc_target, settings_from_ui, verify_mirrorc_cdk, visible_sources,
};
use crate::session::run::{run_install, run_uninstall};
use crate::session::source::needs_js_plugin;
use crate::session::state::{
    CancelState, CdkStatus, Intent, Mode, Options, Phase, Progress, ProgressUnit, Prompt, Renderer,
    UiSession, UiState,
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
    prompt_text, show_error, show_error_coded, show_ready, task_dialog, CommandLink, ErrorDialog,
    ProgressDialog, ProgressHwnd, ReadySpec, TaskDialogRequest, ID_ADVANCED, ID_CHANGE_PATH,
    ID_CLOSE, ID_INSTALL, ID_LAUNCH, ID_RADIO_BASE,
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
    if crate::fs::staging::enter_neutral_cwd().is_err() {
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
                if !ensure_mirrorc_cdk(&project, &mut sess, false, None).await {
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
                    let _ = ensure_mirrorc_cdk(&project, &mut sess, true, None).await;
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
    if cdk.is_none() {
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
        offline: config.embedded_index.is_some(),
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
        config.install_path.clone(),
        project.reg_name.clone(),
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

fn apply_cdk(sess: &mut UiSession, cdk: String) {
    sess.apply(Intent::SetCdk { cdk, uri: None });
}

fn has_cdk(sess: &UiSession) -> bool {
    !sess
        .state
        .options
        .mirrorc_cdk
        .as_deref()
        .unwrap_or("")
        .is_empty()
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
        // 只要可见源非空而当前选择不在其中就必须改选——与是否显示单选框
        // 无关；否则过滤后恰好剩一个源时会带着不可用的源直接进 Start。
        if !sources.is_empty()
            && !sources
                .iter()
                .any(|s| s.uri == sess.state.options.source_uri)
        {
            sess.apply(Intent::SetSource {
                uri: sources
                    .iter()
                    .find(|s| !s.hidden)
                    .or(sources.first())
                    .unwrap()
                    .uri
                    .clone(),
            });
        }
        let listed: Vec<_> = sources
            .iter()
            .filter(|s| !s.hidden || s.uri == sess.state.options.source_uri)
            .cloned()
            .collect();
        // 离线整包不提供源选择：切到需要联网/CDK 的源没有意义。
        let show_radios = listed.len() > 1 && !sess.state.offline;
        let default_radio = if show_radios {
            listed
                .iter()
                .position(|s| s.uri == sess.state.options.source_uri)
                .map(|i| ID_RADIO_BASE + i as i32)
                .unwrap_or(0)
        } else {
            0
        };
        let radios = if show_radios {
            listed
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
            needs_elevate: sess.state.needs_elevate,
        };
        let result = tokio::task::spawn_blocking(move || show_ready(spec))
            .await
            .context("ready dialog")??;

        // TaskDialog 单选在按按钮时才返回。Mirror 源只在安装/更新确认且 CDK
        // 校验通过后提交；改路径、高级模式不带上这个候选。卸载不下载，单选直接提交。
        // CDK 取消时本次勾选也不写入。
        if show_radios {
            if let Some(src) = listed
                .iter()
                .enumerate()
                .find(|(i, _)| ID_RADIO_BASE + *i as i32 == result.radio)
                .map(|(_, s)| s)
            {
                let mirrorc_switch =
                    src.uri.starts_with("mirrorc://") && src.uri != sess.state.options.source_uri;
                let confirm_install =
                    result.button == ID_INSTALL && !matches!(sess.state.mode, Mode::Uninstall);
                if mirrorc_switch && confirm_install {
                    if !ensure_mirrorc_cdk(project, sess, false, Some(src.uri.as_str())).await {
                        continue;
                    }
                } else if !mirrorc_switch || result.button == ID_INSTALL {
                    sess.apply(Intent::SetSource {
                        uri: src.uri.clone(),
                    });
                }
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

async fn ensure_mirrorc_cdk(
    project: &ProjectConfig,
    sess: &mut UiSession,
    force: bool,
    candidate_uri: Option<&str>,
) -> bool {
    let uri = candidate_uri
        .unwrap_or(sess.state.options.source_uri.as_str())
        .to_string();
    if !uri.starts_with("mirrorc://") {
        return true;
    }
    let already_this = uri == sess.state.options.source_uri;

    if !force && already_this {
        if !has_cdk(sess) {
            if let Ok(cdk) = crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name))
            {
                apply_cdk(sess, cdk);
            }
        }
        if has_cdk(sess) {
            sess.state.cdk = CdkStatus::Ok;
            return true;
        }
    }

    let mut initial = sess.state.options.mirrorc_cdk.clone().unwrap_or_default();
    if initial.is_empty() {
        if let Ok(cdk) = crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name)) {
            initial = cdk;
        }
    }

    loop {
        let title = t(&sess.state, "dialog.mirrorc_cdk_title");
        let prompt = t(&sess.state, "dialog.mirrorc_cdk_placeholder");
        let typed = initial.clone();
        let key = tokio::task::spawn_blocking(move || prompt_text(&title, &prompt, &typed))
            .await
            .ok()
            .flatten();
        match key {
            Some(key) if key.is_empty() => {
                // 空输入只清当前已提交的 Mirror 源。候选切换不删凭据，也不改源。
                if already_this {
                    let _ =
                        crate::utils::wincred::wincred_delete(&mirrorc_target(&project.app_name));
                    apply_cdk(sess, String::new());
                    sess.state.cdk = CdkStatus::Idle;
                }
                return false;
            }
            Some(key) => match verify_mirrorc_cdk(&uri, &key).await {
                Ok(()) => {
                    sess.apply(Intent::SetSource { uri: uri.clone() });
                    apply_cdk(sess, key.clone());
                    sess.state.cdk = CdkStatus::Ok;
                    let _ = crate::utils::wincred::wincred_write(
                        &mirrorc_target(&project.app_name),
                        &key,
                        "MirrorChyan CDK",
                    );
                    return true;
                }
                Err(coded) => {
                    show_error_coded(&coded, desktop_hwnd());
                    initial = key;
                }
            },
            None => return false,
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
        Intent::Start => native_session(args, config, project, sess).await,
        _ => Ok(NativeOutcome::Exit),
    }
}

async fn native_session(
    args: InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
    sess: &mut UiSession,
) -> anyhow::Result<NativeOutcome> {
    if let Err(coded) = sess.registry.clone() {
        show_error_coded(&coded, desktop_hwnd());
        sess.state.phase = Phase::Failed(coded);
        sess.apply(Intent::Dismiss);
        return Ok(NativeOutcome::Again {
            reopen_source: false,
        });
    }
    let settings = settings_from_ui(sess, &args);
    let heading = match sess.state.mode {
        Mode::Uninstall => t(&sess.state, "ready.uninstalling"),
        Mode::Update => t(&sess.state, "ready.updating"),
        Mode::Install => t(&sess.state, "ready.installing"),
    };
    let prepare = t(&sess.state, "progress.prepare");
    sess.state.phase = Phase::Running(Progress::new(
        crate::session::state::ProgressStage::Prepare,
        Some(0),
        Some(0.0),
    ));
    let cancel = tokio_util::sync::CancellationToken::new();
    let dialog = ProgressDialog::show_with_cancel(
        &project.window_title,
        &heading,
        &prepare,
        false,
        Some(cancel.clone()),
    )
    .await?;
    let ui = NativeUi::new(dialog.hwnd_arc(), cancel);
    ui.state(&sess.state);
    let mgr = ManagedElevate::new();
    let uninstall = matches!(sess.state.mode, Mode::Uninstall);
    let result = if uninstall {
        run_uninstall(&settings, config, project, &ui, &sess.state, &mgr).await
    } else {
        run_install(&settings, config, project, &ui, &sess.state, &mgr).await
    };
    mgr.close().await;
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
        needs_elevate: false,
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
    cancel: tokio_util::sync::CancellationToken,
}

impl NativeUi {
    fn new(hwnd: Arc<ProgressHwnd>, cancel: tokio_util::sync::CancellationToken) -> Self {
        Self { hwnd, cancel }
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
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                        hwnd,
                        windows::Win32::UI::Controls::TDM_ENABLE_BUTTON.0 as u32,
                        Some(windows::Win32::Foundation::WPARAM(
                            windows::Win32::UI::WindowsAndMessaging::IDCANCEL.0 as usize,
                        )),
                        Some(windows::Win32::Foundation::LPARAM(
                            (p.cancel == CancelState::Available) as isize,
                        )),
                    );
                }
            }
        }
    }

    fn begin_commit(&self) -> bool {
        if let Some(hwnd) = self.hwnd.get() {
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    windows::Win32::UI::Controls::TDM_ENABLE_BUTTON.0 as u32,
                    Some(windows::Win32::Foundation::WPARAM(
                        windows::Win32::UI::WindowsAndMessaging::IDCANCEL.0 as usize,
                    )),
                    Some(windows::Win32::Foundation::LPARAM(0)),
                );
            }
        }
        !self.cancel.is_cancelled()
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

    fn cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancel.clone()
    }
}

fn progress_current(p: &Progress) -> String {
    let subject = p.subject.as_deref().unwrap_or_default();
    let mut text = if p.cancel == CancelState::Requested {
        i18n::t("running.cancelling", &[])
    } else {
        i18n::t(p.stage.i18n_key(), &[("subject", subject)])
    };
    if let Some(counter) = &p.summary {
        let fmt = |n| {
            if counter.unit == ProgressUnit::Bytes {
                i18n::format_size(n)
            } else {
                n.to_string()
            }
        };
        text.push_str(&format!("\n{}", fmt(counter.done)));
        if let Some(total) = counter.total {
            text.push_str(&format!(" / {}", fmt(total)));
        }
    }
    let speed = if p.network_pending {
        p.network_bps
    } else {
        p.processing_bps
    };
    if let Some(speed) = speed {
        text.push_str(&format!("  {}/s", i18n::format_size(speed)));
    }
    for file in &p.files {
        text.push_str(&format!(
            "\n{}  {}",
            file.name,
            i18n::t(&format!("file_action.{}", file.action.as_str()), &[])
        ));
        if let Some(bytes) = &file.bytes {
            text.push_str(&format!("  {}", i18n::format_size(bytes.done)));
            if let Some(total) = bytes.total {
                text.push_str(&format!(" / {}", i18n::format_size(total)));
            }
        }
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

fn set_progress_hwnd(hwnd: HWND, percent: Option<f64>, text: &str) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Controls::{
        TDE_CONTENT, TDM_SET_PROGRESS_BAR_POS, TDM_UPDATE_ELEMENT_TEXT,
    };
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let pos = percent.unwrap_or(0.0).round().clamp(0.0, 100.0) as usize;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            windows::Win32::UI::Controls::TDM_SET_MARQUEE_PROGRESS_BAR.0 as u32,
            Some(WPARAM(percent.is_none() as usize)),
            Some(LPARAM(0)),
        );
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            windows::Win32::UI::Controls::TDM_SET_PROGRESS_BAR_MARQUEE.0 as u32,
            Some(WPARAM(percent.is_none() as usize)),
            Some(LPARAM(30)),
        );
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
