use std::collections::BTreeMap;

use serde::Serialize;
use twox_hash::XxHash3_128;

use crate::{
    cli::SplitArgs,
    pack_format::{parse_index, parse_pack_layout, scan_embedded, LAYOUT_SEARCH_LEN},
};

#[derive(Serialize)]
struct Extent {
    offset: u64,
    len: u64,
    phys_offset: u64,
    path: String,
}

#[derive(Serialize)]
struct IndexEntry {
    name: String,
    offset: u64,
    raw_offset: u64,
    size: u64,
}

/// `kachina-builder split`：把最终成品 exe 拆成 `cas/{xxh}` 不可变块 +
/// `{ver}.json`。json 与 dfs2 `/api` 响应的 `data` 同构：
/// `{index, metadata, installer_end, size, extents}`——调度器只消费
/// `size`/`extents` 做逻辑→物理映射，`index`/`metadata`/`installer_end`
/// 原样透传给安装器。
///
/// cas 块三类：头区 `0..data_end`（含全部 TLV 头）、每个
/// `data_split_hint` 保留段（带 TLV 头的连续字节）、窗口外每个文件的
/// payload（剥去 TLV 头）。
pub async fn split_cli(args: SplitArgs) -> Result<(), String> {
    let bytes = tokio::fs::read(&args.input)
        .await
        .map_err(|e| format!("Failed to read {:?}: {e}", args.input))?;
    let head_len = bytes.len().min(LAYOUT_SEARCH_LEN);
    let layout = parse_pack_layout(&bytes[..head_len])
        .ok_or_else(|| format!("{:?} has no pack layout header", args.input))?;
    let index_start = layout.base_end as u64;
    let data_end = index_start
        + layout.config_len as u64
        + layout.theme_len as u64
        + layout.index_len as u64
        + layout.manifest_len as u64;
    let installer_end = index_start + layout.config_len as u64 + layout.theme_len as u64;
    // 截断或损坏的包：布局头声明的长度越界时响亮报错，不 panic
    if data_end as usize > bytes.len() || index_start > data_end {
        return Err(format!(
            "{:?} is truncated: layout declares data end {data_end}, file is {} bytes",
            args.input,
            bytes.len()
        ));
    }

    let entries = scan_embedded(&bytes);
    let meta = entries
        .iter()
        .find(|e| e.name == "\0META")
        .ok_or_else(|| "package has no \\0META".to_string())?;
    let metadata: serde_json::Value = serde_json::from_slice(
        &bytes[meta.offset..meta.offset + meta.size],
    )
    .map_err(|e| format!("Failed to parse \\0META: {e}"))?;

    // data_split_hint：None = 全数据区可合并（单窗口）；Some([]) = 全独立。
    // 数据区为空（无文件只有头）时不产窗口，否则切出 0 字节 cas 块。
    let hint: Vec<(u64, u64)> = match metadata.get("data_split_hint") {
        None | Some(serde_json::Value::Null) => {
            if bytes.len() as u64 == data_end {
                vec![]
            } else {
                vec![(0, bytes.len() as u64 - data_end)]
            }
        }
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("Bad data_split_hint: {e}"))?,
    };
    let mut windows: Vec<(u64, u64)> = Vec::new();
    for (s, e) in &hint {
        let (Some(w0), Some(w1)) = (data_end.checked_add(*s), data_end.checked_add(*e)) else {
            return Err(format!(
                "{:?} has overflowing data_split_hint window [{s}, {e})",
                args.input
            ));
        };
        windows.push((w0, w1));
    }
    for (w0, w1) in &windows {
        if w0 >= w1 || *w1 > bytes.len() as u64 {
            return Err(format!(
                "{:?} has invalid data_split_hint window [{w0}, {w1}) for a {}-byte file",
                args.input,
                bytes.len()
            ));
        }
    }

    // 数据区内不被任何窗口覆盖的文件单独成块（payload only）
    let files: Vec<&crate::pack_format::Embedded> = entries
        .iter()
        .filter(|e| !e.name.starts_with('\0') && e.raw_offset as u64 >= data_end)
        .collect();
    let in_window = |e: &&crate::pack_format::Embedded| {
        windows
            .iter()
            .any(|(w0, w1)| e.raw_offset as u64 >= *w0 && (e.offset + e.size) as u64 <= *w1)
    };

    // dfs2 形态的 index：物理块名 -> {name, offset, raw_offset, size}，
    // 与包内 \0INDEX 1:1（同 dfs2 服务端与 apply_packed_index 的读法）。
    // 提前解析并校验边界：数据区中后段截断时 scan_embedded 丢块，
    // 若照写索引就会静默产出引用不存在数据块的残缺拆包产物。
    let index_entry = entries
        .iter()
        .find(|e| e.name == "\0INDEX")
        .ok_or_else(|| "package has no \\0INDEX".to_string())?;
    let parsed = parse_index(
        &bytes[index_entry.offset..index_entry.offset + index_entry.size],
        index_start as usize,
    )
    .ok_or_else(|| "malformed \\0INDEX".to_string())?;
    for entry in &parsed {
        let bounded = entry
            .offset
            .checked_add(entry.size)
            .is_some_and(|end| end <= bytes.len());
        if !bounded {
            return Err(format!(
                "{:?} is truncated: index entry {:?} ends at {:?}, file is {} bytes",
                args.input,
                entry.name,
                entry.offset.checked_add(entry.size),
                bytes.len()
            ));
        }
    }

    let out_dir = &args.output;
    let cas_dir = out_dir.join("cas");
    tokio::fs::create_dir_all(&cas_dir)
        .await
        .map_err(|e| format!("Failed to create {:?}: {e}", cas_dir))?;

    let mut written = std::collections::HashSet::new();
    let mut extents = Vec::new();
    let mut emit = |offset: u64, block: &[u8]| -> Result<(), String> {
        let mut hasher = XxHash3_128::new();
        hasher.write(block);
        let name = format!("{:032x}", hasher.finish_128());
        if written.insert(name.clone()) {
            let p = cas_dir.join(&name);
            std::fs::write(&p, block).map_err(|e| format!("Failed to write {:?}: {e}", p))?;
        }
        extents.push(Extent {
            offset,
            len: block.len() as u64,
            phys_offset: 0,
            path: format!("cas/{name}"),
        });
        Ok(())
    };

    emit(0, &bytes[..data_end as usize])?;
    for (w0, w1) in &windows {
        emit(*w0, &bytes[*w0 as usize..*w1 as usize])?;
    }
    for file in &files {
        if in_window(file) {
            continue;
        }
        emit(
            file.offset as u64,
            &bytes[file.offset..file.offset + file.size],
        )?;
    }
    extents.sort_by_key(|e| e.offset);

    let index: BTreeMap<String, IndexEntry> = parsed
        .into_iter()
        .map(|e| {
            (
                e.name.clone(),
                IndexEntry {
                    name: e.name,
                    offset: e.offset as u64,
                    raw_offset: e.raw_offset as u64,
                    size: e.size as u64,
                },
            )
        })
        .collect();

    let doc = serde_json::json!({
        "index": index,
        "metadata": metadata,
        "installer_end": installer_end,
        "size": bytes.len() as u64,
        "extents": extents,
    });
    let name = args.name.clone().unwrap_or_else(|| {
        args.input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "pack".to_string())
    });
    let out_path = out_dir.join(format!("{name}.json"));
    std::fs::write(&out_path, serde_json::to_string_pretty(&doc).unwrap())
        .map_err(|e| format!("Failed to write {:?}: {e}", out_path))?;
    println!(
        "Split {:?}: {} cas blocks, {} extents -> {:?}",
        args.input,
        written.len(),
        doc["extents"].as_array().map(|v| v.len()).unwrap_or(0),
        out_path
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::SplitArgs;

    fn tlv(name: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = b"!IN\0".to_vec();
        out.extend((name.len() as u16).to_be_bytes());
        out.extend(name);
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }

    /// 最小合法包：布局头 + \0CONFIG + \0INDEX(空) + \0META，数据区为空。
    /// `manifest_len_lie` 往布局头声明的 manifest 长度里注水模拟截断/损坏。
    fn build_pack(meta_body: &[u8], manifest_len_lie: u32) -> Vec<u8> {
        let config = tlv(b"\0CONFIG", br#"{"appName":"Demo"}"#);
        let index = tlv(b"\0INDEX", &[]);
        let meta = tlv(b"\0META", meta_body);
        let base_end = 200u32;
        let mut bytes = vec![0u8; base_end as usize];
        let mut header = crate::pack_format::LAYOUT_MARKER.to_vec();
        for v in [
            base_end,
            config.len() as u32,
            0,
            index.len() as u32,
            meta.len() as u32 + manifest_len_lie,
        ] {
            header.extend(v.to_be_bytes());
        }
        bytes[..header.len()].copy_from_slice(&header);
        bytes.extend(config);
        bytes.extend(index);
        bytes.extend(meta);
        bytes
    }

    async fn run_split(bytes: &[u8], dir: &std::path::Path) -> Result<(), String> {
        let input = dir.join("pack.exe");
        let output = dir.join("out");
        std::fs::write(&input, bytes).unwrap();
        split_cli(SplitArgs {
            input,
            output,
            name: None,
        })
        .await
    }

    #[tokio::test]
    async fn empty_data_region_yields_no_zero_length_cas_block() {
        let dir = std::env::temp_dir().join(format!("kachina-split-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = build_pack(br#"{"tag_name":"v1","hashed":[]}"#, 0);
        run_split(&pack, &dir).await.unwrap();
        let cas = dir.join("out").join("cas");
        let blocks: Vec<_> = std::fs::read_dir(&cas).unwrap().collect();
        // 空数据区只有头区一个块，没有 0 字节块
        assert_eq!(blocks.len(), 1);
        for entry in blocks {
            assert!(entry.unwrap().metadata().unwrap().len() > 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn truncated_pack_errors_instead_of_panicking() {
        let dir = std::env::temp_dir().join(format!("kachina-split-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = build_pack(br#"{"tag_name":"v1","hashed":[]}"#, 100000);
        let err = run_split(&pack, &dir).await.unwrap_err();
        assert!(err.contains("truncated"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn out_of_bounds_hint_window_errors_instead_of_panicking() {
        let dir = std::env::temp_dir().join(format!("kachina-split-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = build_pack(br#"{"tag_name":"v1","hashed":[],"data_split_hint":[[0,99999999]]}"#, 0);
        let err = run_split(&pack, &dir).await.unwrap_err();
        assert!(err.contains("invalid data_split_hint"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// 索引声明的数据块超出文件末尾：数据区截断必须报错而非照写索引。
    #[tokio::test]
    async fn index_entry_beyond_file_end_errors() {
        let dir = std::env::temp_dir().join(format!("kachina-split-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = tlv(b" CONFIG", br#"{"appName":"Demo"}"#);
        let mut index_body = Vec::new();
        let name = b"abcdef";
        index_body.push(name.len() as u8);
        index_body.extend(name);
        index_body.extend(100u32.to_be_bytes()); // size
        index_body.extend(999999u32.to_be_bytes()); // rel，远超文件长度
        let index = tlv(b" INDEX", &index_body);
        let meta = tlv(b" META", br#"{"tag_name":"v1","hashed":[]}"#);
        let base_end = 200u32;
        let mut bytes = vec![0u8; base_end as usize];
        let mut header = crate::pack_format::LAYOUT_MARKER.to_vec();
        for v in [
            base_end,
            config.len() as u32,
            0,
            index.len() as u32,
            meta.len() as u32,
        ] {
            header.extend(v.to_be_bytes());
        }
        bytes[..header.len()].copy_from_slice(&header);
        bytes.extend(config);
        bytes.extend(index);
        bytes.extend(meta);
        let input = dir.join("pack.exe");
        let output = dir.join("out");
        std::fs::write(&input, &bytes).unwrap();
        let err = split_cli(SplitArgs { input, output, name: None })
            .await
            .unwrap_err();
        assert!(err.contains("truncated"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

