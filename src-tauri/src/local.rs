use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
static MMAP_SELF: OnceCell<AsyncMmapFile> = OnceCell::const_new();

pub async fn mmap() -> &'static AsyncMmapFile {
    MMAP_SELF
        .get_or_init(|| async {
            let exe_path = if cfg!(debug_assertions) {
                std::env::current_exe().unwrap().with_extension("bin")
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

async fn search_pattern() -> Result<Vec<usize>, String> {
    let pattern = "!in\0".to_ascii_uppercase();
    let pattern: &[u8; 4] = pattern.as_bytes().try_into().unwrap();
    let file = mmap().await;
    let mut reader = file.reader(0).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 4096];
    let mut offset: usize = 0;
    let mut founds = Vec::new();
    let mut read = 0;

    loop {
        // move last 4 bytes to the beginning of the buffer
        if read > 4 {
            buffer[0] = buffer[read - 4];
            buffer[1] = buffer[read - 3];
            buffer[2] = buffer[read - 2];
            buffer[3] = buffer[read - 1];
        }
        read = reader
            .read(&mut buffer[4..])
            .await
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        for i in 0..read + 4 {
            if buffer[i] == pattern[0] {
                if i + 4 > read {
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
pub async fn get_embedded() -> Result<Vec<Embedded>, String> {
    let offsets = search_pattern().await?;
    let mut entries = Vec::new();
    let file = mmap().await;
    let mut last_offset: usize = 0;
    for offset in offsets.iter() {
        println!("Offset: {}, Last Offset: {}", offset, last_offset);
        if *offset < last_offset {
            // in case of content includes header
            println!("Skipping offset: {} {}", offset, last_offset);
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
) -> Result<(Option<Value>, Option<Value>, Option<Vec<Embedded>>), String> {
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
            config = Some(serde_json::from_str(&content).map_err(|e| e.to_string())?);
        } else if entry.name == "\0META" {
            let content = file.slice(entry.offset, entry.size);
            let content = String::from_utf8_lossy(content);
            metadata = Some(serde_json::from_str(&content).map_err(|e| e.to_string())?);
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
    Ok((config, metadata, index))
}

pub fn get_header_size(name: &str) -> usize {
    "!in\0".len() + 2 + name.len() + 4
}

pub async fn get_base_with_config() -> Result<AsyncMmapFileReader<'static>, String> {
    let embedded = get_embedded().await?;
    let config_index = embedded.iter().position(|x| x.name == "\0CONFIG");
    let image_index = embedded.iter().position(|x| x.name == "\0IMAGE");
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
