//! 自解压免安装运行器：把包内数据还原到临时目录、运行应用主体、退出后清理。
//!
//! 包格式与安装包一致（base + \0CONFIG + \0META + hash 命名的 zstd 数据块），
//! 由 builder 的 `replace-bin --base` 换头产出。设计见
//! docs/notes/implemented/2026-11-03-sfx-portable-runner.md。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dialog;
mod local;
mod reaper;
mod run;

mod utils {
    // utils/log.rs 引用 crate::utils::sentry 的两个函数；sfx 不上报遥测，
    // 以最小桩满足引用（时间戳格式与 sentry.rs 的 rfc3339 一致）。
    pub mod sentry {
        pub fn rfc3339(secs: u64) -> String {
            let dt = chrono::DateTime::from_timestamp(secs as i64, 0).unwrap_or_default();
            dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
        }
        pub fn add_breadcrumb(_category: &str, _level: &str, _message: String) {}
    }
}

// 共享模块里 sfx 只用得到一部分（日志、进程拉起、提权、元数据），
// 其余导出属于安装器侧，这里整体允许未使用。
#[path = "../utils/log.rs"]
#[allow(dead_code)]
mod log;
#[path = "../utils/metadata.rs"]
#[allow(dead_code)]
mod metadata;
#[path = "../utils/process.rs"]
#[allow(dead_code)]
mod process;
#[path = "../utils/uac.rs"]
#[allow(dead_code)]
mod uac;

fn main() {
    use windows::Win32::System::Console::{AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS};
    // 从终端启动时接上父控制台：控制台与输出重定向（管道/文件）场景不弹
    // 对话框，错误与进度走 stderr。
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    let console = attached || unsafe { !GetConsoleWindow().0.is_null() } || stdio_redirected();

    log::init(&std::env::temp_dir().join("KachinaSfx.log"));

    let code = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run::run(console));
    std::process::exit(code);
}

/// stdout 被重定向到管道或文件时视为非交互场景。
fn stdio_redirected() -> bool {
    use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_CHAR};
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    unsafe {
        match GetStdHandle(STD_OUTPUT_HANDLE) {
            Ok(h) if !h.is_invalid() && !h.0.is_null() => GetFileType(h) != FILE_TYPE_CHAR,
            _ => false,
        }
    }
}
