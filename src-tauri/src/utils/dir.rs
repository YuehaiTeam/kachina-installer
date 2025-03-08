use std::path::Path;
use windows::{
    core::{GUID, PWSTR},
    Win32::{
        Foundation::HANDLE,
        UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
            FOLDERID_LocalAppDataLow, FOLDERID_RoamingAppData, GetUserProfileDirectoryW,
            SHGetKnownFolderPath, KF_FLAG_DEFAULT,
        },
    },
};

pub fn get_dir(dir: &GUID) -> Result<String, String> {
    let pwstr = unsafe {
        SHGetKnownFolderPath(dir, KF_FLAG_DEFAULT, None)
            .map(|pwstr| {
                pwstr
                    .to_string()
                    .map_err(|e| format!("Failed to convert pwstr: {:?}", e))
            })
            .map_err(|e| format!("Failed to get known folder path: {:?}", e))??
    };
    Ok(pwstr)
}

pub fn get_userprofile() -> Result<String, String> {
    // GetUserProfileDirectoryW(htoken, lpprofiledir, lpcchsize)
    let mut buffer = [0u16; 1024];
    let pwstr = PWSTR::from_raw(buffer.as_mut_ptr());
    let mut size = buffer.len() as u32;
    let res = unsafe { GetUserProfileDirectoryW(HANDLE::default(), Some(pwstr), &mut size) };
    if res.is_err() {
        return Err(format!(
            "Failed to get user profile directory: {:?}",
            res.err()
        ));
    }
    Ok(unsafe {
        pwstr
            .to_string()
            .map_err(|e| format!("Failed to convert pwstr: {:?}", e))?
    })
}

pub fn in_private_folder(path: &Path) -> bool {
    let path_ids = vec![
        FOLDERID_LocalAppData,
        FOLDERID_LocalAppDataLow,
        FOLDERID_RoamingAppData,
        FOLDERID_Desktop,
        FOLDERID_Documents,
        FOLDERID_Downloads,
    ];
    // first check userprofile
    let userprofile = get_userprofile();
    if let Ok(userprofile) = userprofile {
        if path.starts_with(userprofile) {
            return true;
        }
    }
    // then check known folders
    for id in path_ids {
        let known_folder = get_dir(&id);
        if let Ok(known_folder) = known_folder {
            if path.starts_with(known_folder) {
                return true;
            }
        }
    }
    false
}
