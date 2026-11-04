use std::sync::OnceLock;

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::pack_format::{parse_index, read_json, scan_embedded, Embedded};
use crate::utils::error::return_anyhow_result;
use crate::utils::metadata::RepoMetadata;

static MMAP_SELF: OnceCell<AsyncMmapFile> = OnceCell::const_new();
static EMBEDDED_FILES: OnceLock<Vec<Embedded>> = OnceLock::new();
static PARSED_PACK: OnceCell<ParsedPack> = OnceCell::const_new();

#[derive(Clone)]
struct ParsedPack {
    config: Option<Value>,
    metadata: Option<RepoMetadata>,
    index: Option<Vec<Embedded>>,
    image: Option<String>,
}

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

pub fn get_embedded(file: &'static AsyncMmapFile) -> Vec<Embedded> {
    EMBEDDED_FILES
        .get_or_init(|| scan_embedded(file.as_slice()))
        .clone()
}

pub async fn get_config_from_embedded(
    embedded: &[Embedded],
) -> anyhow::Result<(
    Option<Value>,
    Option<RepoMetadata>,
    Option<Vec<Embedded>>,
    Option<String>,
)> {
    let parsed = PARSED_PACK
        .get_or_try_init(|| {
            let embedded = embedded.to_vec();
            async move { parse_embedded(&embedded).await }
        })
        .await?;
    Ok((
        parsed.config.clone(),
        parsed.metadata.clone(),
        parsed.index.clone(),
        parsed.image.clone(),
    ))
}

async fn parse_embedded(embedded: &[Embedded]) -> anyhow::Result<ParsedPack> {
    let bytes = mmap().await.as_slice();
    let config = read_json(bytes, embedded, "\0CONFIG").context("LOCAL_CONFIG_ERR")?;
    let metadata = read_json(bytes, embedded, "\0META").context("LOCAL_CONFIG_ERR")?;
    let index = match embedded.iter().find(|e| e.name == "\0INDEX") {
        Some(entry) => {
            // 索引偏移相对首个 TLV 头
            let start = embedded[0].raw_offset;
            let content = &bytes[entry.offset..entry.offset + entry.size];
            match parse_index(content, start) {
                Some(index) => Some(index),
                None => {
                    return return_anyhow_result(
                        "Malformed packed files: truncated index".to_string(),
                        "LOCAL_PACK_ERR",
                    )
                }
            }
        }
        None => None,
    };
    let image = embedded
        .iter()
        .find(|e| e.name == "\0IMAGE")
        .map(|e| BASE64.encode(&bytes[e.offset..e.offset + e.size]));
    Ok(ParsedPack {
        config,
        metadata,
        index,
        image,
    })
}

pub async fn get_base_with_config() -> anyhow::Result<AsyncMmapFileReader<'static>> {
    let file = mmap().await;
    let embedded = get_embedded(file);
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
