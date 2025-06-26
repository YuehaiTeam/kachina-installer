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
                    exe_path
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
pub async fn get_embedded(file: &AsyncMmapFile) -> anyhow::Result<Vec<Embedded>> {
    let offsets = search_pattern_for_extract(file).await?;
    let mut entries = Vec::new();
    let mut last_offset: usize = 0;
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
        let mem_pos_name = mem_pos_name_length + 2;
        let name_length =
            u16::from_be_bytes(file.slice(mem_pos_name_length, 2).try_into().unwrap()) as usize;
        let name = file.slice(mem_pos_name, name_length);
        let name = String::from_utf8_lossy(name).to_string();
        let mem_pos_content_length = mem_pos_name + name_length;
        let content_length =
            u32::from_be_bytes(file.slice(mem_pos_content_length, 4).try_into().unwrap()) as usize;
        let mem_pos_content = mem_pos_content_length + 4;
        entries.push(Embedded {
            name,
            offset: mem_pos_content,
            size: content_length,
            raw_offset: *offset,
        });
        last_offset = mem_pos_content + content_length;
    }
    Ok(entries)
}

async fn search_pattern(file: &AsyncMmapFile) -> Result<Vec<usize>, String> {
    let pattern: [u8; 4] = [0x4D, 0x5A, 0x90, 0x00]; // exe header
    let mut reader = file.reader(0).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 4096];
    let mut offset: usize = 0;
    let mut previous_bytes = Vec::new();
    let mut founds = Vec::new();

    loop {
        let bytes_read = reader.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if bytes_read == 0 {
            break;
        }

        // Step 1: Check across previous_bytes and buffer
        if !previous_bytes.is_empty() {
            let pb_len = previous_bytes.len();
            let needed = 4 - pb_len;
            if bytes_read >= needed {
                let combined: Vec<u8> = previous_bytes
                    .iter()
                    .cloned()
                    .chain(buffer[..needed].iter().cloned())
                    .collect();
                if combined == pattern[..] {
                    founds.push(offset - (pb_len));
                }
            }
        }

        // Step 2: Check within the buffer
        for i in 0..bytes_read - 3 {
            if buffer[i..i + 4] == pattern {
                founds.push(offset + i);
            }
        }

        // Step 3: Update previous_bytes
        if bytes_read > 0 {
            let start = bytes_read - std::cmp::min(3, bytes_read);
            previous_bytes = buffer[start..bytes_read].to_vec();
        }

        offset += bytes_read as usize;
    }

    Ok(founds)
}

pub async fn get_reader_for_bundle() -> Result<AsyncMmapFileReader<'static>, String> {
    let file = mmap().await;
    let headers = search_pattern(file).await.map_err(|e| e.to_string())?;
    if headers.len() < 3 {
        return Err("Failed to find packed exe".to_string());
    }
    let exe_offset = headers[2];
    let reader = file.reader(exe_offset).map_err(|e| e.to_string())?;
    Ok(reader)
}
