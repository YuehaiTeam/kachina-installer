//! 自解压包的读取侧：TLV 扫描与 \0CONFIG/\0META 解析。
//! 与 builder/local.rs 同为安装器 local.rs 的最小拷贝（仓库内第三份）；
//! 不含 INDEX 解析——TLV 扫描已给出全部内嵌项（含数据块）的绝对偏移。

use anyhow::{Context, Result};
use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

#[path = "../embedded_name.rs"]
#[allow(dead_code)]
mod embedded_name;
use embedded_name::is_embedded_name;

use crate::metadata::RepoMetadata;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Embedded {
    pub name: String,
    /// 内容起点（绝对文件偏移）
    pub offset: usize,
    /// TLV 头起点（绝对文件偏移）
    pub raw_offset: usize,
    pub size: usize,
}

async fn search_pattern(file: &AsyncMmapFile) -> Result<Vec<usize>> {
    let pattern: &[u8; 4] = b"!IN\0";
    let mut reader = file.reader(0).context("MMAP_ERR")?;
    let mut buffer = [0u8; 4096];
    let mut offset: usize = 0;
    let mut founds = Vec::new();
    let mut read = 0;

    loop {
        // 把上一块的尾部 4 字节挪到开头，覆盖跨块命中；
        // 首轮 buffer[..4] 为 0，不可能误中 '!' 开头，offset + i - 4 不会下溢。
        if read > 4 {
            buffer.copy_within(read..read + 4, 0);
        }
        read = reader.read(&mut buffer[4..]).await.context("MMAP_ERR")?;
        if read == 0 {
            break;
        }
        for i in 0..read + 4 - 1 {
            if buffer[i] == pattern[0]
                && i + 3 <= read + 3
                && buffer[i + 1] == pattern[1]
                && buffer[i + 2] == pattern[2]
                && buffer[i + 3] == pattern[3]
            {
                founds.push(offset + i - 4);
            }
        }
        offset += read;
    }

    Ok(founds)
}

/// 扫描文件中的全部 TLV 内嵌项。与 builder 侧同一算法：跳过内容里的假命中
/// （last_offset），校验名字合法性与长度边界。
pub async fn scan_embedded(file: &AsyncMmapFile) -> Result<Vec<Embedded>> {
    let offsets = search_pattern(file).await?;
    let mut entries = Vec::new();
    let mut last_offset: usize = 0;
    let file_len = file.len();
    for offset in offsets.iter() {
        if *offset < last_offset {
            // 内容里恰好含魔数的假命中
            continue;
        }
        // TLV 布局: "!IN\0" + u16 名字长 + 名字 + u32 内容长 + 内容（均大端）
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
            raw_offset: *offset,
            size: content_length,
        });
        last_offset = mem_pos_content + content_length;
    }
    Ok(entries)
}

/// 与 builder/pack 的块命名一致：md5 优先于 xxh（实际包中 md5 恒为 None，块名恒为 xxh）。
pub fn preferred_file_hash<'a>(
    md5: &'a Option<String>,
    xxh: &'a Option<String>,
) -> Option<&'a String> {
    md5.as_ref().or(xxh.as_ref())
}

pub fn read_json<T: serde::de::DeserializeOwned>(
    file: &AsyncMmapFile,
    embedded: &[Embedded],
    name: &str,
) -> Result<Option<T>> {
    let Some(entry) = embedded.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let content = file.slice(entry.offset, entry.size);
    let text = String::from_utf8_lossy(content);
    serde_json::from_str(&text)
        .with_context(|| format!("LOCAL_CONFIG_ERR: malformed {name}"))
        .map(Some)
}

pub async fn open_self() -> Result<AsyncMmapFile> {
    let exe = std::env::current_exe().context("CURRENT_EXE_ERR")?;
    // 与安装器一致的调试约定：debug 构建读同目录的 .bin 副本（pack 产物）。
    let path = if cfg!(debug_assertions) {
        let bin = exe.with_extension("bin");
        if bin.exists() {
            bin
        } else {
            exe
        }
    } else {
        exe
    };
    AsyncMmapFile::open(path).await.context("MMAP_ERR")
}

pub fn slice_json_metadata(
    file: &AsyncMmapFile,
    embedded: &[Embedded],
) -> Result<Option<RepoMetadata>> {
    read_json(file, embedded, "\0META")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(name: &str, body: &[u8]) -> Vec<u8> {
        let mut out = b"!IN\0".to_vec();
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    async fn scan_bytes(bytes: &[u8]) -> Vec<Embedded> {
        let dir = std::env::temp_dir().join(format!("kachina-sfx-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pkg.exe");
        std::fs::write(&path, bytes).unwrap();
        let file = AsyncMmapFile::open(&path).await.unwrap();
        let result = scan_embedded(&file).await.unwrap();
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[tokio::test]
    async fn scan_finds_all_tlvs_with_absolute_offsets() {
        let mut bytes = vec![0u8; 100]; // 模拟 PE base
        let config = tlv("\0CONFIG", br#"{"exeName":"app.exe"}"#);
        let meta = tlv("\0META", br#"{"tagName":"1.0.0","hashed":[]}"#);
        let data = tlv("abc123", b"payload");
        let config_at = bytes.len();
        bytes.extend_from_slice(&config);
        let meta_at = bytes.len();
        bytes.extend_from_slice(&meta);
        let data_at = bytes.len();
        bytes.extend_from_slice(&data);

        let found = scan_bytes(&bytes).await;
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].name, "\0CONFIG");
        assert_eq!(found[0].raw_offset, config_at);
        assert_eq!(found[1].raw_offset, meta_at);
        assert_eq!(found[2].name, "abc123");
        assert_eq!(found[2].raw_offset, data_at);
        assert_eq!(found[2].size, 7);
    }

    #[tokio::test]
    async fn scan_skips_magic_inside_content() {
        // 内容里嵌一个伪 TLV 头，必须被 last_offset 跳过
        let mut inner = b"!IN\0".to_vec();
        inner.extend_from_slice(&3u16.to_be_bytes());
        inner.extend_from_slice(b"bad");
        inner.extend_from_slice(&5u32.to_be_bytes());
        inner.extend_from_slice(b"xxxxx");
        let mut bytes = tlv("real1", &inner);
        bytes.extend_from_slice(&tlv("real2", b"ok"));

        let found = scan_bytes(&bytes).await;
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "real1");
        assert_eq!(found[1].name, "real2");
    }

    #[test]
    fn preferred_hash_is_md5_then_xxh() {
        let md5 = Some("m".to_string());
        let xxh = Some("x".to_string());
        assert_eq!(preferred_file_hash(&md5, &xxh).unwrap(), "m");
        assert_eq!(preferred_file_hash(&None, &xxh).unwrap(), "x");
        assert!(preferred_file_hash(&None, &None).is_none());
    }
}
