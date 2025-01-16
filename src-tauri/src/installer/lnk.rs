use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Path};
use windows::Win32::UI::Shell::{SHGetFolderPathW, CSIDL_COMMON_PROGRAMS, CSIDL_DESKTOPDIRECTORY};

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

fn get_start_menu_directory() -> String {
    let mut path: [u16; 260] = [0; 260];
    unsafe {
        let _ = SHGetFolderPathW(None, CSIDL_COMMON_PROGRAMS as i32, None, 0, &mut path);
    }
    OsString::from_wide(&path)
        .to_string_lossy()
        .as_ref()
        .trim_end_matches('\0')
        .to_string()
}

fn get_desktop_directory() -> String {
    use windows::Win32::UI::Shell::SHGetFolderPathW;
    let mut path: [u16; 260] = [0; 260];
    unsafe {
        let _ = SHGetFolderPathW(None, CSIDL_DESKTOPDIRECTORY as i32, None, 0, &mut path);
    }
    OsString::from_wide(&path)
        .to_string_lossy()
        .as_ref()
        .trim_end_matches('\0')
        .to_string()
}

#[tauri::command]
pub async fn get_dirs() -> Option<(String, String)> {
    Some((get_start_menu_directory(), get_desktop_directory()))
}
