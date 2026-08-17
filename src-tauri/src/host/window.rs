use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;

use anyhow::Context;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, Win32WindowHandle, WindowHandle, WindowsDisplayHandle,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, UpdateWindow, HBRUSH, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
    GetSystemMetrics, GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassExW,
    SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, GWLP_USERDATA,
    GWL_STYLE, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN, SWP_FRAMECHANGED,
    SWP_NOMOVE, SWP_NOZORDER, SW_HIDE, SW_MINIMIZE, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_SETICON,
    WM_SETTINGCHANGE, WNDCLASSEXW, WS_CAPTION, WS_EX_NOREDIRECTIONBITMAP, WS_MAXIMIZE, WS_MINIMIZE,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_POPUP, WS_SYSMENU, WS_VISIBLE,
};

use crate::installer::uninstall::delete_self_on_exit;
use crate::utils::gui::is_dark_mode;

#[derive(Clone, Copy)]
pub struct HwndParent(pub isize);

impl HwndParent {
    pub fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    pub fn hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

unsafe impl Send for HwndParent {}
unsafe impl Sync for HwndParent {}

impl HasWindowHandle for HwndParent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
        let handle = Win32WindowHandle::new(hwnd);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

impl HasDisplayHandle for HwndParent {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new()))
        })
    }
}

use std::num::NonZeroIsize;

const CLASS: PCWSTR = w!("KachinaInstaller");

pub fn primary_dpi() -> u32 {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
            dpi_x.max(1)
        } else {
            96
        }
    }
}

pub fn dpi_scale() -> f64 {
    primary_dpi() as f64 / 96.0
}

pub fn create(client_w: i32, client_h: i32, is_win11: bool) -> anyhow::Result<HWND> {
    let hinstance = unsafe { GetModuleHandleW(None) }?.into();
    let (h_icon, h_icon_sm) = load_exe_icons().unwrap_or_default();
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: CLASS,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
        hbrBackground: HBRUSH::default(),
        hIcon: h_icon,
        hIconSm: h_icon_sm,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&class) };

    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let ex_style = WS_EX_NOREDIRECTIONBITMAP;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    unsafe { AdjustWindowRectEx(&mut rect, style, false, ex_style)? };
    let win_w = rect.right - rect.left;
    let win_h = rect.bottom - rect.top;
    let x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - win_w) / 2;
    let y = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - win_h) / 2;

    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS,
            w!(" "),
            style,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .context("CreateWindowExW")?;

    if is_win11 {
        apply_mica(hwnd);
    }
    apply_icon(hwnd);
    unsafe {
        let _ = UpdateWindow(hwnd);
    }
    Ok(hwnd)
}

pub fn apply_mica(hwnd: HWND) {
    let backdrop = DWM_SYSTEMBACKDROP_TYPE(2); // DWMSBT_MAINWINDOW (Mica)
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const _,
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
    };
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    let _ = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
    let dark = is_dark_mode().unwrap_or(false) as i32;
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            size_of::<i32>() as u32,
        )
    };
}

pub fn apply_icon(hwnd: HWND) {
    let Some((large, small)) = load_exe_icons() else {
        return;
    };
    unsafe {
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(large.0 as isize)),
        );
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0 as isize)),
        );
    }
}

fn load_exe_icons() -> Option<(HICON, HICON)> {
    let exe = std::env::current_exe().ok()?;
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
        return None;
    }
    normalize_icons(large, small)
}

fn normalize_icons(large: HICON, small: HICON) -> Option<(HICON, HICON)> {
    match (large.is_invalid(), small.is_invalid()) {
        (true, true) => None,
        (false, false) => Some((large, small)),
        (true, false) => Some((small, small)),
        (false, true) => Some((large, large)),
    }
}

pub fn set_visible(hwnd: HWND, visible: bool) {
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
    }
}

pub fn set_title(hwnd: HWND, title: &str) {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
    }
}

pub fn minimize(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
}

pub fn set_decorations(hwnd: HWND, decorated: bool) {
    let current = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    if decorated == (current & WS_CAPTION.0 != 0) {
        return;
    }
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let keep = current & (WS_VISIBLE.0 | WS_MINIMIZE.0 | WS_MAXIMIZE.0);
    let new_style = if decorated {
        (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX).0
    } else {
        (WS_POPUP | WS_SYSMENU).0
    } | keep;
    let win_style = windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(new_style);
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style as isize);
        let mut outer = RECT {
            left: 0,
            top: 0,
            right: rect.right - rect.left,
            bottom: rect.bottom - rect.top,
        };
        let _ = AdjustWindowRectEx(&mut outer, win_style, false, WS_EX_NOREDIRECTIONBITMAP);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            outer.right - outer.left,
            outer.bottom - outer.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

pub fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            delete_self_on_exit();
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_SETTINGCHANGE => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[allow(dead_code)]
pub fn set_user_data(hwnd: HWND, ptr: isize) {
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr);
    }
}

#[allow(dead_code)]
pub fn user_data(hwnd: HWND) -> isize {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) }
}
