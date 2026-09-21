use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;

use anyhow::Context;
use serde_json::Value;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_F12, VK_I, VK_SHIFT,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

fn devtools_enabled() -> bool {
    cfg!(debug_assertions) || cfg!(feature = "webview-devtools")
}

use super::assets;
use super::bridge;
use super::window;
use super::{HostCtx, HostHandle, UiAction, UI_HOST};
use crate::utils::gui::is_dark_mode;

/// Host→page `PostWebMessageAsJson` posted before the first
/// `NavigationCompleted` is dropped; on Windows 7 (WebView2 ~109) a post
/// during that first navigation also prevents later posts from reaching the
/// same document.
struct PostGate {
    ready: bool,
    pending: Vec<Value>,
}

pub struct WebViewHost {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    mica: bool,
    posts: Rc<RefCell<PostGate>>,
}

impl WebViewHost {
    pub fn open_devtools(&self) -> anyhow::Result<()> {
        unsafe { self.webview.OpenDevToolsWindow() }.context("OpenDevToolsWindow")?;
        Ok(())
    }

    pub fn apply(&self, hwnd: HWND, action: UiAction) -> anyhow::Result<()> {
        match action {
            UiAction::Emit { event, payload } => {
                enqueue_or_post(
                    &self.webview,
                    &self.posts,
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
                enqueue_or_post(&self.webview, &self.posts, &msg)?;
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
                set_background(&self.controller, self.mica, dark)?;
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
        settings.SetAreDevToolsEnabled(devtools_enabled())?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsZoomControlEnabled(false)?;
        settings.SetIsWebMessageEnabled(true)?;
    }
    if devtools_enabled() {
        bind_devtools_shortcut(&controller, &webview)?;
    }
    inject_error_hook(&webview)?;

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

    let posts = Rc::new(RefCell::new(PostGate {
        ready: false,
        pending: Vec::new(),
    }));
    unsafe {
        let mut token = 0;
        let posts_nav = posts.clone();
        let webview_nav = webview.clone();
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
                let mut ok = windows::core::BOOL::default();
                if let Some(args) = args {
                    let _ = args.IsSuccess(&mut ok);
                }
                if !ok.as_bool() {
                    return Ok(());
                }
                let pending = {
                    let mut gate = posts_nav.borrow_mut();
                    gate.ready = true;
                    std::mem::take(&mut gate.pending)
                };
                for msg in &pending {
                    if let Err(err) = post_json(&webview_nav, msg) {
                        tracing::warn!("flush web message failed: {err}");
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
        mica: is_win11,
        posts,
    })
}

fn bind_devtools_shortcut(
    controller: &ICoreWebView2Controller,
    webview: &ICoreWebView2,
) -> anyhow::Result<()> {
    let webview = webview.clone();
    let mut token = 0i64;
    unsafe {
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_sender, args| {
                if let Some(args) = args {
                    if is_devtools_hotkey(&args) {
                        let _ = args.SetHandled(true);
                        let _ = webview.OpenDevToolsWindow();
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

fn is_devtools_hotkey(args: &ICoreWebView2AcceleratorKeyPressedEventArgs) -> bool {
    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
    if unsafe { args.KeyEventKind(&mut kind) }.is_err() {
        return false;
    }
    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
        && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
    {
        return false;
    }
    let mut vk = 0u32;
    if unsafe { args.VirtualKey(&mut vk) }.is_err() {
        return false;
    }
    if vk == VK_F12.0 as u32 {
        return true;
    }
    vk == VK_I.0 as u32 && key_down(VK_CONTROL) && key_down(VK_SHIFT)
}

fn key_down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    let state = unsafe { GetKeyState(i32::from(vk.0)) };
    state < 0
}

/// ES5：在页面脚本之前挂上，把未捕获错误打到 `%TEMP%\KachinaInstaller.log`。
const ERROR_HOOK: &str = r#"
window.addEventListener("error", function (e) {
  try {
    chrome.webview.postMessage({
      id: 0,
      kind: "invoke",
      cmd: "error",
      args: {
        data: "webview uncaught " + (e.message || "") + " " + (e.filename || "") + ":" + (e.lineno || 0)
      }
    });
  } catch (x) {}
});
window.addEventListener("unhandledrejection", function (e) {
  try {
    var r = e.reason;
    chrome.webview.postMessage({
      id: 0,
      kind: "invoke",
      cmd: "error",
      args: {
        data: "webview unhandledrejection " + (r && r.stack ? r.stack : String(r))
      }
    });
  } catch (x) {}
});
"#;

fn inject_error_hook(webview: &ICoreWebView2) -> anyhow::Result<()> {
    let script = wide(ERROR_HOOK);
    unsafe {
        webview.AddScriptToExecuteOnDocumentCreated(
            PCWSTR(script.as_ptr()),
            &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(
                |_error, _id| Ok(()),
            )),
        )?;
    }
    Ok(())
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

fn enqueue_or_post(
    webview: &ICoreWebView2,
    posts: &RefCell<PostGate>,
    value: &Value,
) -> anyhow::Result<()> {
    let mut gate = posts.borrow_mut();
    if gate.ready {
        drop(gate);
        return post_json(webview, value);
    }
    gate.pending.push(value.clone());
    Ok(())
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
