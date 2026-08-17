mod assets;
mod bridge;
mod webview;
mod window;

use std::sync::mpsc;
use std::sync::Arc;

use anyhow::Context;
use serde_json::Value;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, IsWindow, PostThreadMessageW, TranslateMessage, MSG, WM_APP,
    WM_QUIT,
};

use crate::cli::arg::InstallArgs;
use crate::installer::uninstall::delete_self_on_exit;
use crate::ipc::manager::ManagedElevate;
use crate::session::commands::SessionState;
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
}

pub fn webview_version() -> Result<String, anyhow::Error> {
    webview::available_version()
}

pub fn run(args: InstallArgs) -> anyhow::Result<()> {
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
    });

    tokio::spawn({
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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

    let webview =
        webview::attach(hwnd, handle.clone(), ctx, is_win11).context("attach webview2")?;

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
