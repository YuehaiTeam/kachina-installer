//! 安装包格式的读取侧：DOS stub 布局头、TLV 内嵌项、`\0INDEX`、数据块命名与归档相对路径。
//!
//! 安装器、builder 与 sfx 共用这一份实现（builder 与 sfx 经 `#[path]` 引入），因此本文件
//! 只依赖外部 crate，不引用任何 `crate::` 路径。
//!
//! 包布局：PE base，随后依次是 `\0CONFIG`、可选 `\0IMAGE`、可选 `\0INDEX`、可选 `\0META`
//! 与数据块，每项一个 TLV（`!IN\0` + u16 名字长 + 名字 + u32 内容长 + 内容，均大端）。
//! 离线包在 DOS stub 里写布局头，`base_end` 指向首个 TLV；在线包与卸载器/更新器的布局头
//! 为全零。分发侧做 Authenticode 签名时，签名表追加在文件末尾。

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[path = "embedded_name.rs"]
pub mod embedded_name;
use embedded_name::is_embedded_name;

pub const TLV_MAGIC: &[u8; 4] = b"!IN\0";
pub const LAYOUT_MARKER: &[u8] = b"!KachinaInstaller!";
/// 布局头位于 PE 头部的 DOS stub，只在文件开头这一段内查找。
pub const LAYOUT_SEARCH_LEN: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Embedded {
    pub name: String,
    /// 内容起点（绝对文件偏移）
    pub offset: usize,
    /// TLV 头起点（绝对文件偏移）
    pub raw_offset: usize,
    pub size: usize,
}

/// `!KachinaInstaller!` 之后的 5 个大端 u32。`base_end` 是首个 TLV 头的绝对文件偏移，
/// 其余是各段含 TLV 头的长度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackLayout {
    pub base_end: u32,
    pub config_len: u32,
    pub theme_len: u32,
    pub index_len: u32,
    pub manifest_len: u32,
}

pub fn parse_pack_layout(header_region: &[u8]) -> Option<PackLayout> {
    let pos = header_region
        .windows(LAYOUT_MARKER.len())
        .position(|w| w == LAYOUT_MARKER)?;
    let at = pos + LAYOUT_MARKER.len();
    Some(PackLayout {
        base_end: be_u32(header_region, at)?,
        config_len: be_u32(header_region, at + 4)?,
        theme_len: be_u32(header_region, at + 8)?,
        index_len: be_u32(header_region, at + 12)?,
        manifest_len: be_u32(header_region, at + 16)?,
    })
}

/// 把 `head`（文件开头一段）里布局头的 5 个字段抹零：落盘的卸载器、更新器与自更新写出的
/// 安装器只保留 base 与配置，布局头不能再指向包内数据。找不到完整布局头时不改动。
pub fn clear_layout(head: &mut [u8]) -> bool {
    let Some(pos) = head
        .windows(LAYOUT_MARKER.len())
        .position(|w| w == LAYOUT_MARKER)
    else {
        return false;
    };
    let start = pos + LAYOUT_MARKER.len();
    match head.get_mut(start..start + 20) {
        Some(fields) => {
            fields.fill(0);
            true
        }
        None => false,
    }
}

pub fn get_header_size(name: &str) -> usize {
    TLV_MAGIC.len() + 2 + name.len() + 4
}

/// 解析 `pos` 处的 TLV 头：魔数匹配、名字非空且合法、内容不越过文件末尾。
pub fn read_tlv(bytes: &[u8], pos: usize) -> Option<Embedded> {
    if bytes.get(pos..pos.checked_add(4)?)? != TLV_MAGIC {
        return None;
    }
    let name_len = be_u16(bytes, pos + 4)? as usize;
    if name_len == 0 {
        return None;
    }
    let name_at = pos + 6;
    let name = std::str::from_utf8(bytes.get(name_at..name_at + name_len)?).ok()?;
    if !is_embedded_name(name) {
        return None;
    }
    let size = be_u32(bytes, name_at + name_len)? as usize;
    let offset = name_at + name_len + 4;
    if offset.checked_add(size)? > bytes.len() {
        return None;
    }
    Some(Embedded {
        name: name.to_string(),
        offset,
        raw_offset: pos,
        size,
    })
}

