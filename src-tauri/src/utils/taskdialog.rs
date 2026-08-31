use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use tokio::sync::oneshot;
use windows::core::{w, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, S_OK, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_BUTTON,
    TASKDIALOG_COMMON_BUTTON_FLAGS, TASKDIALOG_NOTIFICATIONS, TDE_CONTENT,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_NO_DEFAULT_RADIO_BUTTON, TDF_SHOW_MARQUEE_PROGRESS_BAR,
    TDF_SHOW_PROGRESS_BAR, TDF_SIZE_TO_CONTENT, TDF_USE_COMMAND_LINKS, TDF_USE_HICON_MAIN,
    TDF_VERIFICATION_FLAG_CHECKED, TDM_SET_PROGRESS_BAR_MARQUEE, TDM_SET_PROGRESS_BAR_POS,
    TDM_UPDATE_ELEMENT_TEXT, TDN_CREATED, TDN_DESTROYED,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, TranslateMessage, BS_DEFPUSHBUTTON, CW_USEDEFAULT, ES_AUTOHSCROLL,
    GWLP_USERDATA, HICON, IDCANCEL, IDC_ARROW, IDOK, SW_SHOW, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WNDCLASSEXW, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

pub const ID_INSTALL: i32 = 101;
pub const ID_CHANGE_PATH: i32 = 102;
pub const ID_ADVANCED: i32 = 103;
pub const ID_LAUNCH: i32 = 104;
pub const ID_CLOSE: i32 = 105;
pub const ID_RADIO_BASE: i32 = 200;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn load_main_icon() -> HICON {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return HICON::default(),
    };
    let path: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut large = HICON::default();
    let mut small = HICON::default();
    let n = unsafe {
        ExtractIconExW(
            PCWSTR(path.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        )
    };
    if n == 0 {
        return HICON::default();
    }
    if !large.is_invalid() {
        if !small.is_invalid() {
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyIcon(small) };
        }
        large
    } else {
        small
    }
}

use std::os::windows::ffi::OsStrExt;

fn hinstance() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { GetModuleHandleW(None) }
        .map(|m| m.into())
        .unwrap_or_default()
}

fn config_size() -> u32 {
    u32::try_from(std::mem::size_of::<TASKDIALOGCONFIG>()).unwrap_or(0)
}

pub struct CommandLink {
    pub id: i32,
    pub text: String,
}

pub struct ReadySpec {
    pub title: String,
    pub instruction: String,
    pub content: String,
    pub links: Vec<CommandLink>,
    pub radios: Vec<CommandLink>,
    pub default_radio: i32,
    pub verification: Option<String>,
    pub verification_checked: bool,
}

pub struct ReadyResult {
    pub button: i32,
    pub radio: i32,
    pub verified: bool,
}

pub fn show_ready(spec: ReadySpec) -> anyhow::Result<ReadyResult> {
    let title = wide(&spec.title);
    let instruction = wide(&spec.instruction);
    let content = wide(&spec.content);
    let verification = spec.verification.as_deref().map(wide);
    let link_wides: Vec<Vec<u16>> = spec.links.iter().map(|b| wide(&b.text)).collect();
    let radio_wides: Vec<Vec<u16>> = spec.radios.iter().map(|b| wide(&b.text)).collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = spec
        .links
        .iter()
        .zip(link_wides.iter())
        .map(|(b, w)| TASKDIALOG_BUTTON {
            nButtonID: b.id,
            pszButtonText: PCWSTR(w.as_ptr()),
        })
        .collect();
    let radios: Vec<TASKDIALOG_BUTTON> = spec
        .radios
        .iter()
        .zip(radio_wides.iter())
        .map(|(b, w)| TASKDIALOG_BUTTON {
            nButtonID: b.id,
            pszButtonText: PCWSTR(w.as_ptr()),
        })
        .collect();

    let icon = load_main_icon();
    let mut flags = TDF_USE_HICON_MAIN
        | TDF_USE_COMMAND_LINKS
        | TDF_ALLOW_DIALOG_CANCELLATION
        | TDF_SIZE_TO_CONTENT;
    if spec.verification_checked {
        flags |= TDF_VERIFICATION_FLAG_CHECKED;
    }
    if spec.default_radio == 0 {
        flags |= TDF_NO_DEFAULT_RADIO_BUTTON;
    }

    let config = TASKDIALOGCONFIG {
        cbSize: config_size(),
        hInstance: hinstance(),
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        dwFlags: flags,
        dwCommonButtons: TASKDIALOG_COMMON_BUTTON_FLAGS(0),
        cButtons: buttons.len() as u32,
        pButtons: if buttons.is_empty() {
            std::ptr::null()
        } else {
            buttons.as_ptr()
        },
        cRadioButtons: radios.len() as u32,
        pRadioButtons: if radios.is_empty() {
            std::ptr::null()
        } else {
            radios.as_ptr()
        },
        nDefaultRadioButton: spec.default_radio,
        pszVerificationText: verification
            .as_ref()
            .map(|v| PCWSTR(v.as_ptr()))
            .unwrap_or_else(PCWSTR::null),
        pfCallback: None,
        Anonymous1: TASKDIALOGCONFIG_0 { hMainIcon: icon },
        ..TASKDIALOGCONFIG::default()
    };

    let mut button = 0i32;
    let mut radio = 0i32;
    let mut verified = BOOL(0);
    unsafe {
        TaskDialogIndirect(
            &config,
            Some(&mut button),
            Some(&mut radio),
            Some(&mut verified),
        )
        .context("TaskDialogIndirect")?;
    }
    Ok(ReadyResult {
        button,
        radio,
        verified: verified.as_bool(),
    })
}

