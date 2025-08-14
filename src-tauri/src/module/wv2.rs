use std::ptr::null_mut;

use windows::{
    core::{HRESULT, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, S_OK, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                TASKDIALOGCONFIG, TASKDIALOG_NOTIFICATIONS, TDE_CONTENT,
                TDF_SHOW_MARQUEE_PROGRESS_BAR, TDF_USE_HICON_MAIN, TDM_SET_PROGRESS_BAR_MARQUEE,
                TDM_UPDATE_ELEMENT_TEXT, TDN_CREATED, TDN_DESTROYED,
            },
            WindowsAndMessaging::{LoadIconW, SendMessageW, WM_CLOSE},
        },
    },
};

use crate::{utils::url::HttpContextExt, REQUEST_CLIENT};

pub struct SendableHwnd(pub *mut Option<HWND>);
unsafe impl Send for SendableHwnd {}
unsafe impl Sync for SendableHwnd {}
impl SendableHwnd {
    pub fn as_isize(&self) -> isize {
        self.0 as isize
    }
}

pub async fn install_webview2() {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
    }
    let title = "安装 WebView2 运行时";
    let heading = "当前系统缺少 WebView2 运行时，正在安装...";
    let content = "正在下载安装程序...";
    let title_utf16_nul = title
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    let heading_utf16_nul = heading
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    let content_utf16_nul = content
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    let mut dialog_hwnd: Option<HWND> = None;
    let ptr_dialog_hwnd = SendableHwnd(&mut dialog_hwnd as *mut Option<HWND>);
    unsafe extern "system" fn callback(
        hwnd: HWND,
        msg: TASKDIALOG_NOTIFICATIONS,
        _w_param: WPARAM,
        _l_param: LPARAM,
        lp_ref_data: isize,
    ) -> HRESULT {
        let conf = lp_ref_data as *mut std::option::Option<windows::Win32::Foundation::HWND>;
        match msg {
            TDN_CREATED => {
                (*conf).replace(hwnd);
                SendMessageW(
                    hwnd,
                    TDM_SET_PROGRESS_BAR_MARQUEE.0 as u32,
                    Some(WPARAM(1)),
                    Some(LPARAM(1)),
                );
            }
            TDN_DESTROYED => {
                if (*conf).is_some() {
                    (*conf).take();
                    std::process::exit(1);
                }
            }
            _ => {}
        };
        S_OK
    }
    tokio::task::spawn_blocking(move || {
        // get HICON of the current process
        let hmodule = unsafe { GetModuleHandleW(PCWSTR(null_mut())).unwrap() };
        let hicon = unsafe {
            LoadIconW(
                Some(hmodule.into()),
                windows::Win32::UI::WindowsAndMessaging::IDI_APPLICATION,
            )
        };

        let config: TASKDIALOGCONFIG = TASKDIALOGCONFIG {
            cbSize: u32::try_from(std::mem::size_of::<TASKDIALOGCONFIG>()).unwrap(),
            hInstance: unsafe { GetModuleHandleW(PCWSTR(std::ptr::null())).unwrap().into() },
            pszWindowTitle: PCWSTR(title_utf16_nul.as_ptr()),
            pszMainInstruction: PCWSTR(heading_utf16_nul.as_ptr()),
            pszContent: PCWSTR(content_utf16_nul.as_ptr()),
            dwFlags: TDF_SHOW_MARQUEE_PROGRESS_BAR | TDF_USE_HICON_MAIN,
            pfCallback: Some(callback),
            lpCallbackData: ptr_dialog_hwnd.as_isize(),
            dwCommonButtons: windows::Win32::UI::Controls::TDCBF_CANCEL_BUTTON,
            Anonymous1: windows::Win32::UI::Controls::TASKDIALOGCONFIG_0 {
                hMainIcon: if let Ok(hicon) = hicon {
                    hicon
                } else {
                    windows::Win32::UI::WindowsAndMessaging::HICON(null_mut())
                },
            },
            ..TASKDIALOGCONFIG::default()
        };
        let _ =
            unsafe { windows::Win32::UI::Controls::TaskDialogIndirect(&config, None, None, None) };
    });
    // use reqwest to download the installer
    let wv2_url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let res = REQUEST_CLIENT
        .get(wv2_url)
        .send()
        .await
        .with_http_context("install_webview2", wv2_url);
    if let Err(e) = res {
        let hwnd = dialog_hwnd.take();
        unsafe {
            SendMessageW(hwnd.unwrap(), WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        rfd::MessageDialog::new()
            .set_title("出错了")
            .set_description(format!("WebView2 运行时下载失败: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(0);
    }
    let res = res.unwrap();
    let wv2_installer_blob = res.bytes().await.with_http_context("install_webview2", wv2_url);
    if let Err(e) = wv2_installer_blob {
        let hwnd = dialog_hwnd.take();
        unsafe {
            SendMessageW(hwnd.unwrap(), WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        rfd::MessageDialog::new()
            .set_title("出错了")
            .set_description(format!("WebView2 运行时下载失败: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(0);
    }
    let wv2_installer_blob = wv2_installer_blob.unwrap();
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir
        .as_path()
        .join("kachina.MicrosoftEdgeWebview2Setup.exe");
    let res = tokio::fs::write(&installer_path, wv2_installer_blob).await;
    if let Err(e) = res {
        let hwnd = dialog_hwnd.take();
        unsafe {
            SendMessageW(hwnd.unwrap(), WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        rfd::MessageDialog::new()
            .set_title("出错了")
            .set_description(format!("WebView2 运行时安装程序写入失败: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(0);
    }
    // change content of the dialog
    let content = "正在安装 WebView2 运行时...";
    let content_utf16_nul = content
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    unsafe {
        SendMessageW(
            *dialog_hwnd.as_ref().unwrap(),
            TDM_UPDATE_ELEMENT_TEXT.0 as u32,
            Some(WPARAM(TDE_CONTENT.0.try_into().unwrap())),
            Some(LPARAM(content_utf16_nul.as_ptr() as isize)),
        );
    }
    // run the installer
    let status = tokio::process::Command::new(installer_path.clone())
        .arg("/install")
        .status()
        .await;
    if let Err(e) = status {
        let hwnd = dialog_hwnd.take();
        unsafe {
            SendMessageW(hwnd.unwrap(), WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        rfd::MessageDialog::new()
            .set_title("出错了")
            .set_description(format!("WebView2 运行时安装失败: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(0);
    }
    let status = status.unwrap();
    let _ = tokio::fs::remove_file(installer_path).await;
    if status.success() {
        dialog_hwnd.take();
        // close the dialog
        let hwnd = dialog_hwnd.take();
        unsafe {
            SendMessageW(hwnd.unwrap(), WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        let _ = tokio::process::Command::new(std::env::current_exe().unwrap()).spawn();
        // delete the installer
    } else {
        let hwnd = dialog_hwnd.take();
        unsafe {
            SendMessageW(hwnd.unwrap(), WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        rfd::MessageDialog::new()
            .set_title("出错了")
            .set_description("WebView2 运行时安装失败")
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(0);
    }
}
