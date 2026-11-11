//! 最小 TaskDialog 封装：错误框与解压进度条。
//! 安装器的 utils/taskdialog.rs 依赖 i18n 与错误码（经 utils/code.rs 连到 fs/dfs），
//! sfx 只需要这两种对话框，单独维护。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_NOTIFICATIONS,
    TDCBF_CANCEL_BUTTON, TDCBF_OK_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_SHOW_PROGRESS_BAR,
    TDF_SIZE_TO_CONTENT, TDM_CLICK_BUTTON, TDM_SET_PROGRESS_BAR_POS, TDN_CREATED, TDN_DESTROYED,
    TD_ERROR_ICON,
};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, HICON, IDCANCEL};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hinstance() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { GetModuleHandleW(None) }.unwrap_or_default().into()
}

/// 同步错误框。
pub fn show_error(title: &str, content: &str) {
    let title = wide(title);
    let content = wide(content);
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hInstance: hinstance(),
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        dwCommonButtons: TDCBF_OK_BUTTON,
        dwFlags: TDF_SIZE_TO_CONTENT,
        Anonymous1: TASKDIALOGCONFIG_0 {
            // 未设 TDF_USE_HICON_MAIN 时该字段按 pszMainIcon 解释
            hMainIcon: HICON(TD_ERROR_ICON.0 as *mut _),
        },
        ..TASKDIALOGCONFIG::default()
    };
    unsafe {
        let _ = TaskDialogIndirect(&config, None, None, None);
    }
}

struct Shared {
    hwnd: Mutex<Option<isize>>,
    closing: AtomicBool,
    /// 用户点了取消或关掉了对话框
    cancelled: AtomicBool,
    created: Mutex<Option<mpsc::SyncSender<()>>>,
    total: u64,
    done: AtomicU64,
    /// 最近一次发给进度条的整数百分比
    shown_pct: AtomicU64,
}

/// 解压进度对话框：独立线程跑 TaskDialogIndirect，主线程按字节数推进。
pub struct Progress {
    shared: Arc<Shared>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    pub fn show(title: &str, heading: &str, total_bytes: u64) -> Result<Self> {
        let shared = Arc::new(Shared {
            hwnd: Mutex::new(None),
            closing: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            created: Mutex::new(None),
            total: total_bytes,
            done: AtomicU64::new(0),
            shown_pct: AtomicU64::new(0),
        });
        let (tx, rx) = mpsc::sync_channel(1);
        *shared.created.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

        let shared_thread = shared.clone();
        let title = title.to_string();
        let heading = heading.to_string();
        let join = std::thread::spawn(move || progress_thread(shared_thread, title, heading));

        // 等对话框创建完成；线程提前退出（例如无 UI 会话）则降级为无进度条。在 tokio 运行时里
        // 调用，用 std 通道阻塞：tokio 的 blocking_recv 在运行时线程上会 panic。
        match rx.recv() {
            Ok(()) => Ok(Self {
                shared,
                join: Some(join),
            }),
            Err(_) => {
                let _ = join.join();
                anyhow::bail!("progress dialog unavailable")
            }
        }
    }

    /// 累加已解压字节；只在整数百分比变化时向对话框线程发消息，整个解压最多 100 次。
    pub fn add_bytes(&self, n: u64) {
        let done = self.shared.done.fetch_add(n, Ordering::Relaxed) + n;
        let pct = done
            .saturating_mul(100)
            .checked_div(self.shared.total)
            .unwrap_or(100)
            .min(100);
        if self.shared.shown_pct.swap(pct, Ordering::Relaxed) == pct {
            return;
        }
        let Some(hwnd) = self.hwnd() else { return };
        unsafe {
            SendMessageW(
                hwnd,
                TDM_SET_PROGRESS_BAR_POS.0 as u32,
                Some(WPARAM(pct as usize)),
                Some(LPARAM(0)),
            );
        }
    }

    pub fn cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::SeqCst)
    }

    fn hwnd(&self) -> Option<HWND> {
        self.shared
            .hwnd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|h| HWND(h as *mut _))
    }

    pub async fn close(mut self) {
        self.request_close();
        if let Some(join) = self.join.take() {
            let _ = tokio::task::spawn_blocking(move || join.join()).await;
        }
    }

    fn request_close(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        let hwnd = self
            .shared
            .hwnd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(hwnd) = hwnd {
            // 点按必有的 Cancel 按钮关闭；callback 在 closing 已置位时放行
            unsafe {
                SendMessageW(
                    HWND(hwnd as *mut _),
                    TDM_CLICK_BUTTON.0 as u32,
                    Some(WPARAM(IDCANCEL.0 as usize)),
                    Some(LPARAM(0)),
                );
            }
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.request_close();
        }
    }
}

fn progress_thread(shared: Arc<Shared>, title: String, heading: String) {
    let title = wide(&title);
    let heading = wide(&heading);
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hInstance: hinstance(),
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        dwFlags: TDF_SHOW_PROGRESS_BAR | TDF_ALLOW_DIALOG_CANCELLATION | TDF_SIZE_TO_CONTENT,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pfCallback: Some(progress_callback),
        lpCallbackData: Arc::as_ptr(&shared) as isize,
        ..TASKDIALOGCONFIG::default()
    };
    unsafe {
        let _ = TaskDialogIndirect(&config, None, None, None);
    }
    // 对话框未能创建时也要放行 show() 的等待者
    if let Some(tx) = shared
        .created
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = tx.send(());
    }
}

unsafe extern "system" fn progress_callback(
    hwnd: HWND,
    msg: TASKDIALOG_NOTIFICATIONS,
    _w_param: WPARAM,
    _l_param: LPARAM,
    lp_ref_data: isize,
) -> windows::core::HRESULT {
    let shared = unsafe { &*(lp_ref_data as *const Shared) };
    match msg {
        TDN_CREATED => {
            // 进度条默认范围 0..100，与 add_bytes 发的百分比一致
            *shared.hwnd.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
            if let Some(tx) = shared
                .created
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
        }
        TDN_DESTROYED => {
            *shared.hwnd.lock().unwrap_or_else(|e| e.into_inner()) = None;
            if !shared.closing.load(Ordering::SeqCst) {
                // 用户点 Cancel/关闭 = 放弃本次运行；解压循环看到后中止，由调用方清理临时目录
                shared.cancelled.store(true, Ordering::SeqCst);
            }
        }
        _ => {}
    }
    S_OK
}
