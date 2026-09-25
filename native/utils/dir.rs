use anyhow::{Context, Result};
use std::path::Path;
use windows::{
    core::{GUID, PCWSTR, PWSTR},
    Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetDriveTypeW, QueryDosDeviceW},
        UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
            FOLDERID_LocalAppDataLow, FOLDERID_RoamingAppData, GetUserProfileDirectoryW,
            SHGetKnownFolderPath, KF_FLAG_DEFAULT,
        },
    },
};

pub fn get_dir(dir: &GUID) -> Result<String> {
    let pwstr = unsafe {
        SHGetKnownFolderPath(dir, KF_FLAG_DEFAULT, None)
            .map(|pwstr| pwstr.to_string().context("INTERNAL_ERROR"))
            .context("GET_KNOWNFOLDER_ERR")??
    };
    Ok(pwstr)
}

pub fn get_userprofile() -> Result<String> {
    let mut buffer = [0u16; 1024];
    let pwstr = PWSTR::from_raw(buffer.as_mut_ptr());
    let mut size = buffer.len() as u32;
    unsafe { GetUserProfileDirectoryW(HANDLE::default(), Some(pwstr), &mut size) }
        .context("GET_KNOWNFOLDER_ERR")?;
    Ok(unsafe { pwstr.to_string().context("INTERNAL_ERROR")? })
}

/// Whether `path` sits on a drive letter that is a network mapping or a `subst`
/// alias. Both belong to the logon session that created them; with default
/// `EnableLinkedConnections` an elevated token does not see them.
pub fn on_session_drive(path: &str) -> bool {
    const DRIVE_REMOTE: u32 = 4;
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    let letter = &path[..2];
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let root = wide(&format!("{letter}\\"));
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } == DRIVE_REMOTE {
        return true;
    }
    let device = wide(letter);
    let mut target = [0u16; 1024];
    let len = unsafe { QueryDosDeviceW(PCWSTR(device.as_ptr()), Some(&mut target)) } as usize;
    len > 0 && String::from_utf16_lossy(&target[..len]).starts_with(r"\??\")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_unc_paths_are_not_session_drives() {
        let temp = std::env::temp_dir();
        assert!(!on_session_drive(&temp.to_string_lossy()));
        assert!(!on_session_drive(r"\\server\share\app"));
        assert!(!on_session_drive("relative"));
    }
}
