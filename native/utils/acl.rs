use windows::{
    core::w,
    Win32::Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
};

/// Security of the elevation pipe: full access for SYSTEM and Administrators
/// only. Administrators is deny-only in a filtered (non-elevated) token, so
/// only elevated processes can open the pipe; the creator keeps its own
/// handle. The default medium integrity label applies.
pub fn create_security_attributes() -> SECURITY_ATTRIBUTES {
    let mut security_descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:(A;;GA;;;SY)(A;;GA;;;BA)"),
            SDDL_REVISION,
            &mut security_descriptor,
            None,
        )
        .unwrap();

        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security_descriptor.0,
            bInheritHandle: false.into(),
        }
    }
}
