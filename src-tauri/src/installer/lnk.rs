use std::path::Path;
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_PublicDesktop,
};

use crate::utils::dir::get_dir;

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
pub struct CreateLnkArgs {
    pub target: String,
    pub lnk: String,
}
pub async fn create_lnk_with_args(args: CreateLnkArgs) -> Result<(), String> {
    create_lnk(args.target, args.lnk).await
}

#[tauri::command]
pub async fn create_lnk(target: String, lnk: String) -> Result<(), String> {
    let target = Path::new(&target);
    let lnk = Path::new(&lnk);
    let lnk_dir = lnk.parent();
    if lnk_dir.is_none() {
        return Err("Failed to get lnk parent dir".to_string());
    }
    let lnk_dir = lnk_dir.unwrap();
    tokio::fs::create_dir_all(lnk_dir)
        .await
        .map_err(|e| format!("Failed to create lnk dir: {:?}", e))?;
    let sl = mslnk::ShellLink::new(target)
        .map_err(|e| format!("Failed to create shell link: {:?}", e))?;
    sl.create_lnk(lnk)
        .map_err(|e| format!("Failed to create lnk: {:?}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn get_dirs(elevated: bool) -> Result<(String, String), String> {
    if elevated {
        Ok((
            get_dir(&FOLDERID_PublicDesktop)?,
            get_dir(&FOLDERID_CommonPrograms)?,
        ))
    } else {
        Ok((get_dir(&FOLDERID_Desktop)?, get_dir(&FOLDERID_Programs)?))
    }
}
