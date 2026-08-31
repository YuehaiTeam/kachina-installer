mod assets;
mod bridge;
pub mod native;
mod webview;
mod window;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde_json::Value;
use tokio::sync::oneshot;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, IsWindow, PostThreadMessageW, TranslateMessage, MSG, WM_APP,
    WM_QUIT, WM_SETTINGCHANGE,
};

use crate::cli::arg::InstallArgs;
use crate::installer::uninstall::delete_self_on_exit;
use crate::ipc::manager::ManagedElevate;
use crate::session::commands::SessionState;
use crate::session::error::{self, user};
use crate::session::types::SessionInput;
use crate::APP_BOOT_SIGNAL;

pub use window::HwndParent;

const UI_HOST: &str = "https://app.localhost";

pub enum UiAction {
    Emit { event: String, payload: Value },
    Reply { id: u64, ok: bool, data: Value },
    Close,
    Show,
    Minimize,
    SetTitle(String),
    SetDecorations(bool),
    SetBackground { dark: bool },
}

#[derive(Clone)]
pub struct HostHandle {
    tx: mpsc::Sender<UiAction>,
    thread_id: u32,
    hwnd: isize,
}

impl HostHandle {
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    pub fn parent(&self) -> HwndParent {
        HwndParent::from_hwnd(self.hwnd())
    }

    pub fn emit(&self, event: &str, payload: impl serde::Serialize) {
        let payload = serde_json::to_value(payload).unwrap_or(Value::Null);
        self.send(UiAction::Emit {
            event: event.to_string(),
            payload,
        });
    }

    pub fn close(&self) {
        self.send(UiAction::Close);
    }

    pub(crate) fn send(&self, action: UiAction) {
        let _ = self.tx.send(action);
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_APP, WPARAM(0), LPARAM(0));
        }
    }
}

pub struct HostCtx {
    pub args: InstallArgs,
    pub elevate: ManagedElevate,
    pub session: SessionState,
    pub ui: HostHandle,
    pub plugin_runtime: bool,
    pub plugin_ready: Mutex<Option<oneshot::Sender<()>>>,
    pub preset: Option<SessionInput>,
}

pub struct PluginRuntime {
    handle: HostHandle,
    join: Option<std::thread::JoinHandle<()>>,
}

impl PluginRuntime {
    pub fn handle(&self) -> &HostHandle {
        &self.handle
    }

    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.handle.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.shutdown();
        }
    }
}

pub fn webview_version() -> Result<String, anyhow::Error> {
    webview::available_version()
}

