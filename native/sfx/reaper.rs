//! 残留临时目录的兜底清理（reaper）。
//! 删除条件：目录超过年龄阈值 且 没有进程映像位于该目录内。年龄阈值同时
//! 保护正在解压的新目录——解压中的目录里还没有进程映像，只能靠年龄排除。

use std::path::Path;
use std::time::{Duration, SystemTime};

pub const TEMP_PREFIX: &str = "kachina-sfx-";
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn sweep() {
    let temp = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&temp) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        // 先按名字筛：名字来自目录列举本身，其余检查每个条目都要再访问一次文件系统
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(TEMP_PREFIX))
        {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // 读不到创建时间时无法判断年龄，留给下次或操作系统
        let Ok(created) = entry.metadata().and_then(|m| m.created()) else {
            continue;
        };
        if !should_reap(created, now) {
            continue;
        }
        if dir_in_use(&path) {
            continue;
        }
        // 删不掉（文件被占用）就留到下次
        let _ = std::fs::remove_dir_all(&path);
    }
}

fn should_reap(created: SystemTime, now: SystemTime) -> bool {
    now.duration_since(created)
        .map(|age| age >= MAX_AGE)
        .unwrap_or(false)
}

/// 有任一进程的映像位于 dir 之下。两侧都先解析成规范路径再比较：同一目录可能以
/// 8.3 短名、长名或经链接的不同形式出现（`%TEMP%` 含短名、应用规范化路径后自我重启）。
/// dir 本身解析失败时按占用处理，不删。
fn dir_in_use(dir: &Path) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let Some(dir) = canonical(dir) else {
        return true;
    };
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut in_use = false;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if let Some(image) = process_image_path(entry.th32ProcessID) {
                    let image =
                        canonical(Path::new(&image)).unwrap_or_else(|| image.to_lowercase());
                    if path_within(&dir, &image) {
                        in_use = true;
                        break;
                    }
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        in_use
    }
}

fn process_image_path(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0u16; 1024];
    let mut size = buffer.len() as u32;
    let ret = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(handle);
    }
    ret.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]))
}

/// 规范路径的小写形式，去掉 `\\?\` 前缀；路径不存在或无权访问时为 `None`。
fn canonical(path: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(path).ok()?;
    let text = resolved.to_string_lossy();
    Some(text.strip_prefix(r"\\?\").unwrap_or(&text).to_lowercase())
}

/// 小写路径的前缀判定：image 位于 dir 之下（任意深度）。
fn path_within(dir: &str, image: &str) -> bool {
    let dir = dir.trim_end_matches(['\\', '/']);
    image.len() > dir.len()
        && image.starts_with(dir)
        && matches!(image.as_bytes()[dir.len()], b'\\' | b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_creation_time_is_not_reaped() {
        // 时钟回拨时 created 在未来，按未超龄处理
        let now = SystemTime::now();
        assert!(!should_reap(now + Duration::from_secs(3600), now));
    }

    #[test]
    fn path_prefix_matching() {
        let dir = r"c:\temp\kachina-sfx-abc";
        assert!(path_within(dir, r"c:\temp\kachina-sfx-abc\app.exe"));
        assert!(path_within(dir, r"c:\temp\kachina-sfx-abc\sub\tool.exe"));
        assert!(!path_within(dir, r"c:\temp\kachina-sfx-abc2\app.exe"));
        assert!(!path_within(dir, r"c:\temp\other\app.exe"));
    }
}
