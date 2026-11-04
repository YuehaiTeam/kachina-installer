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
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(TEMP_PREFIX) {
            continue;
        }
        let created = entry
            .metadata()
            .and_then(|m| m.created())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if !should_reap(created, now) {
            continue;
        }
        if dir_in_use(&path) {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(&path) {
            tracing::info!("reaper 跳过 {}: {e}", path.display());
        } else {
            tracing::info!("reaper 清理残留 {}", path.display());
        }
    }
}

fn should_reap(created: SystemTime, now: SystemTime) -> bool {
    now.duration_since(created)
        .map(|age| age >= MAX_AGE)
        .unwrap_or(false)
}

/// 有任一进程的映像路径位于 dir 内（含 dir 本身的前缀路径）。
fn dir_in_use(dir: &Path) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
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
                    if path_within(dir, &image) {
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

/// 大小写不敏感的路径前缀判定：image 必须等于 dir 或位于其下一层。
fn path_within(dir: &Path, image: &str) -> bool {
    let dir_lower = dir.to_string_lossy().to_lowercase();
    let dir_lower = dir_lower.trim_end_matches(['\\', '/']);
    let image_lower = image.to_lowercase();
    image_lower.len() > dir_lower.len()
        && image_lower.starts_with(dir_lower)
        && matches!(image_lower.as_bytes()[dir_lower.len()], b'\\' | b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_threshold() {
        let now = SystemTime::now();
        assert!(!should_reap(now, now));
        assert!(!should_reap(now - Duration::from_secs(3600), now));
        assert!(should_reap(now - MAX_AGE - Duration::from_secs(1), now));
        // created 在未来（时钟回拨）不删
        assert!(!should_reap(now + Duration::from_secs(3600), now));
    }

    #[test]
    fn path_prefix_matching() {
        let dir = std::path::PathBuf::from(r"C:\Temp\kachina-sfx-abc");
        assert!(path_within(&dir, r"c:\temp\kachina-sfx-abc\app.exe"));
        assert!(path_within(&dir, r"C:\TEMP\kachina-sfx-abc\sub\tool.exe"));
        assert!(!path_within(&dir, r"C:\Temp\kachina-sfx-abc2\app.exe"));
        assert!(!path_within(&dir, r"C:\Temp\other\app.exe"));
    }
}
