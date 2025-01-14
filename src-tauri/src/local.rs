use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
static MMAP_SELF: OnceCell<AsyncMmapFile> = OnceCell::const_new();

pub async fn mmap() -> &'static AsyncMmapFile {
    MMAP_SELF
        .get_or_init(|| async {
            let exe_path = std::env::current_exe().map_err(|e| e.to_string()).unwrap();
            AsyncMmapFile::open(exe_path)
                .await
                .map_err(|e| e.to_string())
                .unwrap()
        })
        .await
}

async fn search_pattern() -> Result<Vec<usize>, String> {
    let pattern = "!ins".to_ascii_uppercase();
    let pattern: &[u8; 4] = pattern.as_bytes().try_into().unwrap();
    let file = mmap().await;
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
            if buffer[i..i + 4] == *pattern {
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
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Embedded {
    pub name: String,
    pub offset: usize,
    pub raw_offset: usize,
    pub size: usize,
}
pub async fn get_embedded() -> Result<Vec<Embedded>, String> {
    let offsets = search_pattern().await?;
    let mut entries = Vec::new();
    let file = mmap().await;
    let mut last_offset: usize = 0;
    for offset in offsets.iter() {
        if *offset < last_offset {
            // in case of content includes header
            continue;
        }
        // TLV
        // header: !INS
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
) -> Result<(Option<Value>, Option<Value>), String> {
    let file = mmap().await;
    let mut config = None;
    let mut metadata = None;
    for entry in embedded.iter() {
        if entry.name == ".config.json" {
            let content = file.slice(entry.offset, entry.size);
            let content = String::from_utf8_lossy(content);
            config = Some(serde_json::from_str(&content).map_err(|e| e.to_string())?);
        } else if entry.name == ".metadata.json" {
            let content = file.slice(entry.offset, entry.size);
            let content = String::from_utf8_lossy(content);
            metadata = Some(serde_json::from_str(&content).map_err(|e| e.to_string())?);
        }
    }
    Ok((config, metadata))
}

pub async fn get_base_with_config() -> Result<AsyncMmapFileReader<'static>, String> {
    let embedded = get_embedded().await?;
    let config_index = embedded.iter().position(|x| x.name == ".config.json");
    let image_index = embedded.iter().position(|x| x.name == ".image");
    if config_index.is_none() {
        if embedded.is_empty() {
            return mmap().await.reader(0).map_err(|e| e.to_string());
        }
        return Err("Malformed packed files: missing config".to_string());
    }
    let config_index = config_index.unwrap();
    // config index should be 0
    if config_index != 0 {
        return Err("Malformed packed files: config not at index 0".to_string());
    }
    let mut end_pos = embedded[config_index].offset + embedded[config_index].size;
    if let Some(image_index) = image_index {
        // image index should be 1
        if image_index != 1 {
            return Err("Malformed packed files: image not at index 1".to_string());
        }
        end_pos = embedded[image_index].offset + embedded[image_index].size;
    }
    mmap()
        .await
        .range_reader(0, end_pos)
        .map_err(|e| e.to_string())
}
