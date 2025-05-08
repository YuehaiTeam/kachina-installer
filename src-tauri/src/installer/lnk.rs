use anyhow::{anyhow, Context, Result};
use std::path::Path;
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_PublicDesktop,
};

use crate::utils::{
    dir::get_dir,
    error::{IntoAnyhow, TAResult},
};

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
pub struct CreateLnkArgs {
    pub target: String,
    pub lnk: String,
}
pub async fn create_lnk_with_args(args: CreateLnkArgs) -> Result<()> {
    create_lnk(args.target, args.lnk).await.into_anyhow()
}

#[tauri::command]
pub async fn create_lnk(target: String, lnk: String) -> TAResult<()> {
    let target = Path::new(&target);
    let lnk = Path::new(&lnk);
    let lnk_dir = lnk.parent();
    if lnk_dir.is_none() {
        return Err(anyhow!("Failed to get lnk parent dir")
            .context("CREATE_LNK_ERR")
            .into());
    }
    let lnk_dir = lnk_dir.unwrap();
    tokio::fs::create_dir_all(lnk_dir)
        .await
        .context("CREATE_LNK_ERR")?;
    let sl = mslnk::ShellLink::new(target).context("CREATE_LNK_ERR")?;
    sl.create_lnk(lnk).context("CREATE_LNK_ERR")?;
    Ok(())
}

#[tauri::command]
pub async fn get_dirs(elevated: bool) -> TAResult<(String, String)> {
    if elevated {
        Ok((
            get_dir(&FOLDERID_CommonPrograms)?,
            get_dir(&FOLDERID_PublicDesktop)?,
        ))
    } else {
        Ok((get_dir(&FOLDERID_Programs)?, get_dir(&FOLDERID_Desktop)?))
    }
}
