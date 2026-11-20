use std::{
    iter::once,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetLongPathNameW};

fn expand_8dot3(path: &Path) -> Option<String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    let len = unsafe { GetLongPathNameW(PCWSTR(wide.as_ptr()), None) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let written = unsafe { GetLongPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) };
    // The name behind a short alias can change between the two queries; a
    // return >= len means the buffer was too small, not chars written.
    if written == 0 || written >= len {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..written as usize]))
}

/// Expand any 8.3 short-name components in `path` to their long names, e.g.
/// `C:\Users\ADMINI~1` -> `C:\Users\Administrator`. The tail of the path does
/// not need to exist: the longest existing prefix is expanded and the
/// remaining components are re-appended verbatim. Junctions and symlinks are
/// not resolved. Returns the input unchanged when nothing can be expanded.
pub fn long_path(path: &str) -> String {
    let path = path.replace('/', "\\");
    let mut cursor = PathBuf::from(&path);
    let mut tail: Vec<String> = Vec::new();
    loop {
        if let Some(expanded) = expand_8dot3(&cursor) {
            // PathBuf::push keeps a root like `C:\` from gaining a second
            // separator — the staging-root hash does not collapse `\\`.
            let mut out = PathBuf::from(expanded);
            for seg in tail.iter().rev() {
                out.push(seg);
            }
            return out.to_string_lossy().to_string();
        }
        match cursor.file_name() {
            Some(name) => {
                tail.push(name.to_string_lossy().to_string());
                if !cursor.pop() {
                    return path;
                }
            }
            None => return path,
        }
    }
}
