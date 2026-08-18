use std::sync::mpsc;
use std::sync::Arc;

use anyhow::Context;
use serde_json::Value;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use super::assets;
use super::bridge;
use super::window;
use super::{HostCtx, HostHandle, UiAction, UI_HOST};
use crate::utils::gui::is_dark_mode;

pub struct WebViewHost {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

impl WebViewHost {
    pub fn open_devtools(&self) -> anyhow::Result<()> {
        unsafe { self.webview.OpenDevToolsWindow() }.context("OpenDevToolsWindow")?;
        Ok(())
    }

    pub fn apply(&self, hwnd: HWND, action: UiAction) -> anyhow::Result<()> {
        match action {
            UiAction::Emit { event, payload } => {
                post_json(
                    &self.webview,
                    &serde_json::json!({
                        "kind": "event",
                        "event": event,
                        "payload": payload,
                    }),
                )?;
            }
            UiAction::Reply { id, ok, data } => {
                let msg = if ok {
                    serde_json::json!({ "kind": "reply", "id": id, "ok": true, "data": data })
                } else {
                    serde_json::json!({ "kind": "reply", "id": id, "ok": false, "error": data })
                };
                post_json(&self.webview, &msg)?;
            }
            UiAction::Close => unsafe {
                let _ = DestroyWindow(hwnd);
            },
            UiAction::Show => window::set_visible(hwnd, true),
            UiAction::Minimize => window::minimize(hwnd),
            UiAction::SetTitle(title) => window::set_title(hwnd, &title),
            UiAction::SetDecorations(decorated) => {
                window::set_decorations(hwnd, decorated);
                resize_controller(&self.controller, hwnd)?;
            }
            UiAction::SetBackground { dark } => {
                set_background(&self.controller, false, dark)?;
            }
        }
        Ok(())
    }
}

pub fn available_version() -> anyhow::Result<String> {
    let mut version = PWSTR::null();
    unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) }
        .map_err(|e| anyhow::anyhow!(e))?;
    if version.is_null() {
        anyhow::bail!("webview2 missing");
    }
    let text = CoTaskMemPWSTR::from(version).to_string();
    if text.is_empty() {
        anyhow::bail!("webview2 missing");
    }
    Ok(text)
}

