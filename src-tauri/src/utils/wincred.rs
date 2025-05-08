use anyhow::Context;
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::FILETIME,
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        },
    },
};

use super::error::TAResult;
#[tauri::command]
pub fn wincred_write(target: &str, token: &str, comment: &str) -> TAResult<()> {
    let mut comment = comment.encode_utf16().collect::<Vec<u16>>();
    comment.push(0); // Null-terminate the string
    let mut target_name = target.encode_utf16().collect::<Vec<u16>>();
    target_name.push(0); // Null-terminate the string
    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_name.as_mut_ptr()),
        Comment: PWSTR(comment.as_mut_ptr()),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: token.len() as u32,
        CredentialBlob: token.as_bytes().as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: PWSTR(std::ptr::null_mut()),
        UserName: PWSTR(std::ptr::null_mut()),
    };
    unsafe { CredWriteW(&credential, 0) }
        .map_err(|e| anyhow::anyhow!(e))
        .context("WRITE_CRED_ERR")?;
    Ok(())
}

#[tauri::command]
pub fn wincred_read(target: &str) -> TAResult<String> {
    let mut target_name = target.encode_utf16().collect::<Vec<u16>>();
    target_name.push(0); // Null-terminate the string
    let mut credential_ptr: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        CredReadW(
            PWSTR(target_name.as_mut_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut credential_ptr,
        )
    }
    .map_err(|e| anyhow::anyhow!(e))
    .context("READ_CRED_ERR")?;
    let credential = unsafe { &*credential_ptr };
    let token = unsafe {
        std::slice::from_raw_parts(
            credential.CredentialBlob,
            credential.CredentialBlobSize as usize,
        )
        .to_vec()
    };
    unsafe { CredFree(credential_ptr as *const std::ffi::c_void) };
    Ok(String::from_utf8(token)
        .map_err(|e| anyhow::anyhow!(e))
        .context("READ_CRED_ERR")?)
}

#[tauri::command]
pub fn wincred_delete(target: &str) -> TAResult<()> {
    let mut target_name = target.encode_utf16().collect::<Vec<u16>>();
    target_name.push(0); // Null-terminate the string
    unsafe { CredDeleteW(PWSTR(target_name.as_mut_ptr()), CRED_TYPE_GENERIC, None) }
        .map_err(|e| anyhow::anyhow!(e))
        .context("DELETE_CRED_ERR")?;
    Ok(())
}