struct ProgressShared {
    hwnd: Mutex<Option<isize>>,
    closing: AtomicBool,
    created: Mutex<Option<oneshot::Sender<()>>>,
    marquee: bool,
}

pub struct ProgressDialog {
    shared: Arc<ProgressShared>,
    join: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl ProgressDialog {
    pub async fn show(
        title: &str,
        heading: &str,
        content: &str,
        marquee: bool,
    ) -> anyhow::Result<Self> {
        let title = title.to_string();
        let heading = heading.to_string();
        let content = content.to_string();
        let shared = Arc::new(ProgressShared {
            hwnd: Mutex::new(None),
            closing: AtomicBool::new(false),
            created: Mutex::new(None),
            marquee,
        });
        let (tx, rx) = oneshot::channel();
        *shared.created.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

        let shared_thread = shared.clone();
        let mut join = tokio::task::spawn_blocking(move || {
            progress_thread(shared_thread, title, heading, content, marquee)
        });

        tokio::select! {
            created = rx => {
                created.context("progress dialog ended before it opened")?;
                Ok(Self {
                    shared,
                    join: Some(join),
                })
            }
            join_res = &mut join => match join_res {
                Ok(Ok(())) => {
                    anyhow::bail!("progress dialog closed before it opened")
                }
                Ok(Err(err)) => Err(err),
                Err(err) => Err(err).context("progress dialog thread"),
            },
        }
    }

    pub fn hwnd(&self) -> Option<HWND> {
        self.shared
            .hwnd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|h| HWND(h as *mut _))
    }

    pub fn hwnd_arc(&self) -> Arc<ProgressHwnd> {
        Arc::new(ProgressHwnd {
            inner: self.shared.clone(),
        })
    }

    pub fn set_content(&self, text: &str) {
        let Some(hwnd) = self.hwnd() else {
            return;
        };
        let wide = wide(text);
        unsafe {
            SendMessageW(
                hwnd,
                TDM_UPDATE_ELEMENT_TEXT.0 as u32,
                Some(WPARAM(TDE_CONTENT.0 as usize)),
                Some(LPARAM(wide.as_ptr() as isize)),
            );
        }
    }

    pub fn set_progress(&self, percent: u32) {
        let Some(hwnd) = self.hwnd() else {
            return;
        };
        let pos = percent.min(100) as usize;
        unsafe {
            SendMessageW(
                hwnd,
                TDM_SET_PROGRESS_BAR_POS.0 as u32,
                Some(WPARAM(pos)),
                Some(LPARAM(0)),
            );
        }
    }

    pub async fn close(mut self) {
        self.request_close();
        if let Some(join) = self.join.take() {
            let _ = join.await;
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
            unsafe {
                SendMessageW(
                    HWND(hwnd as *mut _),
                    WM_CLOSE,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                );
            }
        }
    }
}

impl Drop for ProgressDialog {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.request_close();
            let _ = self.join.take();
        }
    }
}

pub struct ProgressHwnd {
    inner: Arc<ProgressShared>,
}

impl ProgressHwnd {
    pub fn get(&self) -> Option<HWND> {
        self.inner
            .hwnd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|h| HWND(h as *mut _))
    }
}

fn progress_thread(
    shared: Arc<ProgressShared>,
    title: String,
    heading: String,
    content: String,
    marquee: bool,
) -> anyhow::Result<()> {
    let title = wide(&title);
    let heading = wide(&heading);
    let content = wide(&content);
    let icon = load_main_icon();
    let mut flags = TDF_USE_HICON_MAIN | TDF_ALLOW_DIALOG_CANCELLATION | TDF_SIZE_TO_CONTENT;
    flags |= if marquee {
        TDF_SHOW_MARQUEE_PROGRESS_BAR
    } else {
        TDF_SHOW_PROGRESS_BAR
    };

    let ptr = Arc::as_ptr(&shared) as isize;
    let config = TASKDIALOGCONFIG {
        cbSize: config_size(),
        hInstance: hinstance(),
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        dwFlags: flags,
        dwCommonButtons: windows::Win32::UI::Controls::TDCBF_CANCEL_BUTTON,
        pfCallback: Some(progress_callback),
        lpCallbackData: ptr,
        Anonymous1: TASKDIALOGCONFIG_0 { hMainIcon: icon },
        ..TASKDIALOGCONFIG::default()
    };
    unsafe { TaskDialogIndirect(&config, None, None, None) }.context("TaskDialogIndirect")?;
    Ok(())
}

