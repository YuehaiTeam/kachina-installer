//! 用 shell 的 `IShellLinkW` 写 `.lnk`，安装器的桌面与开始菜单快捷方式和 kachina-sfx 的来源
//! 快捷方式共用。目标的条目列表、LinkInfo 与相对路径都由 shell 生成，任何 shell 能打开的路径
//! （含 UNC）都可作目标。

use std::path::Path;

use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

/// 写 `link`，目标为 `target`、不带参数，起始位置为 `working_dir`；已存在的 `link` 被覆盖。
/// 会阻塞，并在调用线程上初始化 COM（MTA）、返回前反初始化；调用线程已按 STA 初始化时
/// 直接沿用。
pub fn create(target: &Path, link: &Path, working_dir: &Path) -> windows::core::Result<()> {
    // S_OK 与 S_FALSE 都要配对反初始化；RPC_E_CHANGED_MODE 表示线程已是 STA，不配对
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let result = unsafe { write(target, link, working_dir) };
    if initialized {
        unsafe { CoUninitialize() };
    }
    result
}

unsafe fn write(target: &Path, link: &Path, working_dir: &Path) -> windows::core::Result<()> {
    let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
    shell_link.SetPath(&HSTRING::from(target.as_os_str()))?;
    shell_link.SetWorkingDirectory(&HSTRING::from(working_dir.as_os_str()))?;
    shell_link
        .cast::<IPersistFile>()?
        .Save(&HSTRING::from(link.as_os_str()), true)
}
