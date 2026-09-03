//! 子进程拉起：直接调用 `CreateProcessW`，不经 `std::process::Command`。
//!
//! 安装器只有两种用法：拉起后等退出码（运行时安装器、WebView2 引导器）与拉起即走
//! （崩溃对话框、退出后自删的 `cmd`）。std 的 `Command` 为通用性携带 stdio 管道、
//! 环境块合并、PATH 解析等代码，本模块只保留命令行拼接与一个 `hide_window` 开关。
//!
//! 子进程不继承句柄、不进 Job，父进程退出后继续存活；拉起即走的调用方直接丢弃
//! 返回的 [`Child`] 即可。

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, GetProcessId, WaitForSingleObject, CREATE_NO_WINDOW,
    INFINITE, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// 已拉起的子进程句柄，Drop 时关闭句柄，不影响子进程运行。
pub struct Child(HANDLE);

// HANDLE 是进程句柄，跨线程使用安全。
unsafe impl Send for Child {}

impl Child {
    /// 阻塞等待子进程退出并返回退出码。
    pub async fn wait(self) -> io::Result<u32> {
        tokio::task::spawn_blocking(move || self.wait_blocking())
            .await
            .map_err(io::Error::other)?
    }

    pub fn wait_blocking(&self) -> io::Result<u32> {
        unsafe {
            if WaitForSingleObject(self.0, INFINITE) == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.0, &mut code)?;
            Ok(code)
        }
    }

    pub fn pid(&self) -> u32 {
        unsafe { GetProcessId(self.0) }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 拉起 `program`，参数按 Windows 命令行规则引号转义。
/// `hide_window` 为 true 时以 `CREATE_NO_WINDOW` 拉起，控制台子进程不弹窗。
/// `program` 可以是绝对路径，也可以是交给系统按 `CreateProcessW` 规则搜索的名字（如 `cmd`）。
pub fn spawn<P: AsRef<OsStr>, A: AsRef<OsStr>>(
    program: P,
    args: &[A],
    hide_window: bool,
) -> io::Result<Child> {
    let mut cmdline = make_command_line(program.as_ref(), args.iter().map(AsRef::as_ref));
    cmdline.push(0);

    let flags = if hide_window {
        CREATE_NO_WINDOW
    } else {
        PROCESS_CREATION_FLAGS(0)
    };
    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )?;
        let _ = CloseHandle(pi.hThread);
    }
    Ok(Child(pi.hProcess))
}

/// 以 shell 默认动词打开 `target`：可执行文件直接运行，URL 交给默认浏览器，
/// 文件与目录交给关联程序。失败不弹系统对话框，由调用方决定如何呈现。
/// 会阻塞到 shell 完成派发，调用方在异步上下文中应放到 `spawn_blocking`。
pub fn shell_open<P: AsRef<OsStr>>(target: P) -> io::Result<()> {
    let file = HSTRING::from(target.as_ref());
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut sei)? };
    Ok(())
}

/// 与 `std::process::Command` 在 Windows 上的拼接规则一致：argv[0] 总是加引号；
/// 其余参数为空或含空格 / 制表符时加引号，引号前的反斜杠成倍转义。
fn make_command_line<'a>(program: &OsStr, args: impl Iterator<Item = &'a OsStr>) -> Vec<u16> {
    let mut cmd = Vec::new();
    append_arg(&mut cmd, program, true);
    for arg in args {
        cmd.push(b' ' as u16);
        append_arg(&mut cmd, arg, false);
    }
    cmd
}

fn append_arg(cmd: &mut Vec<u16>, arg: &OsStr, force_quotes: bool) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let quote = force_quotes
        || arg.is_empty()
        || arg
            .encode_wide()
            .any(|c| c == b' ' as u16 || c == b'\t' as u16);
    if quote {
        cmd.push(QUOTE);
    }
    let mut backslashes = 0usize;
    for x in arg.encode_wide() {
        if x == BACKSLASH {
            backslashes += 1;
        } else {
            if x == QUOTE {
                // 内部引号前需要 2n+1 个反斜杠：已有 n 个，再补 n+1 个。
                cmd.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        cmd.push(x);
    }
    if quote {
        // 结尾引号前需要 2n 个反斜杠：已有 n 个，再补 n 个。
        cmd.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        cmd.push(QUOTE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmdline(program: &str, args: &[&str]) -> String {
        let v = make_command_line(OsStr::new(program), args.iter().map(OsStr::new));
        String::from_utf16(&v).unwrap()
    }

    #[test]
    fn program_is_always_quoted_and_plain_args_are_not() {
        assert_eq!(cmdline("cmd", &["/C", "del", "/f"]), r#""cmd" /C del /f"#);
    }

    #[test]
    fn args_with_spaces_quotes_and_backslashes_are_escaped() {
        assert_eq!(
            cmdline(
                r"C:\Program Files\x.exe",
                &["a b", "", r"C:\dir\", r#"say "hi""#]
            ),
            r#""C:\Program Files\x.exe" "a b" "" C:\dir\ "say \"hi\"""#
        );
        assert_eq!(cmdline("x", &[r"C:\my dir\"]), r#""x" "C:\my dir\\""#);
    }

    #[test]
    fn spawn_hidden_cmd_and_read_exit_code() {
        let child = spawn("cmd", &["/C", "exit", "7"], true).unwrap();
        assert_eq!(child.wait_blocking().unwrap(), 7);
    }

    #[test]
    fn spawn_missing_program_fails() {
        assert!(spawn("kachina-no-such-program-xyz", &[""; 0], true).is_err());
    }

    #[test]
    fn shell_open_missing_target_fails_without_dialog() {
        let missing = std::env::temp_dir().join("kachina-no-such-target-xyz.bin");
        assert!(shell_open(&missing).is_err());
    }
}
