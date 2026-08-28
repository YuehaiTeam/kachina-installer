use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use serde_json::Value;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

use crate::cli::arg::InstallArgs;
use crate::host::HwndParent;
use crate::installer::config::{resolve_installer_config, InstallerConfig};
use crate::installer::{inspect_dir, select_dir, SelectDirRes};
use crate::ipc::manager::ManagedElevate;
use crate::session::commands::settings_from_input;
use crate::session::error;
use crate::session::run::{run_install, run_uninstall};
use crate::session::source::needs_js_plugin;
use crate::session::types::{
    settings_from_cli, SessionInput, SessionResult, SourceField, SourceItem,
};
use crate::session::ui::{send_ev_insight, PromptKind, SessionUi};
use crate::session::ProjectConfig;
use crate::utils::taskdialog::{
    prompt_text, show_ready, CommandLink, ProgressDialog, ProgressHwnd, ReadySpec, ID_ADVANCED,
    ID_CHANGE_PATH, ID_CLOSE, ID_INSTALL, ID_LAUNCH, ID_RADIO_BASE,
};

pub enum NativeOutcome {
    Exit,
    Again,
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
            .set_title("错误")
            .set_description("无法访问临时文件夹")
            .show();
        return Ok(NativeOutcome::Exit);
    }

    let config = resolve_installer_config(args.clone(), true).await?;
    let project = match config.embedded_config.as_ref() {
        Some(value) => ProjectConfig::from_value(value)?,
        None => {
            rfd::MessageDialog::new()
                .set_title("出错了")
                .set_description(error::PKG_BROKEN)
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
            NativeOutcome::Again => Ok(NativeOutcome::Exit),
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
            if !ensure_mirrorc_cdk(&project, &mut state).await {
                continue;
            }
        }

        let input = state_to_input(&state);
        match finish_action(action, input, args.clone(), &config, &project).await? {
            NativeOutcome::Again => continue,
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
            .filter(|s| !s.hidden || s.uri == current_uri)
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
    let seldir = select_dir(current.to_string(), exe_name.to_string(), false, parent).await?;
    apply_path_choice(seldir, app_name).await
}

async fn apply_path_choice(seldir: SelectDirRes, app_name: &str) -> Option<String> {
    if !seldir.empty && !seldir.upgrade {
        let is_drive_root = {
            let n = seldir.path.replace('\\', "/");
            n.len() == 3 && n.as_bytes().get(1) == Some(&b':') && n.ends_with('/')
        };
        let nest = is_drive_root
            || crate::installer::confirm_dialog(
                "提示".to_string(),
                "您选择的目录不为空，是否创建新文件夹再安装？选【否】将可能影响原有数据。"
                    .to_string(),
                HwndParent::from_hwnd(unsafe { GetDesktopWindow() }),
            )
            .await;
        if nest {
            return Some(format!(
                "{}\\{app_name}",
                seldir.path.trim_end_matches(['\\', '/'])
            ));
        }
    }
    Some(seldir.path)
}

fn mirrorc_target(app_name: &str) -> String {
    format!("KachinaInstaller_MirrorChyanCDK_{app_name}")
}

async fn ensure_mirrorc_cdk(project: &ProjectConfig, state: &mut ReadyState) -> bool {
    if state.mirrorc_cdk.as_deref().unwrap_or("").is_empty() {
        state.mirrorc_cdk =
            crate::utils::wincred::wincred_read(&mirrorc_target(&project.app_name)).ok();
    }
    if state.mirrorc_cdk.as_deref().unwrap_or("").is_empty() {
        let app = project.app_name.clone();
        let key = tokio::task::spawn_blocking(move || {
            prompt_text("Mirror酱", &format!("请输入 {app} 的 Mirror酱 CDK"), "")
        })
        .await
        .ok()
        .flatten();
        if let Some(key) = key {
            let _ = crate::utils::wincred::wincred_write(
                &mirrorc_target(&project.app_name),
                &key,
                "MirrorChyan CDK",
            );
            state.mirrorc_cdk = Some(key);
        }
    }
    !state.mirrorc_cdk.as_deref().unwrap_or("").is_empty()
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
        "正在卸载"
    } else if settings.is_update {
        "正在更新"
    } else {
        "正在安装"
    };
    let dialog = ProgressDialog::show(&project.window_title, heading, "准备中...", false).await?;
    let ui = NativeUi::new(dialog.hwnd_arc());
    let mgr = ManagedElevate::new();
    let result = if uninstall {
        run_uninstall(&settings, config, project, &ui, &mgr).await
    } else {
        run_install(&settings, config, project, &ui, &mgr).await
    };
    let reopen = ui.take_reopen();
    dialog.close().await;

    match result {
        Ok(result) if result.cancelled => Ok(NativeOutcome::Again),
        Ok(result) => {
            show_finish(project, &input, &result).await;
            Ok(NativeOutcome::Exit)
        }
        Err(err) => {
            let _ = reopen;
            // native 路径不经过 TACommandError::serialize，在此上报
            if crate::session::error::classify(&err).report {
                crate::utils::sentry::capture_anyhow(&err);
            }
            rfd::MessageDialog::new()
                .set_title("出错了")
                .set_description(format!("{err}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            Ok(NativeOutcome::Again)
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
    reopen: AtomicBool,
}

impl NativeUi {
    fn new(hwnd: Arc<ProgressHwnd>) -> Self {
        Self {
            hwnd,
            reopen: AtomicBool::new(false),
        }
    }

    fn take_reopen(&self) -> bool {
        self.reopen.swap(false, Ordering::SeqCst)
    }

    fn parent(&self) -> HwndParent {
        HwndParent::from_hwnd(
            self.hwnd
                .get()
                .unwrap_or_else(|| unsafe { GetDesktopWindow() }),
        )
    }
}

fn plain_progress(s: &str) -> String {
    s.replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n")
}

#[async_trait]
impl SessionUi for NativeUi {
    async fn confirm(&self, _kind: PromptKind, title: &str, message: &str) -> bool {
        crate::installer::confirm_dialog(title.to_string(), message.to_string(), self.parent())
            .await
    }

    fn progress(&self, event: crate::session::types::ProgressEvent) {
        // ProgressDialog methods need the dialog; we only have hwnd. Send directly.
        if let Some(hwnd) = self.hwnd.get() {
            set_progress_hwnd(hwnd, event.percent, &plain_progress(&event.current));
        }
    }

    async fn alert(&self, title: &str, message: &str) {
        crate::installer::error_dialog(title.to_string(), message.to_string(), self.parent()).await;
    }

    fn insight(&self, url: &str, event: &str, data: Option<Value>) {
        let url = url.to_string();
        let event = event.to_string();
        tokio::spawn(async move {
            send_ev_insight(&url, &event, data).await;
        });
    }

    fn reopen_source(&self) {
        self.reopen.store(true, Ordering::SeqCst);
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
