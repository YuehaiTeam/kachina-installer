use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;

use crate::utils::error::return_anyhow_result;
static MMAP_SELF: OnceCell<AsyncMmapFile> = OnceCell::const_new();

pub async fn mmap() -> &'static AsyncMmapFile {
    MMAP_SELF
        .get_or_init(|| async {
            let exe_path = if cfg!(debug_assertions) {
                let extbin = std::env::current_exe().unwrap().with_extension("bin");
                if extbin.exists() {
                    extbin
                } else {
                    std::env::current_exe().unwrap()
                }
            } else {
                std::env::current_exe().map_err(|e| e.to_string()).unwrap()
            };
            AsyncMmapFile::open(exe_path)
                .await
                .map_err(|e| e.to_string())
                .unwrap()
        })
        .await
}

async fn search_pattern(file: &'static AsyncMmapFile) -> anyhow::Result<Vec<usize>> {
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
pub async fn get_embedded(file: &'static AsyncMmapFile) -> anyhow::Result<Vec<Embedded>> {
    let offsets = search_pattern(file).await?;
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

pub async fn get_config_from_embedded(
    embedded: &[Embedded],
) -> anyhow::Result<(
    Option<Value>,
    Option<Value>,
    Option<Vec<Embedded>>,
    Option<String>,
)> {
    let file = mmap().await;
    let mut config = None;
    let mut metadata = None;
    let mut index: Option<Vec<Embedded>> = None;
    let start_offset = if embedded.is_empty() {
        0
    } else {
        embedded[0].raw_offset
    };
    for entry in embedded.iter() {
        if entry.name == "\0CONFIG" {
            let content = file.slice(entry.offset, entry.size);
            let content = String::from_utf8_lossy(content);
            config = Some(serde_json::from_str(&content).context("LOCAL_CONFIG_ERR")?);
        } else if entry.name == "\0META" {
            let content = file.slice(entry.offset, entry.size);
            let content = String::from_utf8_lossy(content);
            metadata = Some(serde_json::from_str(&content).context("LOCAL_CONFIG_ERR")?);
        } else if entry.name == "\0INDEX" {
            // u8: name_len var: name u32: size u32: offset
            let content: &[u8] = file.slice(entry.offset, entry.size);
            let mut index_entries = Vec::new();
            let mut offset = 0;
            while offset < content.len() {
                let name_len = content[offset] as usize;
                offset += 1;
                let name = String::from_utf8_lossy(&content[offset..offset + name_len]).to_string();
                offset += name_len;
                let size =
                    u32::from_be_bytes(content[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let file_offset =
                    u32::from_be_bytes(content[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                index_entries.push(Embedded {
                    raw_offset: start_offset + file_offset - get_header_size(&name),
                    name,
                    offset: start_offset + file_offset,
                    size,
                });
            }
            index = Some(index_entries);
        }
    }

    // Process \0IMAGE if it exists
    let mut image_base64 = None;
    for entry in embedded.iter() {
        if entry.name == "\0IMAGE" {
            let content: &[u8] = file.slice(entry.offset, entry.size);
            image_base64 = Some(BASE64.encode(content));
            break;
        }
    }

    Ok((config, metadata, index, image_base64))
}

pub fn get_header_size(name: &str) -> usize {
    "!in\0".len() + 2 + name.len() + 4
}

pub async fn get_base_with_config() -> anyhow::Result<AsyncMmapFileReader<'static>> {
    let file = mmap().await;
    let embedded = get_embedded(file).await?;
    let config_index = embedded.iter().position(|x| x.name == "\0CONFIG");
    let image_index = embedded.iter().position(|x| x.name == "\0IMAGE");
    if config_index.is_none() {
        if embedded.is_empty() {
            return mmap().await.reader(0).context("MMAP_ERR");
        }
        return return_anyhow_result(
            "Malformed packed files: missing config".to_string(),
            "LOCAL_PACK_ERR",
        );
    }
    let config_index = config_index.unwrap();
    // config index should be 0
    if config_index != 0 {
        return return_anyhow_result(
            "Malformed packed files: config not at index 0".to_string(),
            "LOCAL_PACK_ERR",
        );
    }
    let mut end_pos = embedded[config_index].offset + embedded[config_index].size;
    if let Some(image_index) = image_index {
        // image index should be 1
        if image_index != 1 {
            return return_anyhow_result(
                "Malformed packed files: image not at index 1".to_string(),
                "LOCAL_PACK_ERR",
            );
        }
        end_pos = embedded[image_index].offset + embedded[image_index].size;
    }
    mmap().await.range_reader(0, end_pos).context("MMAP_ERR")
}
