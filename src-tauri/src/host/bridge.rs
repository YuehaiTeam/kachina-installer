use std::sync::Arc;

use serde_json::{json, Value};

use super::{HostCtx, HostHandle, UiAction};
use crate::session::state::Intent;
use crate::utils::code::{extract, Extracted};
use crate::utils::error::TACommandError;
use crate::utils::taskdialog::{
    show_error, task_dialog, CommandLink, ErrorDialog, TaskDialogRequest,
};

#[derive(Debug, serde::Deserialize)]
struct InvokeMessage {
    id: u64,
    kind: String,
    cmd: String,
    #[serde(default)]
    args: Value,
}

pub fn on_message(ctx: &Arc<HostCtx>, handle: &HostHandle, json: &str) {
    let Ok(msg) = serde_json::from_str::<InvokeMessage>(json) else {
        return;
    };
    if msg.kind != "invoke" {
        return;
    }
    let ctx = ctx.clone();
    let handle = handle.clone();
    tokio::spawn(async move {
        let closing = msg.cmd == "window_close" || msg.cmd == "launch_and_exit";
        let result = dispatch(&ctx, &handle, &msg.cmd, msg.args).await;
        let (ok, data) = match result {
            Ok(data) => (true, data),
            Err(err) => {
                let event_id = err.report_if_needed();
                (false, error_payload(&err, event_id))
            }
        };
        handle.send(UiAction::Reply {
            id: msg.id,
            ok,
            data,
        });
        if closing && ok {
            handle.close();
        }
    });
}

/// Same shape as `Coded` plus `insight`, so the renderer can hand it straight to
/// `error_dialog`. `code` is null for cancelled and uncoded errors.
fn error_payload(err: &TACommandError, event_id: Option<String>) -> Value {
    let (code, detail, subject, sid) = match extract(&err.error) {
        Extracted::Coded(c) => (
            Value::String(c.code.to_string()),
            json_opt(c.detail.as_deref()),
            json_opt(c.subject.as_deref()),
            json_opt(c.sid.as_deref()),
        ),
        Extracted::Cancelled => (
            Value::Null,
            Value::String("cancelled".into()),
            Value::Null,
            Value::Null,
        ),
        Extracted::Uncoded { detail } => (Value::Null, json_opt(Some(&detail)), Value::Null, Value::Null),
    };
    json!({
        "code": code,
        "detail": detail,
        "subject": subject,
        "sid": sid,
        "event_id": json_opt(event_id.as_deref()),
        "insight": err.insight,
    })
}

fn json_opt(v: Option<&str>) -> Value {
    match v {
        Some(s) if !s.is_empty() => Value::String(s.to_string()),
        _ => Value::Null,
    }
}

