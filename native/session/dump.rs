#[cfg(debug_assertions)]
use anyhow::Context;
#[cfg(debug_assertions)]
use serde::Serialize;
#[cfg(debug_assertions)]
use std::path::Path;

macro_rules! session_dump {
    ($dir:expr, $name:expr, $data:expr) => {{
        #[cfg(debug_assertions)]
        $crate::session::dump::write_dump($dir, $name, &$data).await;
        #[cfg(not(debug_assertions))]
        {
            let _ = &$dir;
            let _ = $name;
        }
    }};
}
pub(crate) use session_dump;

#[cfg(debug_assertions)]
pub async fn write_dump(dir: Option<&Path>, name: &str, data: &impl Serialize) {
    let Some(dir) = dir else {
        return;
    };
    if let Err(err) = write_dump_inner(dir, name, data).await {
        tracing::warn!("session dump {name} failed: {err:#}");
    }
}

#[cfg(debug_assertions)]
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
