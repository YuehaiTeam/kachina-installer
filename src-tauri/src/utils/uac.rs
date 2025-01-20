use std::ffi::{c_void, OsStr};
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
pub struct SendableHandle(pub HANDLE);
unsafe impl Send for SendableHandle {}
unsafe impl Sync for SendableHandle {}

pub fn check_elevated() -> windows::core::Result<bool> {
    unsafe {
        let h_process = GetCurrentProcess();
        let mut h_token = HANDLE(null_mut());
        let open_result = OpenProcessToken(h_process, TOKEN_QUERY, &mut h_token);
        let mut ret_len: u32 = 0;
        let mut token_info: TOKEN_ELEVATION = zeroed();

        if let Err(e) = open_result {
            println!("OpenProcessToken {:?}", e);
            return Err(e);
        }

        if let Err(e) = GetTokenInformation(
            h_token,
            TokenElevation,
            Some(std::ptr::addr_of_mut!(token_info).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        ) {
            println!("GetTokenInformation {:?}", e);

            return Err(e);
        }

        Ok(token_info.TokenIsElevated != 0)
    }
}

pub fn run_elevated<S: AsRef<OsStr>, T: AsRef<OsStr>>(
    program_path: S,
    args: T,
) -> std::io::Result<SendableHandle> {
    let file = HSTRING::from(program_path.as_ref());
    let par = HSTRING::from(args.as_ref());

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(par.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut sei)?;
        let process = { sei.hProcess };
        if process.is_invalid() {
            return Err(std::io::Error::last_os_error());
        };
        Ok(SendableHandle(process))
    }
}