unsafe extern "system" fn progress_callback(
    hwnd: HWND,
    msg: TASKDIALOG_NOTIFICATIONS,
    _w_param: WPARAM,
    _l_param: LPARAM,
    lp_ref_data: isize,
) -> HRESULT {
    let shared = unsafe { &*(lp_ref_data as *const ProgressShared) };
    match msg {
        TDN_CREATED => {
            *shared.hwnd.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
            if shared.marquee {
                unsafe {
                    SendMessageW(
                        hwnd,
                        TDM_SET_PROGRESS_BAR_MARQUEE.0 as u32,
                        Some(WPARAM(1)),
                        Some(LPARAM(1)),
                    );
                }
            }
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
                // Match WebUI window close: dismiss is process teardown, not session cancel.
                std::process::exit(1);
            }
        }
        _ => {}
    }
    S_OK
}

const PROMPT_CLASS: PCWSTR = w!("KachinaTextPrompt");

struct PromptState {
    initial: Vec<u16>,
    prompt: Vec<u16>,
    edit: HWND,
    out: *mut Option<String>,
}

pub fn prompt_text(title: &str, prompt: &str, initial: &str) -> Option<String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).ok()?.into();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(prompt_wndproc),
            hInstance: hinstance,
            lpszClassName: PROMPT_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
            ..Default::default()
        };
        let _ = RegisterClassExW(&class);

        let mut state = PromptState {
            initial: wide(initial),
            prompt: wide(prompt),
            edit: HWND::default(),
            out: std::ptr::null_mut(),
        };
        let mut result: Option<String> = None;
        state.out = &mut result;

        let title_w = wide(title);
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
            PROMPT_CLASS,
            PCWSTR(title_w.as_ptr()),
            WS_OVERLAPPED | WS_SYSMENU | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            440,
            180,
            None,
            None,
            Some(hinstance),
            Some(&mut state as *mut PromptState as *mut _),
        )
        .ok()?;
        let _ = SetForegroundWindow(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        result
    }
}

unsafe extern "system" fn prompt_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = unsafe { &mut *((*cs).lpCreateParams as *mut PromptState) };
            let hinstance = unsafe { (*cs).hInstance };
            let label = unsafe {
                CreateWindowExW(
                    windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    PCWSTR(state.prompt.as_ptr()),
                    WS_CHILD | WS_VISIBLE,
                    16,
                    16,
                    390,
                    24,
                    Some(hwnd),
                    None,
                    Some(hinstance),
                    None,
                )
            };
            let _ = label;
            let edit = unsafe {
                CreateWindowExW(
                    windows::Win32::UI::WindowsAndMessaging::WS_EX_CLIENTEDGE,
                    w!("EDIT"),
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                    16,
                    48,
                    390,
                    24,
                    Some(hwnd),
                    None,
                    Some(hinstance),
                    None,
                )
            };
            if let Ok(edit) = edit {
                state.edit = edit;
                unsafe {
                    let _ = SetWindowTextW(edit, PCWSTR(state.initial.as_ptr()));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as *mut PromptState as isize);
                }
            }
            let _ = unsafe {
                CreateWindowExW(
                    windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    w!("确定"),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
                    230,
                    90,
                    80,
                    28,
                    Some(hwnd),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        IDOK.0 as *mut _,
                    )),
                    Some(hinstance),
                    None,
                )
            };
            let _ = unsafe {
                CreateWindowExW(
                    windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    w!("取消"),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                    326,
                    90,
                    80,
                    28,
                    Some(hwnd),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        IDCANCEL.0 as *mut _,
                    )),
                    Some(hinstance),
                    None,
                )
            };
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            if id == IDOK.0 {
                let state_ptr = prompt_state(hwnd);
                if let Some(state) = state_ptr {
                    let edit = unsafe { (*state).edit };
                    let len = unsafe { GetWindowTextLengthW(edit) } as usize;
                    let mut buf = vec![0u16; len + 1];
                    unsafe {
                        GetWindowTextW(edit, &mut buf);
                    }
                    let text = String::from_utf16_lossy(&buf)
                        .trim_end_matches('\0')
                        .trim()
                        .to_string();
                    unsafe {
                        *(*state).out = Some(text);
                    }
                }
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            } else if id == IDCANCEL.0 {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn prompt_state(hwnd: HWND) -> Option<*mut PromptState> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut PromptState;
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}
