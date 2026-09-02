use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

use crate::cli::arg::InstallArgs;
use crate::host::HwndParent;
use crate::installer::config::{resolve_installer_config, InstallerConfig};
use crate::installer::inspect_dir;
use crate::ipc::manager::ManagedElevate;
use crate::session::commands::settings_from_input;
use crate::session::run::{run_install, run_uninstall};
use crate::session::source::needs_js_plugin;
use crate::session::types::{
    settings_from_cli, SessionInput, SessionResult, SourceField, SourceItem,
};
use crate::session::state::{Phase, Prompt, UiState};
use crate::session::ui::{notice_from_error, notice_text, progress_current, prompt_copy, SessionUi};
use crate::utils::code::{
    extract, should_report_error, Coded, Extracted, MIRRORC_CDK_BANNED, MIRRORC_CDK_EXPIRED,
    MIRRORC_CDK_INVALID, MIRRORC_CDK_MISMATCH,
};
use crate::utils::i18n;
use crate::session::ProjectConfig;
use crate::utils::taskdialog::{
    prompt_text, show_ready, CommandLink, ProgressDialog, ProgressHwnd, ReadySpec, ID_ADVANCED,
    ID_CHANGE_PATH, ID_CLOSE, ID_INSTALL, ID_LAUNCH, ID_RADIO_BASE,
};

pub enum NativeOutcome {
    Exit,
    Again { reopen_source: bool },
    Web {
        args: InstallArgs,
        preset: SessionInput,
    },
}

enum ReadyAction {
    Install,
    Uninstall,
    Advanced,
}

struct ReadyState {
    path: String,
    source_uri: String,
    create_lnk: bool,
    delete_user_data: bool,
    mirrorc_cdk: Option<String>,
    is_update: bool,
    is_uninstall: bool,
}

pub async fn run(args: InstallArgs) -> anyhow::Result<NativeOutcome> {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
    }
    let temp_dir = std::env::temp_dir();
    if std::env::set_current_dir(&temp_dir).is_err() {
        rfd::MessageDialog::new()
        .set_title(&i18n::t("dialog.error_title", &[]))
        .set_description(&i18n::t("dialog.temp_dir", &[]))
            .show();
        return Ok(NativeOutcome::Exit);
    }

    let config = resolve_installer_config(args.clone(), true).await?;
    let project = match config.embedded_config.as_ref() {
        Some(value) => ProjectConfig::from_value(value)?,
        None => {
            rfd::MessageDialog::new()
                .set_title(&i18n::t("dialog.error", &[]))
                .set_description(&i18n::t(crate::utils::code::PKG_BROKEN, &[]))
                .set_level(rfd::MessageLevel::Error)
                .show();
            return Ok(NativeOutcome::Exit);
        }
    };

    let mut state = ready_state_from(&args, &config, &project).await?;
    if args.non_interactive {
        let input = state_to_input(&state);
        return match finish_action(
            if state.is_uninstall {
                ReadyAction::Uninstall
            } else {
                ReadyAction::Install
            },
            input,
            args,
            &config,
            &project,
        )
        .await?
        {
            NativeOutcome::Again { .. } => Ok(NativeOutcome::Exit),
            other => Ok(other),
        };
    }

    loop {
        let action = match show_ready_page(&project, &mut state).await? {
            None => return Ok(NativeOutcome::Exit),
            Some(ReadyAction::Advanced) => ReadyAction::Advanced,
            Some(ReadyAction::Install) => ReadyAction::Install,
            Some(ReadyAction::Uninstall) => ReadyAction::Uninstall,
        };

        if matches!(action, ReadyAction::Install | ReadyAction::Uninstall)
            && !project.need_web_view2
            && needs_js_plugin(&state.source_uri)
        {
            rfd::MessageDialog::new()
                .set_title("提示")
                .set_description("该安装源需要 WebView2，请使用「高级安装」。")
                .set_level(rfd::MessageLevel::Info)
                .show();
            continue;
        }

        if matches!(action, ReadyAction::Install)
            && state.source_uri.starts_with("mirrorc://")
            && state.mirrorc_cdk.as_deref().unwrap_or("").is_empty()
        {
            if !ensure_mirrorc_cdk(&project, &mut state, false).await {
                continue;
            }
        }

        let input = state_to_input(&state);
        match finish_action(action, input, args.clone(), &config, &project).await? {
            NativeOutcome::Again { reopen_source } => {
                if reopen_source {
                    let _ = ensure_mirrorc_cdk(&project, &mut state, true).await;
                }
                continue;
            }
            other => return Ok(other),
        }
    }
}

async fn ready_state_from(
    args: &InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
) -> anyhow::Result<ReadyState> {
    let settings = settings_from_cli(args, config, project).await?;
    let mut cdk = settings.mirrorc_cdk;
    if cdk.is_none() && settings.source_uri.starts_with("mirrorc://") {
        cdk = crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name)).ok();
    }
    Ok(ReadyState {
        path: if args.uninstall || config.is_uninstall {
            config.install_path.clone()
        } else {
            settings.install_path
        },
        source_uri: settings.source_uri,
        create_lnk: settings.create_lnk,
        delete_user_data: settings.delete_user_data,
        mirrorc_cdk: cdk,
        is_update: settings.is_update,
        is_uninstall: config.is_uninstall || args.uninstall,
    })
}