/// 解析 `\0INDEX` 内容：每项为 u8 名字长 + 名字 + u32 内容长 + u32 偏移（大端），偏移相对
/// `start`（首个 TLV 头起点）。任一项截断、名字非法或偏移小于 TLV 头长度时返回 `None`。
/// 不检查条目是否越过文件末尾：截断的包由调用方比对索引与实际 TLV 发现。
pub fn parse_index(content: &[u8], start: usize) -> Option<Vec<Embedded>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < content.len() {
        let name_len = content[at] as usize;
        at += 1;
        let name = std::str::from_utf8(content.get(at..at + name_len)?).ok()?;
        if !is_embedded_name(name) {
            return None;
        }
        at += name_len;
        let size = be_u32(content, at)? as usize;
        let rel = be_u32(content, at + 4)? as usize;
        at += 8;
        let raw_rel = rel.checked_sub(get_header_size(name))?;
        out.push(Embedded {
            name: name.to_string(),
            offset: start.checked_add(rel)?,
            raw_offset: start.checked_add(raw_rel)?,
            size,
        });
    }
    Some(out)
}

/// 列出文件中的全部内嵌项（按文件顺序）。
///
/// 先定位首个 TLV：DOS stub 布局头的 `base_end`；布局头为零时取 PE 映像末尾；都不命中时从
/// PE 映像末尾（非 PE 时从文件开头）向后找第一个内容末尾能接上下一项的 TLV 头。随后沿 TLV
/// 链按内容长度跳步，不读取数据块内容；`\0INDEX` 与包体吻合时直接采用索引，只核对首末两个
/// 数据块的 TLV 头。某一项的内容末尾接不上下一项时视为该项损坏，从它的内容起点向后重新找下一项。
pub fn scan_embedded(bytes: &[u8]) -> Vec<Embedded> {
    let pe = pe_image(bytes);
    let end = payload_end(bytes, pe.as_ref());
    let Some(first) = first_block(bytes, pe.as_ref(), end) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pos = first;
    while pos < end {
        // pos 只会是已校验的 TLV 头，或签名表前的补零
        let Some(entry) = read_tlv(bytes, pos) else {
            break;
        };
        let next = entry.offset + entry.size;
        if !lands(bytes, next, end) {
            // 内容长度损坏：丢弃该项，在其内容中找下一项。找不到时它是最后一项，
            // 其后是不属于包的数据，按原样保留。
            match find_block(bytes, entry.offset, end) {
                Some(found) => pos = found,
                None => {
                    out.push(entry);
                    break;
                }
            }
            continue;
        }
        if entry.name == "\0INDEX" {
            if let Some((indexed, indexed_end)) = indexed_entries(bytes, &entry, first, end) {
                out.push(entry);
                out.extend(indexed);
                pos = indexed_end;
                continue;
            }
        }
        out.push(entry);
        pos = next;
    }
    out
}

/// 索引中位于 `\0INDEX` 之后的条目及其末尾。条目须首尾相接、从 `\0INDEX` 末尾开始，末尾接上
/// 下一项或 payload 末尾，首末两项的 TLV 头与索引一致；任一条不满足时返回 `None`，由调用方
/// 逐项读取 TLV 头。
fn indexed_entries(
    bytes: &[u8],
    index: &Embedded,
    start: usize,
    end: usize,
) -> Option<(Vec<Embedded>, usize)> {
    let index_end = index.offset + index.size;
    let mut entries: Vec<Embedded> = parse_index(&bytes[index.offset..index_end], start)?
        .into_iter()
        .filter(|e| e.raw_offset >= index_end)
        .collect();
    entries.sort_by_key(|e| e.raw_offset);
    let (head, tail) = (entries.first()?, entries.last()?);
    if head.raw_offset != index_end {
        return None;
    }
    for pair in entries.windows(2) {
        if pair[0].offset.checked_add(pair[0].size)? != pair[1].raw_offset {
            return None;
        }
    }
    let entries_end = tail.offset.checked_add(tail.size)?;
    if entries_end > end || !lands(bytes, entries_end, end) {
        return None;
    }
    if read_tlv(bytes, head.raw_offset).as_ref() != Some(head)
        || read_tlv(bytes, tail.raw_offset).as_ref() != Some(tail)
    {
        return None;
    }
    Some((entries, entries_end))
}