pub fn attach(
    hwnd: HWND,
    handle: HostHandle,
    ctx: Arc<HostCtx>,
    is_win11: bool,
    start: &str,
) -> anyhow::Result<WebViewHost> {
    let user_data = std::env::temp_dir().join("KachinaInstaller");
    let _ = std::fs::create_dir_all(&user_data);
    let user_data_w = wide(user_data.to_string_lossy().as_ref());

    let environment = {
        let (tx, rx) = mpsc::channel();
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    PCWSTR(user_data_w.as_ptr()),
                    None,
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error_code, environment| {
                error_code?;
                tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                    .expect("send env");
                Ok(())
            }),
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("webview2 environment"))??
    };

    let controller = {
        let (tx, rx) = mpsc::channel();
        let environment = environment.clone();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                environment
                    .CreateCoreWebView2Controller(hwnd, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error_code, controller| {
                error_code?;
                tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                    .expect("send controller");
                Ok(())
            }),
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("webview2 controller"))??
    };

    resize_controller(&controller, hwnd)?;
    unsafe { controller.SetIsVisible(true) }?;
    set_background(&controller, is_win11, is_dark_mode().unwrap_or(false))?;

    let webview = unsafe { controller.CoreWebView2() }?;
    unsafe {
        let settings = webview.Settings()?;
        settings.SetAreDefaultContextMenusEnabled(cfg!(debug_assertions))?;
        settings.SetAreDevToolsEnabled(cfg!(debug_assertions))?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsZoomControlEnabled(false)?;
    }

    let filter = wide(&format!("{UI_HOST}/*"));
    unsafe {
        webview.AddWebResourceRequestedFilter(
            PCWSTR(filter.as_ptr()),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        )?;
        let env = environment.clone();
        let mut token = 0;
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
                if let Some(args) = args {
                    handle_resource(&env, &args);
                }
                Ok(())
            })),
            &mut token,
        )?;
    }

    unsafe {
        let mut token = 0;
        let handle_for_msg = handle.clone();
        let ctx_for_msg = ctx.clone();
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
                if let Some(args) = args {
                    let mut message = PWSTR::null();
                    if args.WebMessageAsJson(&mut message).is_ok() {
                        let json = CoTaskMemPWSTR::from(message).to_string();
                        bridge::on_message(&ctx_for_msg, &handle_for_msg, &json);
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
    }

    let start_w = wide(start);
    unsafe { webview.Navigate(PCWSTR(start_w.as_ptr())) }?;

    Ok(WebViewHost {
        controller,
        webview,
    })
}

fn handle_resource(
    env: &ICoreWebView2Environment,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let mut uri = PWSTR::null();
    if unsafe { request.Uri(&mut uri) }.is_err() {
        return;
    }
    let uri = CoTaskMemPWSTR::from(uri).to_string();
    let Some(path) = uri.strip_prefix(UI_HOST) else {
        return;
    };
    let path = path.split('?').next().unwrap_or(path);
    let Some((bytes, mime)) = assets::lookup(path) else {
        if let Ok(resp) = make_response(env, b"not found", 404, "text/plain") {
            let _ = unsafe { args.SetResponse(&resp) };
        }
        return;
    };
    if let Ok(resp) = make_response(env, bytes, 200, mime) {
        let _ = unsafe { args.SetResponse(&resp) };
    }
}

fn make_response(
    env: &ICoreWebView2Environment,
    bytes: &[u8],
    status: i32,
    mime: &str,
) -> anyhow::Result<ICoreWebView2WebResourceResponse> {
    let stream = unsafe { SHCreateMemStream(Some(bytes)) }.context("SHCreateMemStream")?;
    let stream: IStream = stream;
    let headers = wide(&format!(
        "Content-Type: {mime}\nAccess-Control-Allow-Origin: *\nCache-Control: no-cache"
    ));
    let reason = wide(if status == 200 { "OK" } else { "Not Found" });
    let response = unsafe {
        env.CreateWebResourceResponse(
            &stream,
            status,
            PCWSTR(reason.as_ptr()),
            PCWSTR(headers.as_ptr()),
        )
    }?;
    Ok(response)
}

fn post_json(webview: &ICoreWebView2, value: &Value) -> anyhow::Result<()> {
    let text = value.to_string();
    let wide = wide(&text);
    unsafe { webview.PostWebMessageAsJson(PCWSTR(wide.as_ptr())) }?;
    Ok(())
}

fn resize_controller(controller: &ICoreWebView2Controller, hwnd: HWND) -> anyhow::Result<()> {
    let (cx, cy) = window::client_size(hwnd);
    unsafe {
        controller.SetBounds(RECT {
            left: 0,
            top: 0,
            right: cx,
            bottom: cy,
        })?;
    }
    Ok(())
}

fn set_background(
    controller: &ICoreWebView2Controller,
    mica: bool,
    dark: bool,
) -> anyhow::Result<()> {
    let color = if mica {
        COREWEBVIEW2_COLOR {
            A: 0,
            R: 0,
            G: 0,
            B: 0,
        }
    } else if dark {
        COREWEBVIEW2_COLOR {
            A: 255,
            R: 0,
            G: 0,
            B: 0,
        }
    } else {
        COREWEBVIEW2_COLOR {
            A: 255,
            R: 255,
            G: 255,
            B: 255,
        }
    };
    if let Ok(ctrl2) = controller.cast::<ICoreWebView2Controller2>() {
        unsafe { ctrl2.SetDefaultBackgroundColor(color) }?;
    }
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