fn state_to_input(state: &ReadyState) -> SessionInput {
    SessionInput {
        install_path: state.path.clone(),
        source_uri: state.source_uri.clone(),
        create_lnk: state.create_lnk,
        delete_user_data: state.delete_user_data,
        mirrorc_cdk: state.mirrorc_cdk.clone(),
    }
}

fn visible_sources(project: &ProjectConfig, current_uri: &str) -> Vec<SourceItem> {
    match &project.source {
        SourceField::Single(_) => Vec::new(),
        SourceField::List(list) => list
            .iter()
            .filter(|s| (!s.hidden || s.uri == current_uri) && !needs_js_plugin(&s.uri))
            .cloned()
            .collect(),
    }
}

fn apply_preset_to_args(args: &mut InstallArgs, project: &ProjectConfig, input: &SessionInput) {
    args.target = Some(PathBuf::from(&input.install_path));
    args.mirrorc_cdk = input.mirrorc_cdk.clone();
    if let SourceField::List(list) = &project.source {
        if let Some(item) = list.iter().find(|s| s.uri == input.source_uri) {
            args.source = Some(item.id.clone());
        }
    }
}

async fn show_ready_page(
    project: &ProjectConfig,
    state: &mut ReadyState,
) -> anyhow::Result<Option<ReadyAction>> {
    loop {
        let sources = visible_sources(project, &state.source_uri);
        if !sources.is_empty() && !sources.iter().any(|s| s.uri == state.source_uri) {
            state.source_uri = sources[0].uri.clone();
        }
        let default_radio = sources
            .iter()
            .position(|s| s.uri == state.source_uri)
            .map(|i| ID_RADIO_BASE + i as i32)
            .unwrap_or(0);
        let radios = sources
            .iter()
            .enumerate()
            .map(|(i, s)| CommandLink {
                id: ID_RADIO_BASE + i as i32,
                text: s.name.clone(),
            })
            .collect();

        let verb = if state.is_uninstall {
            "卸载"
        } else if state.is_update {
            "更新"
        } else {
            "安装"
        };
        let dest = if state.is_uninstall {
            format!("卸载自 {}", state.path)
        } else if state.is_update {
            format!("更新到 {}", state.path)
        } else {
            format!("安装到 {}", state.path)
        };
        let mut content = project.description.clone();
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(&dest);

        let mut links = vec![CommandLink {
            id: ID_INSTALL,
            text: if state.is_uninstall {
                "卸载".to_string()
            } else {
                format!("{verb}\n使用当前选项继续")
            },
        }];
        if !state.is_uninstall {
            links.push(CommandLink {
                id: ID_CHANGE_PATH,
                text: "更改安装位置".to_string(),
            });
        }
        links.push(CommandLink {
            id: ID_ADVANCED,
            text: "高级安装\n将安装 WebView2 并打开完整界面".to_string(),
        });

        let verification = if state.is_uninstall {
            Some("同时删除用户数据".to_string())
        } else if !state.is_update {
            Some("创建桌面快捷方式".to_string())
        } else {
            None
        };
        let verification_checked = if state.is_uninstall {
            state.delete_user_data
        } else {
            state.create_lnk
        };

        let spec = ReadySpec {
            title: project.window_title.clone(),
            instruction: project.title.clone(),
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

        if !sources.is_empty() {
            if let Some(src) = sources
                .iter()
                .enumerate()
                .find(|(i, _)| ID_RADIO_BASE + *i as i32 == result.radio)
                .map(|(_, s)| s)
            {
                state.source_uri = src.uri.clone();
            }
        }
        if state.is_uninstall {
            state.delete_user_data = result.verified;
        } else if !state.is_update {
            state.create_lnk = result.verified;
        }

        match result.button {
            ID_INSTALL if state.is_uninstall => return Ok(Some(ReadyAction::Uninstall)),
            ID_INSTALL => return Ok(Some(ReadyAction::Install)),
            ID_ADVANCED => return Ok(Some(ReadyAction::Advanced)),
            ID_CHANGE_PATH => {
                if let Some(path) =
                    pick_path(&state.path, &project.exe_name, &project.app_name).await
                {
                    state.path = path;
                    if let Some(dir) =
                        inspect_dir(state.path.clone(), project.exe_name.clone()).await
                    {
                        state.is_update = dir.upgrade;
                    }
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

fn mirrorc_target(app_name: &str) -> String {
    format!("KachinaInstaller_MirrorChyanCDK_{app_name}")
}

async fn ensure_mirrorc_cdk(
    project: &ProjectConfig,
    state: &mut ReadyState,
    force: bool,
) -> bool {
    if !force {
        if state.mirrorc_cdk.as_deref().unwrap_or("").is_empty() {
            state.mirrorc_cdk =
                crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name)).ok();
        }
        if !state.mirrorc_cdk.as_deref().unwrap_or("").is_empty() {
            return true;
        }
    }

    let app = project.app_name.clone();
    let initial = state.mirrorc_cdk.clone().unwrap_or_default();
    let key = tokio::task::spawn_blocking(move || {
        prompt_text("Mirror酱", &format!("请输入 {app} 的 Mirror酱 CDK"), &initial)
    })
    .await
    .ok()
    .flatten();

    match key {
        Some(key) if key.is_empty() => {
            let _ = crate::utils::wincred::wincred_delete(&mirrorc_target(&project.app_name));
            state.mirrorc_cdk = None;
            false
        }
        Some(key) => {
            let _ = crate::utils::wincred::wincred_write(
                &mirrorc_target(&project.app_name),
                &key,
                "MirrorChyan CDK",
            );
            state.mirrorc_cdk = Some(key);
            true
        }
        None => !state.mirrorc_cdk.as_deref().unwrap_or("").is_empty(),
    }
}

async fn finish_action(
    action: ReadyAction,
    input: SessionInput,
    mut args: InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
) -> anyhow::Result<NativeOutcome> {
    let to_web = project.need_web_view2 || matches!(action, ReadyAction::Advanced);
    if to_web {
        if crate::module::wv2::install_webview2().await.is_err() {
            return Ok(NativeOutcome::Exit);
        }
        apply_preset_to_args(&mut args, project, &input);
        args.non_interactive = !matches!(action, ReadyAction::Advanced);
        args.uninstall = matches!(action, ReadyAction::Uninstall);
        return Ok(NativeOutcome::Web {
            args,
            preset: input,
        });
    }

    match action {
        ReadyAction::Advanced => unreachable!(),
        ReadyAction::Install => native_session(false, input, args, config, project).await,
        ReadyAction::Uninstall => native_session(true, input, args, config, project).await,
    }
}

async fn native_session(
    uninstall: bool,
    input: SessionInput,
    args: InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
) -> anyhow::Result<NativeOutcome> {
    let (settings, _) = settings_from_input(&input, &args, config).await?;
    let heading = if uninstall {
        i18n::t("ready.uninstalling", &[])
    } else if settings.is_update {
        i18n::t("ready.updating", &[])
    } else {
        i18n::t("ready.installing", &[])
    };
    let prepare = i18n::t("progress.prepare", &[]);
    let dialog = ProgressDialog::show(&project.window_title, &heading, &prepare, false).await?;
    let ui = NativeUi::new(dialog.hwnd_arc());
    let mgr = ManagedElevate::new();
    let result = if uninstall {
        run_uninstall(&settings, config, project, &ui, &mgr).await
    } else {
        run_install(&settings, config, project, &ui, &mgr).await
    };
    dialog.close().await;

    match result {
        Ok(result) if result.cancelled => Ok(NativeOutcome::Again { reopen_source: false }),
        Ok(result) => {
            show_finish(project, &input, &result).await;
            Ok(NativeOutcome::Exit)
        }
        Err(err) => {
            if should_report_error(&err) {
                crate::utils::sentry::capture_anyhow(&err);
            }
            let (title, message) = notice_from_error(&err);
            rfd::MessageDialog::new()
                .set_title(&title)
                .set_description(&message)
                .set_level(rfd::MessageLevel::Error)
                .show();
            Ok(NativeOutcome::Again {
                reopen_source: cdk_should_reopen(&err),
            })
        }
    }
}

async fn show_finish(project: &ProjectConfig, input: &SessionInput, result: &SessionResult) {
    if result.cancelled {
        return;
    }
    let (instruction, launch) = if result.is_uninstall {
        ("卸载成功", false)
    } else if result.already_latest {
        ("您已安装最新版本", true)
    } else if result.is_update {
        ("更新完成", true)
    } else {
        ("安装完成", true)
    };
    let mut links = Vec::new();
    if launch {
        links.push(CommandLink {
            id: ID_LAUNCH,
            text: "启动".to_string(),
        });
    }
    links.push(CommandLink {
        id: ID_CLOSE,
        text: "关闭".to_string(),
    });
    let spec = ReadySpec {
        title: project.window_title.clone(),
        instruction: instruction.to_string(),
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
        let path = PathBuf::from(&input.install_path).join(&project.exe_name);
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
            MIRRORC_CDK_EXPIRED
                | MIRRORC_CDK_INVALID
                | MIRRORC_CDK_MISMATCH
                | MIRRORC_CDK_BANNED
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
        crate::installer::confirm_dialog(title, message, self.parent()).await
    }

    fn notify(&self, coded: &Coded) {
        let (title, message) = notice_text(coded);
        let parent = self.parent();
        tokio::spawn(async move {
            crate::installer::error_dialog(title, message, parent).await;
        });
    }
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
