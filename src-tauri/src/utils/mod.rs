pub mod acl;
pub mod dir;
pub mod error;
pub mod gui;
pub mod hash;
pub mod metadata;
pub mod progressed_read;
pub mod sentry;
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
