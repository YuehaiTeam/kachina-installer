use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};

use super::{HostCtx, HostHandle, UiAction};
use crate::utils::error::TACommandError;

#[derive(Debug, Deserialize)]
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
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                scan_exe: bool,
            }
            let args: Args = parse(args)?;
            let cfg =
                crate::installer::config::get_installer_config(&ctx.args, args.scan_exe).await?;
            ok(cfg)
        }
        "select_dir" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                path: String,
                exe_name: String,
                silent: bool,
            }
            let args: Args = parse(args)?;
            let res = crate::installer::select_dir(
                args.path,
                args.exe_name,
                args.silent,
                handle.parent(),
            )
            .await;
            ok(res)
        }
        "start_install" => {
            #[derive(Deserialize)]
            struct Args {
                input: crate::session::types::SessionInput,
            }
            let args: Args = parse(args)?;
            let res = crate::session::commands::start_install(
                args.input,
                &ctx.args,
                &ctx.elevate,
                &ctx.session,
                handle.clone(),
            )
            .await?;
            ok(res)
        }
        "start_uninstall" => {
            #[derive(Deserialize)]
            struct Args {
                input: crate::session::types::SessionInput,
            }
            let args: Args = parse(args)?;
            let res = crate::session::commands::start_uninstall(
                args.input,
                &ctx.args,
                &ctx.elevate,
                &ctx.session,
                handle.clone(),
            )
            .await?;
            ok(res)
        }
        "answer_session_prompt" => {
            #[derive(Deserialize)]
            struct Args {
                id: String,
                accept: bool,
            }
            let args: Args = parse(args)?;
            ok(ctx.session.prompts.answer(&args.id, args.accept).await)
        }
        "answer_session_plugin" => {
            #[derive(Deserialize)]
            struct Args {
                id: String,
                ok: bool,
                data: Option<Value>,
                error: Option<String>,
                unimplemented: Option<bool>,
            }
            let args: Args = parse(args)?;
            ok(ctx
                .session
                .plugins
                .answer(crate::session::ui::PluginAnswer {
                    id: args.id,
                    ok: args.ok,
                    data: args.data,
                    error: args.error,
                    unimplemented: args.unimplemented.unwrap_or(false),
                })
                .await)
        }
        "http_get_request" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                url: String,
                ignore_redirects: Option<bool>,
                headers: Option<std::collections::HashMap<String, String>>,
                timeout_ms: Option<u64>,
            }
            let args: Args = parse(args)?;
            let res = crate::dfs::http_get_request(
                args.url,
                args.ignore_redirects,
                args.headers,
                args.timeout_ms,
            )
            .await
            .map_err(|e| TACommandError::new(anyhow::anyhow!(e)))?;
            ok(res)
        }
        "wincred_read" => {
            #[derive(Deserialize)]
            struct Args {
                target: String,
            }
            let args: Args = parse(args)?;
            ok(crate::utils::wincred::wincred_read(&args.target)?)
        }
        "wincred_write" => {
            #[derive(Deserialize)]
            struct Args {
                target: String,
                token: String,
                comment: String,
            }
            let args: Args = parse(args)?;
            crate::utils::wincred::wincred_write(&args.target, &args.token, &args.comment)?;
            ok(())
        }
        "wincred_delete" => {
            #[derive(Deserialize)]
            struct Args {
                target: String,
            }
            let args: Args = parse(args)?;
            crate::utils::wincred::wincred_delete(&args.target)?;
            ok(())
        }
        "get_mirrorc_status" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                resource_id: String,
                current_version: String,
                cdk: String,
                channel: String,
                arch: Option<String>,
                os: Option<String>,
            }
            let args: Args = parse(args)?;
            let res = crate::thirdparty::mirrorc::get_mirrorc_status(
                &args.resource_id,
                &args.current_version,
                &args.cdk,
                &args.channel,
                args.arch.as_deref(),
                args.os.as_deref(),
            )
            .await?;
            ok(res)
        }
        "read_uninstall_metadata" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                reg_name: String,
            }
            let args: Args = parse(args)?;
            ok(crate::installer::registry::read_uninstall_metadata(args.reg_name).await?)
        }
        "launch" => {
            #[derive(Deserialize)]
            struct Args {
                path: String,
            }
            let args: Args = parse(args)?;
            crate::installer::launch(args.path).await;
            ok(())
        }
        "launch_and_exit" => {
            #[derive(Deserialize)]
            struct Args {
                path: String,
            }
            let args: Args = parse(args)?;
            crate::installer::launch(args.path).await;
            handle.close();
            ok(())
        }
        "error_dialog" => {
            #[derive(Deserialize)]
            struct Args {
                title: String,
                message: String,
            }
            let args: Args = parse(args)?;
            crate::installer::error_dialog(args.title, args.message, handle.parent()).await;
            ok(())
        }
        "confirm_dialog" => {
            #[derive(Deserialize)]
            struct Args {
                title: String,
                message: String,
            }
            let args: Args = parse(args)?;
            ok(crate::installer::confirm_dialog(args.title, args.message, handle.parent()).await)
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
            #[derive(Deserialize)]
            struct Args {
                title: String,
            }
            let args: Args = parse(args)?;
            handle.send(UiAction::SetTitle(args.title));
            ok(())
        }
        "window_set_decorations" => {
            #[derive(Deserialize)]
            struct Args {
                decorations: bool,
            }
            let args: Args = parse(args)?;
            handle.send(UiAction::SetDecorations(args.decorations));
            ok(())
        }
        other => Err(TACommandError::new(anyhow::anyhow!(
            "unknown command: {other}"
        ))),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, TACommandError> {
    serde_json::from_value(args).map_err(|e| TACommandError::new(anyhow::anyhow!(e)))
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
