use anyhow::Context;
use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
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

async fn search_pattern_for_extract(file: &AsyncMmapFile) -> anyhow::Result<Vec<usize>> {
    let pattern = "!in\0".to_ascii_uppercase();
    let pattern: &[u8; 4] = pattern.as_bytes().try_into().unwrap();
    let mut reader = file.reader(0).context("MMAP_ERR")?;
    let mut buffer = [0u8; 4096];
    let mut offset: usize = 0;
    let mut founds = Vec::new();
    let mut read = 0;

    loop {
        // move last 4 bytes to the beginning of the buffer
        if read > 4 {
            buffer[0] = buffer[read + 4 - 4];
            buffer[1] = buffer[read + 4 - 3];
            buffer[2] = buffer[read + 4 - 2];
            buffer[3] = buffer[read + 4 - 1];
        }
        read = reader.read(&mut buffer[4..]).await.context("MMAP_ERR")?;
        if read == 0 {
            break;
        }
        for i in 0..read + 4 - 1 {
            if buffer[i] == pattern[0] {
                if i + 3 > read + 3 {
                    continue;
                }
                if buffer[i + 1] == pattern[1]
                    && buffer[i + 2] == pattern[2]
                    && buffer[i + 3] == pattern[3]
                {
                    founds.push(offset + i - 4);
                }
            }
        }
        offset += read;
    }

    Ok(founds)
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Embedded {
    pub name: String,
    pub offset: usize,
    pub raw_offset: usize,
    pub size: usize,
}
pub fn preferred_file_hash<'a>(
    md5: &'a Option<String>,
    xxh: &'a Option<String>,
) -> Option<&'a String> {
    md5.as_ref().or(xxh.as_ref())
}

#[path = "../embedded_name.rs"]
mod embedded_name;
#[cfg(test)]
pub use embedded_name::INDEX_NAME_MAX;
pub use embedded_name::{check_index_name, check_tlv_name, is_embedded_name, EmbeddedNameError};

pub async fn get_embedded(file: &AsyncMmapFile) -> anyhow::Result<Vec<Embedded>> {
    let offsets = search_pattern_for_extract(file).await?;
    let mut entries = Vec::new();
    let mut last_offset: usize = 0;
    let file_len = file.len();
    for offset in offsets.iter() {
        if *offset < last_offset {
            // in case of content includes header
            continue;
        }
        // TLV
        // header: !IN\0
        // name length: 2 bytes big endian
        // name: variable length
        // content length: 4 bytes big endian
        // content: variable length
        let mem_pos_name_length = *offset + 4;
        if mem_pos_name_length + 2 > file_len {
            continue;
        }
        let name_length =
            u16::from_be_bytes(file.slice(mem_pos_name_length, 2).try_into().unwrap()) as usize;
        if name_length == 0 {
            continue;
        }
        let mem_pos_name = mem_pos_name_length + 2;
        let mem_pos_content_length = mem_pos_name + name_length;
        if mem_pos_content_length + 4 > file_len {
            continue;
        }
        let name_bytes = file.slice(mem_pos_name, name_length);
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            continue;
        };
        if !is_embedded_name(name) {
            continue;
        }
        let content_length =
            u32::from_be_bytes(file.slice(mem_pos_content_length, 4).try_into().unwrap()) as usize;
        let mem_pos_content = mem_pos_content_length + 4;
        if mem_pos_content.saturating_add(content_length) > file_len {
            continue;
        }
        entries.push(Embedded {
            name: name.to_string(),
            offset: mem_pos_content,
            size: content_length,
            raw_offset: *offset,
        });
        last_offset = mem_pos_content + content_length;
    }
    Ok(entries)
}

/// DOS `e_lfanew` is a 32-bit offset to `PE\0\0`. Real linkers keep it in this window;
/// a random `MZ\x90\x00` in `.rdata` / zstd / ico almost never does.
const PE_LFANEW_MIN: usize = 0x40;
const PE_LFANEW_MAX: usize = 0x1000;

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

fn is_pe_at(bytes: &[u8], offset: usize) -> bool {
    if offset.saturating_add(0x40) > bytes.len() {
        return false;
    }
    if bytes[offset] != b'M' || bytes[offset + 1] != b'Z' {
        return false;
    }
    let e_lfanew =
        u32::from_le_bytes(bytes[offset + 0x3C..offset + 0x40].try_into().unwrap()) as usize;
    if !(PE_LFANEW_MIN..=PE_LFANEW_MAX).contains(&e_lfanew) {
        return false;
    }
    let pe = offset.saturating_add(e_lfanew);
    bytes.get(pe..pe + 4) == Some(&b"PE\0\0"[..])
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
    use super::{is_pe_at, pe_image_starts};

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
    fn pe_at_requires_pe_signature() {
        let pe = mini_pe(1);
        assert!(is_pe_at(&pe, 0));
        let mut false_mz = vec![0x4D, 0x5A, 0x90, 0x00];
        false_mz.extend_from_slice(&[0u8; 60]);
        assert!(!is_pe_at(&false_mz, 0));
    }

    #[test]
    fn bundle_uses_last_real_pe_not_last_mz90() {
        let builder = mini_pe(1);
        let installer = mini_pe(2);
        let mut bundle = builder.clone();
        bundle.extend_from_slice(&installer);
        // 安装器体内再放一个 DOS 魔数：旧扫描会把它当成映像起点，rcedit 加载失败。
        let planted = builder.len() + 0x40;
        bundle[planted..planted + 4].copy_from_slice(&[0x4D, 0x5A, 0x90, 0x00]);
        assert_eq!(pe_image_starts(&bundle), vec![0, builder.len()]);
    }
}
