pub(crate) mod assets;
mod bridge;
pub mod native;
mod webview;
mod window;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use tokio::sync::oneshot;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, IsWindow, PostThreadMessageW, TranslateMessage, MSG, WM_APP,
    WM_QUIT,
};

use crate::cli::arg::InstallArgs;
use crate::installer::uninstall::delete_self_on_exit;
use crate::session::commands::{GuiRuntime, SessionState};
use crate::session::types::SessionInput;
use crate::utils::code::{Attach, Coded, PLUGIN_HOST_FAILED, TEMP_DIR_UNAVAILABLE, WEBVIEW2_FAULT};
use crate::utils::taskdialog::{show_error, ErrorDialog};

pub use window::HwndParent;

const UI_HOST: &str = "https://app.localhost";

pub enum UiAction {
    Emit {
        event: String,
        payload: Value,
    },
    Reply {
        id: u64,
        ok: bool,
        data: Value,
    },
    Close,
    Show,
    Minimize,
    SetTitle(String),
    SetDecorations(bool),
    SetBackground {
        dark: bool,
    },
    /// The page went blank: a WebView2 process died or a load never reported
    /// ready. The hidden plugin host is not rebuilt; a plugin call it was
    /// serving times out.
    Recover(WebViewLost),
}

#[derive(Debug, Clone, Copy)]
pub enum WebViewLost {
    Browser,
    Renderer,
    /// The page did not report `frontend_ready` within [`PAGE_READY_TIMEOUT`].
    Stalled,
}

/// An injected module that crashes the browser process crashes each new one
/// too, so rebuilding stops after this many attempts.
const MAX_WEBVIEW_RECOVERIES: u32 = 2;

