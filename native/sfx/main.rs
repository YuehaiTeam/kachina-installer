//! 自解压免安装运行器：把包内数据还原到临时目录、运行应用主体、退出后清理。
//!
//! 包格式与安装包一致（base + \0CONFIG + \0META + hash 命名的 zstd 数据块），
//! 由 builder 的 `replace-bin --base` 换头产出。设计见
//! docs/notes/implemented/2026-11-03-sfx-portable-runner.md。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dialog;
mod reaper;
mod run;

// 共享模块里 sfx 只用得到一部分（进程拉起、提权、元数据、包格式读取），
// 其余导出属于安装器侧，这里整体允许未使用。sfx 不接日志：被包装应用继承同一
// 控制台，sfx 不往 stdout 写任何东西。
#[path = "../utils/metadata.rs"]
#[allow(dead_code)]
mod metadata;
#[path = "../pack_format.rs"]
#[allow(dead_code)]
mod pack_format;
#[path = "../utils/process.rs"]
#[allow(dead_code)]
mod process;
#[path = "../utils/uac.rs"]
#[allow(dead_code)]
mod uac;

fn main() {
    use windows::Win32::System::Console::{AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS};
    // 从终端启动时接上父控制台：控制台与输出重定向（管道/文件）场景不弹
    // 对话框（也不显示进度），错误写 stderr。
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    let console = attached || unsafe { !GetConsoleWindow().0.is_null() } || stdio_redirected();

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
