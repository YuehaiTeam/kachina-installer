pub mod acl;
pub mod code;
pub mod dir;
pub mod error;
pub mod folderdialog;
pub mod gui;
pub mod hash;
pub mod i18n;
pub mod icon;
pub mod log;
pub mod metadata;
pub mod process;
pub mod progressed_read;
pub mod sentry;
pub mod taskdialog;
pub mod uac;
pub mod url;
pub mod wincred;

pub fn get_device_id() -> anyhow::Result<String> {
    let username = whoami::username();
    let key = windows_registry::LOCAL_MACHINE
        .options()
        .read()
        .open(r#"SOFTWARE\Microsoft\Cryptography"#)?;

    let guid: String = key.get_string("MachineGuid")?;
    let raw_device_id = format!("{username}{guid}");
    Ok(chksum_md5::hash(raw_device_id).to_hex_uppercase())
}