fn first_block(bytes: &[u8], pe: Option<&PeImage>, end: usize) -> Option<usize> {
    let region = &bytes[..bytes.len().min(LAYOUT_SEARCH_LEN)];
    if let Some(layout) = parse_pack_layout(region) {
        let at = layout.base_end as usize;
        if at > 0 && read_tlv(bytes, at).is_some() {
            return Some(at);
        }
    }
    let Some(pe) = pe else {
        return find_block(bytes, 0, end);
    };
    // 先签名再打包的 base，TLV 紧跟在签名表之后
    let after_cert = pe
        .cert
        .filter(|(off, _)| *off >= pe.sections_end)
        .map(|(off, size)| off + size);
    for at in std::iter::once(pe.sections_end).chain(after_cert) {
        if at < end && read_tlv(bytes, at).is_some() {
            return Some(at);
        }
    }
    if pe.sections_end >= end {
        // 未打包的裸程序
        return None;
    }
    find_block(bytes, pe.sections_end, end)
}

/// 在 `[from, end)` 内找下一个 TLV 头，且其内容末尾接上下一项或 payload 末尾。
fn find_block(bytes: &[u8], from: usize, end: usize) -> Option<usize> {
    let mut at = from;
    while at + TLV_MAGIC.len() <= end {
        let pos = at
            + bytes[at..end]
                .windows(TLV_MAGIC.len())
                .position(|w| w == TLV_MAGIC)?;
        if let Some(entry) = read_tlv(bytes, pos) {
            if lands(bytes, entry.offset + entry.size, end) {
                return Some(pos);
            }
        }
        at = pos + 1;
    }
    None
}

/// 一项内容的末尾 `next` 是否落在合法边界：payload 末尾、签名表前的对齐补零，或下一个 TLV 头。
fn lands(bytes: &[u8], next: usize, end: usize) -> bool {
    if next == end {
        return true;
    }
    if next > end {
        return false;
    }
    if end - next < 8 && bytes[next..end].iter().all(|b| *b == 0) {
        return true;
    }
    read_tlv(bytes, next).is_some()
}

/// 签名表恰好占据文件末尾且位于节数据之后时，它是打包之后追加的，payload 止于其起点。
fn payload_end(bytes: &[u8], pe: Option<&PeImage>) -> usize {
    match pe.and_then(|pe| pe.cert.map(|cert| (pe.sections_end, cert))) {
        Some((sections_end, (off, size)))
            if off >= sections_end && off.checked_add(size) == Some(bytes.len()) =>
        {
            off
        }
        _ => bytes.len(),
    }
}

struct PeImage {
    /// 各节原始数据末尾的最大值
    sections_end: usize,
    /// 安全目录（Authenticode 签名表）的文件偏移与长度
    cert: Option<(usize, usize)>,
}

fn pe_image(bytes: &[u8]) -> Option<PeImage> {
    if !is_pe_at(bytes, 0) {
        return None;
    }
    let pe = le_u32(bytes, 0x3C)? as usize;
    let sections = le_u16(bytes, pe + 6)? as usize;
    let optional_size = le_u16(bytes, pe + 20)? as usize;
    let optional = pe + 24;
    let dirs = match le_u16(bytes, optional)? {
        0x10b => optional + 96,
        0x20b => optional + 112,
        _ => return None,
    };
    const SECURITY_DIR: usize = 4;
    let dir_count = le_u32(bytes, dirs - 4)? as usize;
    let cert_entry = dirs + SECURITY_DIR * 8;
    let cert = if dir_count > SECURITY_DIR && cert_entry + 8 <= optional + optional_size {
        let off = le_u32(bytes, cert_entry)? as usize;
        let size = le_u32(bytes, cert_entry + 4)? as usize;
        (off > 0 && size > 0).then_some((off, size))
    } else {
        None
    };
    let table = optional + optional_size;
    let mut sections_end = 0;
    for i in 0..sections {
        let section = table + i * 40;
        let raw_size = le_u32(bytes, section + 16)? as usize;
        let raw_ptr = le_u32(bytes, section + 20)? as usize;
        if raw_size > 0 {
            sections_end = sections_end.max(raw_ptr + raw_size);
        }
    }
    Some(PeImage { sections_end, cert })
}

