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

    let entries = scan_embedded(&bytes);
    let meta = entries
        .iter()
        .find(|e| e.name == "\0META")
        .ok_or_else(|| "package has no \\0META".to_string())?;
    let metadata: serde_json::Value = serde_json::from_slice(
        &bytes[meta.offset..meta.offset + meta.size],
    )
    .map_err(|e| format!("Failed to parse \\0META: {e}"))?;

    // data_split_hint：None = 全数据区可合并（单窗口）；Some([]) = 全独立
    let hint: Vec<(u64, u64)> = match metadata.get("data_split_hint") {
        None | Some(serde_json::Value::Null) => {
            vec![(0, bytes.len() as u64 - data_end)]
        }
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("Bad data_split_hint: {e}"))?,
    };
    let windows: Vec<(u64, u64)> = hint
        .iter()
        .map(|(s, e)| (data_end + s, data_end + e))
        .collect();

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

    // dfs2 形态的 index：物理块名 -> {name, offset, raw_offset, size}，
    // 与包内 \0INDEX 1:1（同 dfs2 服务端与 apply_packed_index 的读法）。
    let index_entry = entries
        .iter()
        .find(|e| e.name == "\0INDEX")
        .ok_or_else(|| "package has no \\0INDEX".to_string())?;
    let parsed = parse_index(
        &bytes[index_entry.offset..index_entry.offset + index_entry.size],
        index_start as usize,
    )
    .ok_or_else(|| "malformed \\0INDEX".to_string())?;
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
