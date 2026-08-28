use std::sync::Arc;

use serde_json::{json, Value};

use super::{HostCtx, HostHandle, UiAction};
use crate::utils::error::TACommandError;

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
        if closing {
            return;
        }
        let (ok, data) = match result {
            Ok(data) => (true, data),
            Err(err) => (
                false,
                json!({
                    "message": format!("{:#}", err.error),
                    "insight": err.insight,
                }),
            ),
        };
        handle.send(UiAction::Reply {
            id: msg.id,
            ok,
            data,
        });
    });
}

async fn dispatch(
    ctx: &HostCtx,
    handle: &HostHandle,
    cmd: &str,
    args: Value,
) -> Result<Value, TACommandError> {
    if ctx.plugin_runtime && matches!(cmd, "start_install" | "start_uninstall" | "window_show") {
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
        "get_installer_config" => {
            let scan_exe = req_bool(&args, &["scanExe", "scan_exe"])?;
            let mut cfg =
                crate::installer::config::get_installer_config(&ctx.args, scan_exe).await?;
            cfg.preset = ctx.preset.clone();
            ok(cfg)
        }
        "select_dir" => {
            let path = req_str(&args, &["path"])?;
            let exe_name = req_str(&args, &["exeName", "exe_name"])?;
            let silent = req_bool(&args, &["silent"])?;
            let res = crate::installer::select_dir(path, exe_name, silent, handle.parent()).await;
            ok(res)
        }
        "start_install" => {
            let input = parse_session_input(&args)?;
            let res = crate::session::commands::start_install(
                input,
                &ctx.args,
                &ctx.elevate,
                &ctx.session,
                handle.clone(),
            )
            .await?;
            ok(res)
        }
        "start_uninstall" => {
            let input = parse_session_input(&args)?;
            let res = crate::session::commands::start_uninstall(
                input,
                &ctx.args,
                &ctx.elevate,
                &ctx.session,
                handle.clone(),
            )
            .await?;
            ok(res)
        }
        "answer_session_prompt" => {
            let id = req_str(&args, &["id"])?;
            let accept = req_bool(&args, &["accept"])?;
            ok(ctx.session.prompts.answer(&id, accept).await)
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
            .map_err(|e| TACommandError::new(anyhow::anyhow!(e)))?;
            ok(res)
        }
        "wincred_read" => {
            let target = req_str(&args, &["target"])?;
            ok(crate::utils::wincred::wincred_read(&target)?)
        }
        "wincred_write" => {
            crate::utils::wincred::wincred_write(
                &req_str(&args, &["target"])?,
                &req_str(&args, &["token"])?,
                &req_str(&args, &["comment"])?,
            )?;
            ok(())
        }
        "wincred_delete" => {
            crate::utils::wincred::wincred_delete(&req_str(&args, &["target"])?)?;
            ok(())
        }
        "get_mirrorc_status" => {
            let resource_id = req_str(&args, &["resourceId", "resource_id"])?;
            let current_version = req_str(&args, &["currentVersion", "current_version"])?;
            let cdk = req_str(&args, &["cdk"])?;
            let channel = req_str(&args, &["channel"])?;
            let arch = opt_str(&args, &["arch"]);
            let os = opt_str(&args, &["os"]);
            let res = crate::thirdparty::mirrorc::get_mirrorc_status(
                &resource_id,
                &current_version,
                &cdk,
                &channel,
                arch.as_deref(),
                os.as_deref(),
            )
            .await?;
            ok(res)
        }
        "read_uninstall_metadata" => {
            let reg_name = req_str(&args, &["regName", "reg_name"])?;
            ok(crate::installer::registry::read_uninstall_metadata(reg_name).await?)
        }
        "launch" => {
            crate::installer::launch(req_str(&args, &["path"])?).await;
            ok(())
        }
        "launch_and_exit" => {
            crate::installer::launch(req_str(&args, &["path"])?).await;
            handle.close();
            ok(())
        }
        "error_dialog" => {
            crate::installer::error_dialog(
                req_str(&args, &["title"])?,
                req_str(&args, &["message"])?,
                handle.parent(),
            )
            .await;
            ok(())
        }
        "confirm_dialog" => ok(crate::installer::confirm_dialog(
            req_str(&args, &["title"])?,
            req_str(&args, &["message"])?,
            handle.parent(),
        )
        .await),
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
            ok(())
        }
        "window_close" => {
            handle.close();
            ok(())
        }
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

fn parse_session_input(
    args: &Value,
) -> Result<crate::session::types::SessionInput, TACommandError> {
    let input = args
        .get("input")
        .cloned()
        .ok_or_else(|| TACommandError::new(anyhow::anyhow!("missing input")))?;
    serde_json::from_value(input).map_err(|e| TACommandError::new(anyhow::anyhow!(e)))
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