/// DOS `e_lfanew` 指向 `PE\0\0`。真实链接器产物的取值都在这一区间；`.rdata`、zstd 数据
/// 或图标里偶然出现的 `MZ` 几乎不会同时满足。
const PE_LFANEW_MIN: usize = 0x40;
const PE_LFANEW_MAX: usize = 0x1000;

pub fn is_pe_at(bytes: &[u8], offset: usize) -> bool {
    if offset.saturating_add(0x40) > bytes.len() {
        return false;
    }
    if bytes[offset] != b'M' || bytes[offset + 1] != b'Z' {
        return false;
    }
    let Some(e_lfanew) = le_u32(bytes, offset + 0x3C) else {
        return false;
    };
    let e_lfanew = e_lfanew as usize;
    if !(PE_LFANEW_MIN..=PE_LFANEW_MAX).contains(&e_lfanew) {
        return false;
    }
    let pe = offset + e_lfanew;
    bytes.get(pe..pe + 4) == Some(&b"PE\0\0"[..])
}

/// 数据块按内容 hash 命名：md5 优先于 xxh，与 builder pack 写入时一致。
pub fn preferred_file_hash<'a>(
    md5: &'a Option<String>,
    xxh: &'a Option<String>,
) -> Option<&'a String> {
    md5.as_ref().or(xxh.as_ref())
}

/// 读取名为 `name` 的内嵌 JSON；不存在时返回 `Ok(None)`。走 `from_str`：`RepoMetadata` 的反序列化
/// 按 Deserializer 类型单态化，安装器只用 `StrRead` 一种，省掉一整份 `SliceRead` 副本。
pub fn read_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    embedded: &[Embedded],
    name: &str,
) -> serde_json::Result<Option<T>> {
    let Some(entry) = embedded.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes[entry.offset..entry.offset + entry.size]);
    serde_json::from_str(&text).map(Some)
}