pub fn run(args: InstallArgs, preset: Option<SessionInput>) -> anyhow::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        if SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE).is_err() {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
        }
    }

    let temp_dir = std::env::temp_dir();
    if std::env::set_current_dir(&temp_dir).is_err() {
        rfd::MessageDialog::new()
            .set_title("错误")
            .set_description("无法访问临时文件夹")
            .show();
        return Ok(());
    }

    let (major, minor, build) = nt_version::get();
    let build = (build & 0xffff) as u16;
    let is_win11 = major == 10 && minor == 0 && build >= 22000;

    let text_scale = crate::windows_text_scale_factor();
    let scale = text_scale * window::dpi_scale();
    let width = (520.0 * scale).round() as i32;
    let height = (250.0 * scale).round() as i32;

    let hwnd = window::create(width, height, is_win11).context("create window")?;

    let (tx, rx) = mpsc::channel();
    let handle = HostHandle {
        tx,
        thread_id: unsafe { GetCurrentThreadId() },
        hwnd: hwnd.0 as isize,
    };

    let ctx = Arc::new(HostCtx {
        args,
        elevate: ManagedElevate::new(),
        session: SessionState::default(),
        ui: handle.clone(),
        plugin_runtime: false,
        plugin_ready: Mutex::new(None),
        preset,
    });

    tokio::spawn({
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if APP_BOOT_SIGNAL.load(std::sync::atomic::Ordering::SeqCst) {
                tracing::info!("Webview2 is alive");
                return;
            }
            rfd::MessageDialog::new()
                .set_title("Kachina Installer")
                .set_description("Initialization failed due to webview2 fault")
                .set_level(rfd::MessageLevel::Error)
                .show();
            tracing::error!("Webview2 fault detected");
            std::process::exit(1);
        }
    });

    let start = if cfg!(debug_assertions) {
        "http://localhost:1420".to_string()
    } else {
        format!("{UI_HOST}/index.html")
    };
    let webview =
        webview::attach(hwnd, handle.clone(), ctx, is_win11, &start).context("attach webview2")?;

    if !cfg!(debug_assertions) {
        window::set_visible(hwnd, false);
    } else {
        window::set_visible(hwnd, true);
        let _ = webview.open_devtools();
    }

    let mut msg = MSG::default();
    loop {
        while let Ok(action) = rx.try_recv() {
            if matches!(action, UiAction::Close) {
                delete_self_on_exit();
            }
            if !matches!(action, UiAction::Close) && !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                continue;
            }
            if let Err(err) = webview.apply(hwnd, action) {
                tracing::warn!("ui action failed: {err}");
            }
        }

        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match result.0 {
            -1 => break Err(anyhow::anyhow!("GetMessage failed")),
            0 => {
                delete_self_on_exit();
                break Ok(());
            }
            _ => {
                if msg.message == WM_QUIT {
                    delete_self_on_exit();
                    break Ok(());
                }
                if msg.message == WM_SETTINGCHANGE && window::is_color_theme_change(msg.lParam) {
                    let dark = crate::utils::gui::is_dark_mode().unwrap_or(false);
                    if let Err(err) = webview.apply(hwnd, UiAction::SetBackground { dark }) {
                        tracing::warn!("ui action failed: {err}");
                    }
                }
                if msg.message != WM_APP {
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }
}

pub async fn spawn_plugin_runtime(
    args: InstallArgs,
    session: SessionState,
) -> anyhow::Result<PluginRuntime> {
    let (started_tx, started_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let rt = tokio::runtime::Handle::current();
    let join = std::thread::Builder::new()
        .name("kachina-plugin-host".into())
        .spawn(move || {
            let _enter = rt.enter();
            plugin_runtime_thread(args, session, started_tx, ready_tx);
        })
        .context("spawn plugin host thread")?;

    let handle = started_rx
        .await
        .map_err(|_| user(error::PLUGIN_HOST_FAILED))?
        .map_err(|err| {
            tracing::error!("plugin host thread failed: {err:#}");
            user(error::PLUGIN_HOST_FAILED)
        })?;

    let runtime = PluginRuntime {
        handle,
        join: Some(join),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), ready_rx).await {
        Ok(Ok(())) => Ok(runtime),
        _ => {
            runtime.close();
            Err(user(error::PLUGIN_HOST_FAILED))
        }
    }
}

fn plugin_runtime_thread(
    args: InstallArgs,
    session: SessionState,
    started_tx: oneshot::Sender<anyhow::Result<HostHandle>>,
    ready_tx: oneshot::Sender<()>,
) {
    match plugin_runtime_setup(args, session, ready_tx) {
        Ok((handle, rx, hwnd, webview)) => {
            let _ = started_tx.send(Ok(handle.clone()));
            plugin_runtime_loop(handle, rx, hwnd, webview);
        }
        Err(err) => {
            let _ = started_tx.send(Err(err));
        }
    }
}

fn plugin_runtime_setup(
    args: InstallArgs,
    session: SessionState,
    ready_tx: oneshot::Sender<()>,
) -> anyhow::Result<(
    HostHandle,
    mpsc::Receiver<UiAction>,
    HWND,
    webview::WebViewHost,
)> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }

    let (major, minor, build) = nt_version::get();
    let build = (build & 0xffff) as u16;
    let is_win11 = major == 10 && minor == 0 && build >= 22000;

    let hwnd = window::create_hidden().context("create hidden plugin window")?;
    let (tx, rx) = mpsc::channel();
    let handle = HostHandle {
        tx,
        thread_id: unsafe { GetCurrentThreadId() },
        hwnd: hwnd.0 as isize,
    };
    let ctx = Arc::new(HostCtx {
        args,
        elevate: ManagedElevate::new(),
        session,
        ui: handle.clone(),
        plugin_runtime: true,
        plugin_ready: Mutex::new(Some(ready_tx)),
        preset: None,
    });
    let start = format!("{UI_HOST}/index.html?pluginHost=1");
    let webview = webview::attach(hwnd, handle.clone(), ctx, is_win11, &start)
        .context("attach hidden plugin webview")?;
    Ok((handle, rx, hwnd, webview))
}

fn plugin_runtime_loop(
    _handle: HostHandle,
    rx: mpsc::Receiver<UiAction>,
    hwnd: HWND,
    webview: webview::WebViewHost,
) {
    let mut msg = MSG::default();
    loop {
        while let Ok(action) = rx.try_recv() {
            if !matches!(action, UiAction::Close) && !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                continue;
            }
            if let Err(err) = webview.apply(hwnd, action) {
                tracing::warn!("plugin host ui action failed: {err}");
            }
        }

        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match result.0 {
            -1 | 0 => break,
            _ => {
                if msg.message == WM_QUIT {
                    break;
                }
                if msg.message == WM_SETTINGCHANGE && window::is_color_theme_change(msg.lParam) {
                    let dark = crate::utils::gui::is_dark_mode().unwrap_or(false);
                    if let Err(err) = webview.apply(hwnd, UiAction::SetBackground { dark }) {
                        tracing::warn!("plugin host ui action failed: {err}");
                    }
                }
                if msg.message != WM_APP {
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }
}
