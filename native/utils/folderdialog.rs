//! System folder picker: Common Item Dialog (`IFileOpenDialog` with
//! `FOS_PICKFOLDERS`). Runs on a blocking thread that owns its own STA so the
//! caller's runtime thread never has to be COM-initialised.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_PICKFOLDERS,
    SIGDN_FILESYSPATH,
};

use crate::host::HwndParent;

/// Shows the folder picker starting at `initial` (silently ignored when the
/// directory does not exist yet, in which case the shell picks its own start
/// folder). `None` when the user cancels or the dialog cannot be shown.
pub async fn pick_folder(initial: String, parent: HwndParent) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let init =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let picked = show(&initial, parent.hwnd());
        if init.is_ok() {
            unsafe { CoUninitialize() };
        }
        match picked {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("folder picker failed: {e}");
                None
            }
        }
    })
    .await
    .ok()
    .flatten()
}

fn show(initial: &str, parent: HWND) -> windows::core::Result<Option<String>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(FOS_PICKFOLDERS)?;

        let wide: Vec<u16> = initial.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(item) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None)
        {
            // SetDefaultFolder only sets a fallback; SetFolder forces the start folder.
            let _ = dialog.SetFolder(&item);
        }

        if let Err(e) = dialog.Show(Some(parent)) {
            return if e.code() == ERROR_CANCELLED.to_hresult() {
                Ok(None)
            } else {
                Err(e)
            };
        }

        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(path)
    }
}