/// 归档 metadata 里的相对路径拼到 `root` 下。只允许 Normal 组件（子目录合法）；`..`、绝对
/// 路径、盘符、UNC 与空路径返回 `None`，防止包内路径把文件写到 `root` 之外。
pub fn relative_under_root(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let mut normal_count = 0;
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => {
                path.push(part);
                normal_count += 1;
            }
            _ => return None,
        }
    }
    (normal_count > 0).then_some(path)
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const E_LFANEW: usize = 0x80;
    const OPTIONAL_SIZE: usize = 112 + 16 * 8;
    const SECTION_TABLE: usize = E_LFANEW + 24 + OPTIONAL_SIZE;
    const SECTION_START: usize = 0x200;
    const DOS_STUB_AT: usize = 0x4E;

    /// 最小 PE32+：一个节，原始数据占 `[0x200, len)`。
    fn pe(len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&(E_LFANEW as u32).to_le_bytes());
        b[DOS_STUB_AT..DOS_STUB_AT + 38].copy_from_slice(b"This program cannot be run in DOS mode");
        b[E_LFANEW..E_LFANEW + 4].copy_from_slice(b"PE\0\0");
        b[E_LFANEW + 6..E_LFANEW + 8].copy_from_slice(&1u16.to_le_bytes());
        b[E_LFANEW + 20..E_LFANEW + 22].copy_from_slice(&(OPTIONAL_SIZE as u16).to_le_bytes());
        let optional = E_LFANEW + 24;
        b[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        let section = SECTION_TABLE;
        b[section + 16..section + 20]
            .copy_from_slice(&((len - SECTION_START) as u32).to_le_bytes());
        b[section + 20..section + 24].copy_from_slice(&(SECTION_START as u32).to_le_bytes());
        b
    }

    fn set_cert_dir(b: &mut [u8], off: usize, size: usize) {
        let entry = E_LFANEW + 24 + 112 + 4 * 8;
        b[entry..entry + 4].copy_from_slice(&(off as u32).to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&(size as u32).to_le_bytes());
    }

    fn write_layout(b: &mut [u8], layout: [u32; 5]) {
        let mut header = LAYOUT_MARKER.to_vec();
        for v in layout {
            header.extend_from_slice(&v.to_be_bytes());
        }
        b[DOS_STUB_AT..DOS_STUB_AT + header.len()].copy_from_slice(&header);
    }

    fn tlv(name: &str, body: &[u8]) -> Vec<u8> {
        let mut out = TLV_MAGIC.to_vec();
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    struct Pack {
        bytes: Vec<u8>,
        /// 期望的 (名字, 内容)，按文件顺序
        expected: Vec<(String, Vec<u8>)>,
    }

    /// 按 builder pack 的顺序与索引规则拼包；`files` 为空时不写 `\0INDEX` 且布局头全零。
    fn build(base: Vec<u8>, files: &[(&str, &[u8])]) -> Pack {
        let config: &[u8] = br#"{"appName":"Demo"}"#;
        let image: &[u8] = b"PNG";
        let meta: &[u8] = br#"{"hashed":[]}"#;
        let mut head = vec![("\0CONFIG", config), ("\0IMAGE", image)];
        let tail: Vec<(&str, &[u8])> = std::iter::once(("\0META", meta))
            .chain(files.iter().copied())
            .collect();

        // 索引不含自身；位于 \0INDEX 之后的条目偏移要加上 \0INDEX 整个 TLV 的长度
        let all: Vec<(&str, &[u8])> = head.iter().chain(tail.iter()).copied().collect();
        let index_tlv_len =
            all.iter().map(|(n, _)| 1 + n.len() + 8).sum::<usize>() + get_header_size("\0INDEX");
        let mut index_bin = Vec::new();
        let mut rel = 0usize;
        for (i, (name, body)) in all.iter().enumerate() {
            if i == head.len() {
                rel += index_tlv_len;
            }
            rel += get_header_size(name);
            index_bin.push(name.len() as u8);
            index_bin.extend_from_slice(name.as_bytes());
            index_bin.extend_from_slice(&(body.len() as u32).to_be_bytes());
            index_bin.extend_from_slice(&(rel as u32).to_be_bytes());
            rel += body.len();
        }

        let mut bytes = base;
        let base_end = bytes.len() as u32;
        if files.is_empty() {
            write_layout(&mut bytes, [0; 5]);
        } else {
            write_layout(&mut bytes, [base_end, 0, 0, index_tlv_len as u32, 0]);
            head.push(("\0INDEX", &index_bin));
        }
        let mut expected = Vec::new();
        for (name, body) in head.iter().chain(tail.iter()) {
            bytes.extend_from_slice(&tlv(name, body));
            expected.push((name.to_string(), body.to_vec()));
        }
        Pack { bytes, expected }
    }

    fn names_and_bodies(bytes: &[u8], found: &[Embedded]) -> Vec<(String, Vec<u8>)> {
        found
            .iter()
            .map(|e| (e.name.clone(), bytes[e.offset..e.offset + e.size].to_vec()))
            .collect()
    }

    fn files() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("aaa", &b"first"[..]),
            ("bbb", &b"second-block"[..]),
            ("ccc", &b"third"[..]),
        ]
    }

    fn raw_offset_of(pack: &Pack, name: &str) -> usize {
        scan_embedded(&pack.bytes)
            .into_iter()
            .find(|e| e.name == name)
            .unwrap()
            .raw_offset
    }

    #[test]
    fn offline_pack_located_by_layout_header() {
        let pack = build(pe(0x400), &files());
        let found = scan_embedded(&pack.bytes);
        assert_eq!(found[0].raw_offset, 0x400);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
        // 文件里的索引与 TLV 头一致
        let index = found.iter().find(|e| e.name == "\0INDEX").unwrap();
        let indexed =
            parse_index(&pack.bytes[index.offset..index.offset + index.size], 0x400).unwrap();
        for entry in &indexed {
            assert_eq!(
                read_tlv(&pack.bytes, entry.raw_offset).as_ref(),
                Some(entry)
            );
        }
    }

    #[test]
    fn valid_index_is_used_without_reading_data_headers() {
        let mut pack = build(pe(0x400), &files());
        let middle = raw_offset_of(&pack, "bbb");
        pack.bytes[middle] = b'?';
        let found = scan_embedded(&pack.bytes);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
    }

    #[test]
    fn online_pack_located_at_pe_end() {
        let pack = build(pe(0x400), &[]);
        assert_eq!(parse_pack_layout(&pack.bytes).unwrap().base_end, 0);
        let found = scan_embedded(&pack.bytes);
        assert_eq!(found[0].raw_offset, 0x400);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
    }

    #[test]
    fn cleared_layout_header_located_at_pe_end() {
        // 卸载器/更新器：clear_index_mark 把布局头抹零
        let mut pack = build(pe(0x400), &files());
        write_layout(&mut pack.bytes, [0; 5]);
        let found = scan_embedded(&pack.bytes);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
    }

    #[test]
    fn bare_exe_has_no_entries() {
        assert!(scan_embedded(&pe(0x400)).is_empty());
        // 未打包但签过名
        let mut signed = pe(0x400);
        signed.extend_from_slice(&[0x5A; 24]);
        set_cert_dir(&mut signed, 0x400, 24);
        assert!(scan_embedded(&signed).is_empty());
    }

    #[test]
    fn signature_appended_after_pack_is_not_an_entry() {
        let mut pack = build(pe(0x400), &files());
        // 签名表 8 字节对齐，前面补零；内容里放一个伪 TLV 头
        let pad = (8 - pack.bytes.len() % 8) % 8;
        pack.bytes.extend(std::iter::repeat_n(0u8, pad));
        let cert_at = pack.bytes.len();
        let mut cert = vec![0x30u8; 16];
        cert.extend_from_slice(&tlv("ddd", b"x"));
        pack.bytes.extend_from_slice(&cert);
        set_cert_dir(&mut pack.bytes, cert_at, cert.len());
        let found = scan_embedded(&pack.bytes);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
    }

    #[test]
    fn base_signed_before_packing() {
        let mut base = pe(0x400);
        base.extend_from_slice(&[0x30; 16]);
        set_cert_dir(&mut base, 0x400, 16);
        // 签名表紧跟节数据，布局头抹零时首块在签名表之后
        let mut pack = build(base, &files());
        write_layout(&mut pack.bytes, [0; 5]);
        let found = scan_embedded(&pack.bytes);
        assert_eq!(found[0].raw_offset, 0x410);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
    }

    #[test]
    fn blocks_appended_after_index() {
        let mut pack = build(pe(0x400), &files());
        let extra = tlv("Microsoft.VCRedist.2015+.x64", b"runtime");
        pack.bytes.extend_from_slice(&extra);
        pack.expected
            .push(("Microsoft.VCRedist.2015+.x64".into(), b"runtime".to_vec()));
        let found = scan_embedded(&pack.bytes);
        assert_eq!(names_and_bodies(&pack.bytes, &found), pack.expected);
    }

    #[test]
    fn truncated_pack_drops_out_of_bounds_entries() {
        let mut pack = build(pe(0x400), &files());
        pack.bytes.truncate(pack.bytes.len() - 2);
        let found = scan_embedded(&pack.bytes);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["\0CONFIG", "\0IMAGE", "\0INDEX", "\0META", "aaa", "bbb"]
        );
        // 索引仍列出全部条目，安装器据此比对出缺块
        let index = &found[2];
        let indexed =
            parse_index(&pack.bytes[index.offset..index.offset + index.size], 0x400).unwrap();
        assert!(indexed.iter().any(|e| e.name == "ccc"));
    }

    #[test]
    fn fake_base_scans_to_first_chained_block() {
        // 非 PE 的 base 里有一段像 TLV 头的数据，但内容末尾接不上任何一项
        let mut bytes = vec![0u8; 64];
        bytes.extend_from_slice(&tlv("fake", b"zz"));
        bytes.extend_from_slice(&[1, 2, 3]);
        let first = bytes.len();
        bytes.extend_from_slice(&tlv("\0CONFIG", b"{}"));
        bytes.extend_from_slice(&tlv("abc", b"payload"));
        let found = scan_embedded(&bytes);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["\0CONFIG", "abc"]);
        assert_eq!(found[0].raw_offset, first);
    }

    #[test]
    fn magic_inside_content_is_not_an_entry() {
        let inner = tlv("bad", b"xxxxx");
        let mut bytes = tlv("real1", &inner);
        bytes.extend_from_slice(&tlv("real2", b"ok"));
        let names: Vec<_> = scan_embedded(&bytes).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["real1", "real2"]);
    }

    #[test]
    fn broken_length_resyncs_to_next_block() {
        // 在线包没有索引，逐项跳步；\0IMAGE 的内容长度偏大或偏小都不能连带丢掉其后的 \0META
        for delta in [3i64, -3] {
            let mut pack = build(pe(0x400), &[]);
            let image = raw_offset_of(&pack, "\0IMAGE");
            let size_at = image + 4 + 2 + "\0IMAGE".len();
            let size = u32::from_be_bytes(pack.bytes[size_at..size_at + 4].try_into().unwrap());
            let broken = (size as i64 + delta) as u32;
            pack.bytes[size_at..size_at + 4].copy_from_slice(&broken.to_be_bytes());
            let names: Vec<_> = scan_embedded(&pack.bytes)
                .into_iter()
                .map(|e| e.name)
                .collect();
            assert_eq!(names, ["\0CONFIG", "\0META"], "delta {delta}");
        }
    }

    #[test]
    fn last_block_followed_by_unknown_data_is_kept() {
        let mut bytes = tlv("\0CONFIG", b"{}");
        bytes.extend_from_slice(&tlv("abc", b"payload"));
        bytes.extend_from_slice(b"trailing bytes that are not a pack entry");
        let names: Vec<_> = scan_embedded(&bytes).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["\0CONFIG", "abc"]);
    }

    #[test]
    fn index_parse_rejects_truncated_entries() {
        let mut content = vec![3u8];
        content.extend_from_slice(b"abc");
        content.extend_from_slice(&5u32.to_be_bytes());
        content.extend_from_slice(&100u32.to_be_bytes());
        assert_eq!(parse_index(&content, 0).unwrap().len(), 1);
        for cut in 1..content.len() {
            assert!(parse_index(&content[..cut], 0).is_none(), "cut {cut}");
        }
        // 偏移小于 TLV 头长度
        let mut short = content.clone();
        let at = short.len() - 4;
        short[at..].copy_from_slice(&1u32.to_be_bytes());
        assert!(parse_index(&short, 0).is_none());
    }

    #[test]
    fn clear_layout_zeroes_only_the_layout_fields() {
        let mut head = pe(0x400)[..0x200].to_vec();
        write_layout(&mut head, [0x400, 1, 2, 3, 4]);
        let original = head.clone();
        assert!(clear_layout(&mut head));
        assert_eq!(parse_pack_layout(&head).unwrap().base_end, 0);
        let fields = DOS_STUB_AT + LAYOUT_MARKER.len();
        assert!(head[fields..fields + 20].iter().all(|b| *b == 0));
        assert_eq!(head[..fields], original[..fields]);
        assert_eq!(head[fields + 20..], original[fields + 20..]);
        // 字段被截断时不改动
        let mut short = original[..fields + 10].to_vec();
        assert!(!clear_layout(&mut short));
        assert_eq!(short, original[..fields + 10]);
    }

    #[test]
    fn pe_detection_requires_pe_signature() {
        assert!(is_pe_at(&pe(0x400), 0));
        let mut false_mz = vec![0x4D, 0x5A, 0x90, 0x00];
        false_mz.extend_from_slice(&[0u8; 60]);
        assert!(!is_pe_at(&false_mz, 0));
    }

    #[test]
    fn relative_path_guard() {
        let root = Path::new(r"C:\temp\sfx");
        assert_eq!(
            relative_under_root(root, "a/b.txt"),
            Some(PathBuf::from(r"C:\temp\sfx\a\b.txt"))
        );
        assert!(relative_under_root(root, "a\\b.txt").is_some());
        for bad in [
            "../evil",
            "..\\evil",
            "a/../../evil",
            "/abs",
            "C:/abs",
            r"C:\Windows\evil.exe",
            r"\\server\share\evil.exe",
            "",
        ] {
            assert!(relative_under_root(root, bad).is_none(), "{bad:?}");
        }
    }
}
