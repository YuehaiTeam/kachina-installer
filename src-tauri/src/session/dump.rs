use crate::cli::arg::InstallArgs;
use crate::utils::error::TAResult;
use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use tauri::State;

pub async fn write_dump(dir: Option<&Path>, name: &str, data: &impl Serialize) {
    let Some(dir) = dir else {
        return;
    };
    if let Err(err) = write_dump_inner(dir, name, data).await {
        tracing::warn!("session dump {name} failed: {err:#}");
    }
}

async fn write_dump_inner(dir: &Path, name: &str, data: &impl Serialize) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dir)
        .await
        .context("DUMP_DIR_ERR")?;
    let body = serde_json::to_vec_pretty(data).context("DUMP_SERIALIZE_ERR")?;
    tokio::fs::write(dir.join(name), body)
        .await
        .context("DUMP_WRITE_ERR")?;
    Ok(())
}

#[tauri::command]
pub async fn write_session_dump(
    args: State<'_, InstallArgs>,
    name: String,
    data: Value,
) -> TAResult<()> {
    let Some(dir) = args.dump_dir.as_ref() else {
        return Ok(());
    };
    tokio::fs::create_dir_all(dir)
        .await
        .context("DUMP_DIR_ERR")?;
    let path = dir.join(name);
    let body = serde_json::to_vec_pretty(&data).context("DUMP_SERIALIZE_ERR")?;
    tokio::fs::write(path, body).await.context("DUMP_WRITE_ERR")?;
    Ok(())
}