async fn dispatch(
    ctx: &HostCtx,
    handle: &HostHandle,
    cmd: &str,
    args: Value,
) -> Result<Value, TACommandError> {
    if ctx.plugin_runtime
        && !matches!(
            cmd,
            "plugin_host_ready"
                | "answer_session_plugin"
                | "http_get_request"
                | "log"
                | "warn"
                | "error"
                | "launch"
        )
    {
        return Err(TACommandError::new(anyhow::anyhow!(
            "command disabled in plugin runtime: {cmd}"
        )));
    }
    match cmd {
        "plugin_host_ready" => {
            if let Some(tx) = ctx
                .plugin_ready
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            ok(())
        }
        "intent" => {
            let intent: Intent = serde_json::from_value(args)
                .map_err(|e| TACommandError::new(anyhow::anyhow!(e)))?;
            crate::session::commands::handle_intent(intent, ctx, handle).await
        }
        "pick_path" => {
            let gui = ctx
                .gui
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| TACommandError::new(anyhow::anyhow!("gui session not ready")))?;
            let snap = gui.snapshot();
            let app_name = gui
                .project
                .as_ref()
                .map(|p| p.app_name.clone())
                .unwrap_or_default();
            let exe_name = gui
                .project
                .as_ref()
                .map(|p| p.exe_name.clone())
                .unwrap_or_default();
            let path = crate::installer::pick_install_path(
                &snap.options.install_path,
                &exe_name,
                &app_name,
                handle.parent(),
            )
            .await
            .unwrap_or_default();
            ok(path)
        }
        "error_dialog" => {
            let code = req_str(&args, &["code"])?;
            let detail = opt_str(&args, &["detail"]);
            let subject = opt_str(&args, &["subject"]);
            let sid = opt_str(&args, &["sid"]);
            let event_id = opt_str(&args, &["event_id", "eventId"]);
            let parent = handle.hwnd().0 as isize;
            tokio::task::spawn_blocking(move || {
                show_error(
                    ErrorDialog {
                        code: &code,
                        detail: detail.as_deref(),
                        subject: subject.as_deref(),
                        sid: sid.as_deref(),
                        event_id: event_id.as_deref(),
                    },
                    windows::Win32::Foundation::HWND(parent as *mut _),
                );
            })
            .await
            .ok();
            ok(())
        }
        "task_dialog" => {
            let title = req_str(&args, &["title"]).unwrap_or_default();
            let content = req_str(&args, &["content"]).unwrap_or_default();
            let expanded = opt_str(&args, &["expanded"]);
            let footer = opt_str(&args, &["footer"]);
            let buttons = args
                .get("buttons")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| {
                            Some(CommandLink {
                                id: b.get("id")?.as_i64()? as i32,
                                text: b.get("text")?.as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let parent = handle.hwnd().0 as isize;
            let clicked = tokio::task::spawn_blocking(move || {
                task_dialog(
                    TaskDialogRequest {
                        title,
                        content,
                        expanded,
                        footer,
                        buttons,
                    },
                    windows::Win32::Foundation::HWND(parent as *mut _),
                )
            })
            .await
            .unwrap_or(0);
            ok(clicked)
        }
        "answer_session_plugin" => ok(ctx
            .session
            .plugins
            .answer(crate::session::ui::PluginAnswer {
                id: req_str(&args, &["id"])?,
                ok: req_bool(&args, &["ok"])?,
                data: opt_str(&args, &["data"]),
                error: opt_str(&args, &["error"]),
                unimplemented: opt_bool(&args, &["unimplemented"]).unwrap_or(false),
            })
            .await),
        "http_get_request" => {
            let url = req_str(&args, &["url"])?;
            let res = crate::dfs::http_get_request(
                url,
                opt_bool(&args, &["ignoreRedirects", "ignore_redirects"]),
                opt_headers(&args),
                opt_u64(&args, &["timeoutMs", "timeout_ms"]),
            )
            .await
            .map_err(TACommandError::new)?;
            ok(res)
        }
        "launch" => {
            let path = req_str(&args, &["path"])?;
            if ctx.plugin_runtime && !is_http_or_https_url(&path) {
                return Err(TACommandError::new(anyhow::anyhow!(
                    "command disabled in plugin runtime: {cmd} (only http(s) URLs allowed)"
                )));
            }
            crate::installer::launch(path).await;
            ok(())
        }
        "launch_and_exit" => {
            crate::installer::launch(req_str(&args, &["path"])?).await;
            ok(())
        }
        "log" => {
            crate::installer::log(string_arg(&args, "data"));
            ok(())
        }
        "warn" => {
            crate::installer::warn(string_arg(&args, "data"));
            ok(())
        }
        "error" => {
            crate::installer::error(string_arg(&args, "data"));
            ok(())
        }
        "window_show" => {
            handle.send(UiAction::Show);
            if let Some(gui) = ctx.gui.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                gui.emit(handle);
            }
            ok(())
        }
        "window_close" => ok(()),
        "window_minimize" => {
            handle.send(UiAction::Minimize);
            ok(())
        }
        "window_set_title" => {
            handle.send(UiAction::SetTitle(req_str(&args, &["title"])?));
            ok(())
        }
        "window_set_decorations" => {
            handle.send(UiAction::SetDecorations(req_bool(&args, &["decorations"])?));
            ok(())
        }
        other => Err(TACommandError::new(anyhow::anyhow!(
            "unknown command: {other}"
        ))),
    }
}

fn is_http_or_https_url(path: &str) -> bool {
    let lower = path.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn field<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| args.get(*key))
}

fn req_str(args: &Value, keys: &[&str]) -> Result<String, TACommandError> {
    field(args, keys)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| TACommandError::new(anyhow::anyhow!("missing string field {}", keys[0])))
}

fn opt_str(args: &Value, keys: &[&str]) -> Option<String> {
    field(args, keys)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn req_bool(args: &Value, keys: &[&str]) -> Result<bool, TACommandError> {
    field(args, keys)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| TACommandError::new(anyhow::anyhow!("missing bool field {}", keys[0])))
}

fn opt_bool(args: &Value, keys: &[&str]) -> Option<bool> {
    field(args, keys).and_then(|v| v.as_bool())
}

fn opt_u64(args: &Value, keys: &[&str]) -> Option<u64> {
    field(args, keys).and_then(|v| v.as_u64())
}

fn opt_headers(args: &Value) -> Option<std::collections::HashMap<String, String>> {
    args.get("headers")?.as_object().map(|obj| {
        obj.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()
    })
}

fn ok<T: serde::Serialize>(value: T) -> Result<Value, TACommandError> {
    serde_json::to_value(value).map_err(|e| TACommandError::new(anyhow::anyhow!(e)))
}

fn string_arg(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}
