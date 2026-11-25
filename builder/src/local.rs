use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use tokio::sync::OnceCell;

use crate::pack_format::is_pe_at;

static MMAP_SELF: OnceCell<AsyncMmapFile> = OnceCell::const_new();

pub async fn mmap() -> &'static AsyncMmapFile {
    MMAP_SELF
        .get_or_init(|| async {
            let exe_path = {
                #[cfg(debug_assertions)]
                {
                    // use last release build
                    let exe_path = std::env::current_exe().unwrap();
                    // ../release/${basename}
                    let exe_path = exe_path
                        .parent()
                        .ok_or("Failed to get parent dir".to_string())
                        .unwrap()
                        .parent()
                        .ok_or("Failed to get parent dir".to_string())
                        .unwrap()
                        .join("release")
                        .join("kachina-builder-bundle.exe");
                    let debug_path = exe_path
                        .parent()
                        .ok_or("Failed to get parent dir".to_string())
                        .unwrap()
                        .parent()
                        .ok_or("Failed to get parent dir".to_string())
                        .unwrap()
                        .join("debug")
                        .join("kachina-builder-bundle.exe");
                    if exe_path.exists() {
                        exe_path
                    } else if debug_path.exists() {
                        debug_path
                    } else {
                        // fallback to current exe
                        std::env::current_exe().unwrap()
                    }
                }
                #[cfg(not(debug_assertions))]
                {
                    std::env::current_exe().unwrap()
                }
            };
            AsyncMmapFile::open(exe_path).await.unwrap()
        })
        .await
}

/// Bundle file is `kachina-builder` bytes followed by `kachina-installer` bytes.
/// Only a real PE image start counts; the last one is the appended installer.
pub fn pe_image_starts(bytes: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        if bytes[i] == b'M' && bytes[i + 1] == b'Z' && is_pe_at(bytes, i) {
            found.push(i);
        }
        i += 1;
    }
    found
}

pub async fn get_reader_for_bundle() -> Result<AsyncMmapFileReader<'static>, String> {
    let file = mmap().await;
    let bytes = file.slice(0, file.len());
    let headers = pe_image_starts(bytes);
    if headers.len() < 2 {
        println!("Found PE images: {headers:?}");
        return Err("Failed to find packed exe: ".to_string());
    }
    let exe_offset = headers[headers.len() - 1];
    let reader = file.reader(exe_offset).map_err(|e| e.to_string())?;
    Ok(reader)
}

#[cfg(test)]
mod tests {
    use super::pe_image_starts;

    fn mini_pe(tag: u8) -> Vec<u8> {
        let e_lfanew = 0x80usize;
        let mut bytes = vec![0u8; 0x200];
        bytes[0] = b'M';
        bytes[1] = b'Z';
        bytes[2] = 0x90;
        bytes[3] = 0x00;
        bytes[0x3C..0x40].copy_from_slice(&(e_lfanew as u32).to_le_bytes());
        bytes[e_lfanew..e_lfanew + 4].copy_from_slice(b"PE\0\0");
        bytes[e_lfanew + 4] = tag;
        bytes
    }

    #[test]
    fn bundle_uses_last_real_pe_not_last_mz90() {
        let builder = mini_pe(1);
        let installer = mini_pe(2);
        let mut bundle = builder.clone();
        bundle.extend_from_slice(&installer);
        // 安装器体内再放一个 DOS 魔数：只认 `MZ\x90\x00` 的扫描会把它当成映像起点，rcedit 加载失败。
        let planted = builder.len() + 0x40;
        bundle[planted..planted + 4].copy_from_slice(&[0x4D, 0x5A, 0x90, 0x00]);
        assert_eq!(pe_image_starts(&bundle), vec![0, builder.len()]);
    }
}