const PAGE_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Page loads of the visible window. Each load (WebView creation plus
/// navigation) is a new generation with its own deadline; a deadline whose
/// generation was superseded or reported ready does nothing.
#[derive(Default)]
pub struct LoadWatch {
    current: AtomicU64,
    ready: AtomicU64,
    /// The UI thread is inside [`webview::attach`]. A hung WebView2 creation
    /// blocks it there, so a deadline cannot be recovered on that thread.
    attaching: AtomicBool,
}

impl LoadWatch {
    pub fn mark_ready(&self) {
        self.ready
            .store(self.current.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// Supersede the pending deadline without starting a new load.
    fn abandon(&self) {
        self.current.fetch_add(1, Ordering::SeqCst);
    }
}

/// Start the deadline for the load about to begin. Called before `attach` or
/// `Reload` pumps messages, so the page cannot report ready ahead of it.
fn watch_load(ctx: &Arc<HostCtx>, handle: &HostHandle) {
    let generation = ctx.load.current.fetch_add(1, Ordering::SeqCst) + 1;
    let ctx = ctx.clone();
    let handle = handle.clone();
    tokio::spawn(async move {
        tokio::time::sleep(PAGE_READY_TIMEOUT).await;
        if ctx.load.current.load(Ordering::SeqCst) != generation
            || ctx.load.ready.load(Ordering::SeqCst) == generation
        {
            return;
        }
        if !ctx.load.attaching.load(Ordering::SeqCst) {
            tracing::error!("page load {generation} not ready within {PAGE_READY_TIMEOUT:?}");
            handle.send(UiAction::Recover(WebViewLost::Stalled));
            return;
        }
        tracing::error!("webview2 creation {generation} hung for {PAGE_READY_TIMEOUT:?}");
        show_error(ErrorDialog::code(WEBVIEW2_FAULT), HWND::default());
        while session_running(&ctx) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        delete_self_on_exit();
        std::process::exit(1);
    });
}

fn attach_watched(
    handle: &HostHandle,
    ctx: &Arc<HostCtx>,
    is_win11: bool,
    start: &str,
) -> anyhow::Result<webview::WebViewHost> {
    watch_load(ctx, handle);
    ctx.load.attaching.store(true, Ordering::SeqCst);
    let result = webview::attach(handle.hwnd(), handle.clone(), ctx.clone(), is_win11, start);
    ctx.load.attaching.store(false, Ordering::SeqCst);
    result
}

fn session_running(ctx: &HostCtx) -> bool {
    ctx.gui
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|gui| gui.running.load(Ordering::SeqCst))
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

    /// 不连接窗口；发出的动作被丢弃。
    #[cfg(test)]
    pub fn detached() -> Self {
        let (tx, _) = mpsc::channel();
        Self {
            tx,
            thread_id: 0,
            hwnd: 0,
        }
    }
}

pub struct HostCtx {
    pub args: InstallArgs,
    pub session: SessionState,
    pub ui: HostHandle,
    pub plugin_runtime: bool,
    pub plugin_ready: Mutex<Option<oneshot::Sender<()>>>,
    pub preset: Option<SessionInput>,
    pub gui: Mutex<Option<std::sync::Arc<GuiRuntime>>>,
    pub load: LoadWatch,
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

pub fn run(
    args: InstallArgs,
    preset: Option<SessionInput>,
    gui: Option<std::sync::Arc<GuiRuntime>>,
) -> anyhow::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    window::enable_dpi_awareness();

    if crate::fs::staging::enter_neutral_cwd().is_err() {
        show_error(ErrorDialog::code(TEMP_DIR_UNAVAILABLE), HWND::default());
        return Ok(());
    }

    let is_win11 = window::is_win11();

    let text_scale = crate::windows_text_scale_factor();
    let scale = text_scale * window::dpi_scale();
    let width = (520.0 * scale).round() as i32;
    let height = (250.0 * scale).round() as i32;

    let hwnd = window::create(width, height).context("create window")?;

    let (tx, rx) = mpsc::channel();
    let handle = HostHandle {
        tx,
        thread_id: unsafe { GetCurrentThreadId() },
        hwnd: hwnd.0 as isize,
    };

    let non_interactive = args.non_interactive;
    let ctx = Arc::new(HostCtx {
        args,
        session: SessionState::default(),
        ui: handle.clone(),
        plugin_runtime: false,
        plugin_ready: Mutex::new(None),
        preset,
        gui: Mutex::new(gui),
        load: LoadWatch::default(),
    });

    let start = if cfg!(debug_assertions) {
        "http://localhost:1420".to_string()
    } else {
        format!("{UI_HOST}/index.html")
    };
    let mut webview = attach_watched(&handle, &ctx, is_win11, &start).context("attach webview2")?;
    let mut recoveries = 0;
    if let Some(gui) = ctx.gui.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        let st = gui.snapshot();
        handle.send(UiAction::SetTitle(st.project.window_title.clone()));
        handle.send(UiAction::SetDecorations(!st.project.borderless));
        gui.emit(&handle);
        if non_interactive {
            let ctx2 = ctx.clone();
            let handle2 = handle.clone();
            tokio::spawn(async move {
                let _ = crate::session::commands::handle_intent(
                    crate::session::state::Intent::Start,
                    &ctx2,
                    &handle2,
                )
                .await;
            });
        }
    }

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
            if let UiAction::Recover(lost) = action {
                recoveries += 1;
                recover(
                    &mut webview,
                    lost,
                    recoveries,
                    &handle,
                    &ctx,
                    is_win11,
                    &start,
                );
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
                if msg.message == window::WM_THEME_BACKGROUND {
                    let dark = crate::utils::gui::is_dark_mode().unwrap_or(false);
                    if let Err(err) = webview.apply(hwnd, UiAction::SetBackground { dark }) {
                        tracing::warn!("ui action failed: {err}");
                    }
                }
                if msg.message == window::WM_RESIZE_WEBVIEW {
                    if let Err(err) = webview.resize(hwnd) {
                        tracing::warn!("resize webview failed: {err}");
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

/// Bring the page back after a WebView2 process exit or a stalled load. The
/// page pulls the session state from the host when it loads, so it returns to
/// the current screen. Every attempt is a new load with its own deadline.
/// Past the cap the window is closed unless a session is running.
fn recover(
    webview: &mut webview::WebViewHost,
    lost: WebViewLost,
    attempt: u32,
    handle: &HostHandle,
    ctx: &Arc<HostCtx>,
    is_win11: bool,
    start: &str,
) {
    let result = if attempt > MAX_WEBVIEW_RECOVERIES {
        Err(anyhow::anyhow!(
            "gave up after {MAX_WEBVIEW_RECOVERIES} recoveries"
        ))
    } else {
        tracing::warn!("recovering webview2 ({lost:?}), attempt {attempt}");
        match lost {
            WebViewLost::Renderer => {
                watch_load(ctx, handle);
                webview.reload()
            }
            WebViewLost::Browser | WebViewLost::Stalled => {
                webview.close();
                attach_watched(handle, ctx, is_win11, start).map(|fresh| *webview = fresh)
            }
        }
    };
    let Err(err) = result else {
        return;
    };
    ctx.load.abandon();
    tracing::error!("webview2 recovery failed: {err:#}");
    show_error(ErrorDialog::code(WEBVIEW2_FAULT), handle.hwnd());
    if !session_running(ctx) {
        handle.close();
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
        .map_err(|_| anyhow::Error::from(Coded::bare(PLUGIN_HOST_FAILED)))?
        .map_err(|err| {
            tracing::error!("plugin host thread failed: {err:#}");
            err.attach(PLUGIN_HOST_FAILED)
        })?;

    let runtime = PluginRuntime {
        handle,
        join: Some(join),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), ready_rx).await {
        Ok(Ok(())) => Ok(runtime),
        _ => {
            runtime.close();
            Err(anyhow::Error::from(Coded::bare(PLUGIN_HOST_FAILED)))
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

    let is_win11 = window::is_win11();

    let hwnd = window::create_hidden().context("create hidden plugin window")?;
    let (tx, rx) = mpsc::channel();
    let handle = HostHandle {
        tx,
        thread_id: unsafe { GetCurrentThreadId() },
        hwnd: hwnd.0 as isize,
    };
    let ctx = Arc::new(HostCtx {
        args,
        session,
        ui: handle.clone(),
        plugin_runtime: true,
        plugin_ready: Mutex::new(Some(ready_tx)),
        preset: None,
        gui: Mutex::new(None),
        load: LoadWatch::default(),
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
                if msg.message == window::WM_THEME_BACKGROUND {
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
