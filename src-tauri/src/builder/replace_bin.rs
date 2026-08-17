use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::pack::gen_index_header;
use crate::{cli::ReplaceBinArgs, local::get_reader_for_bundle};

const MARKER: &[u8] = b"!KachinaInstaller!";
const DOS_STUB: &[u8] = b"This program cannot be run in DOS mode";
const TLV_MAGIC: &[u8] = b"!IN\0";

/// Values stored after `!KachinaInstaller!` in the DOS stub.
/// `base_end` is an absolute file offset; the rest are section *lengths*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackLayout {
    pub base_end: u32,
    pub config_len: u32,
    pub theme_len: u32,
    pub index_len: u32,
    pub manifest_len: u32,
}

impl PackLayout {
    pub fn to_header(&self) -> Vec<u8> {
        gen_index_header(
            self.base_end,
            self.config_len,
            self.theme_len,
            self.index_len,
            self.manifest_len,
        )
    }
}

pub fn parse_pack_layout(header_region: &[u8]) -> Result<PackLayout, String> {
    let pattern_pos = header_region
        .windows(MARKER.len())
        .position(|window| window == MARKER)
        .ok_or_else(|| "Failed to find !KachinaInstaller! pattern in file".to_string())?;
    let data_start = pattern_pos + MARKER.len();
    if data_start + 20 > header_region.len() {
        return Err("Not enough data after !KachinaInstaller! pattern".to_string());
    }
    Ok(PackLayout {
        base_end: read_u32be(header_region, data_start)?,
        config_len: read_u32be(header_region, data_start + 4)?,
        theme_len: read_u32be(header_region, data_start + 8)?,
        index_len: read_u32be(header_region, data_start + 12)?,
        manifest_len: read_u32be(header_region, data_start + 16)?,
    })
}

fn read_u32be(data: &[u8], offset: usize) -> Result<u32, String> {
    data.get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "Truncated installer index".to_string())
}

async fn parse_installer_index(input: &Path) -> Result<PackLayout, String> {
    let file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;
    let search_len = file.len().min(65536);
    parse_pack_layout(file.slice(0, search_len))
}

async fn payload_start(input: &Path, layout: &PackLayout) -> Result<u64, String> {
    let file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;
    let base = layout.base_end as usize;
    if base > 0 && base + 4 <= file.len() && file.slice(base, 4) == TLV_MAGIC {
        return Ok(layout.base_end as u64);
    }
    let embedded = crate::local::get_embedded(&file)
        .await
        .map_err(|e| e.to_string())?;
    let first = embedded
        .first()
        .ok_or_else(|| "Input has no packed !IN payload".to_string())?;
    Ok(first.raw_offset as u64)
}

async fn copy_data_range(
    input: &Path,
    output: &mut File,
    start: u64,
    len: u64,
) -> Result<(), String> {
    let input_file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;
    if start.saturating_add(len) > input_file.len() as u64 {
        return Err(format!(
            "Copy range {start}+{len} exceeds input size {}",
            input_file.len()
        ));
    }
    let chunk_size = 8192u64;
    let mut copied = 0u64;
    while copied < len {
        let to_copy = (len - copied).min(chunk_size);
        let data = input_file.slice((start + copied) as usize, to_copy as usize);
        output.write_all(data).await.map_err(|e| e.to_string())?;
        copied += to_copy;
    }
    Ok(())
}

fn find_dos_stub(buffer: &[u8]) -> Result<usize, String> {
    buffer
        .windows(DOS_STUB.len())
        .position(|window| window == DOS_STUB)
        .ok_or_else(|| "Failed to find DOS mode string in PE header".to_string())
}

async fn update_pe_header(output: &mut File, new_index_header: &[u8]) -> Result<(), String> {
    output
        .seek(SeekFrom::Start(0))
        .await
        .map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; 8192];
    let bytes_read = output.read(&mut buffer).await.map_err(|e| e.to_string())?;
    buffer.truncate(bytes_read);
    let pos = find_dos_stub(&buffer)?;
    if new_index_header.len() != DOS_STUB.len() {
        return Err(format!(
            "Index header length ({}) doesn't match DOS stub length ({})",
            new_index_header.len(),
            DOS_STUB.len()
        ));
    }
    output
        .seek(SeekFrom::Start(pos as u64))
        .await
        .map_err(|e| e.to_string())?;
    output
        .write_all(new_index_header)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn replace_base(
    input: &Path,
    output: &Path,
    new_base: &[u8],
) -> Result<PackLayout, String> {
    if new_base.len() > u32::MAX as usize {
        return Err("New base is larger than 4GiB".to_string());
    }
    let old_layout = parse_installer_index(input).await?;
    let start = payload_start(input, &old_layout).await?;
    let input_file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;
    let total_size = input_file.len() as u64;
    drop(input_file);
    if start > total_size {
        return Err("Packed payload start exceeds input size".to_string());
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create output directory: {e}"))?;
        }
    }

    let mut output_file = File::create(output).await.map_err(|e| e.to_string())?;
    output_file
        .write_all(new_base)
        .await
        .map_err(|e| e.to_string())?;
    copy_data_range(input, &mut output_file, start, total_size - start).await?;
    output_file.flush().await.map_err(|e| e.to_string())?;
    drop(output_file);

    let new_layout = PackLayout {
        base_end: new_base.len() as u32,
        config_len: old_layout.config_len,
        theme_len: old_layout.theme_len,
        index_len: old_layout.index_len,
        manifest_len: old_layout.manifest_len,
    };

    let mut output_file = File::options()
        .read(true)
        .write(true)
        .open(output)
        .await
        .map_err(|e| e.to_string())?;
    update_pe_header(&mut output_file, &new_layout.to_header()).await?;
    output_file.flush().await.map_err(|e| e.to_string())?;
    Ok(new_layout)
}

pub async fn replace_bin_cli(args: ReplaceBinArgs) -> Result<(), String> {
    if !args.input.exists() {
        return Err(format!(
            "Input file does not exist: {}",
            args.input.display()
        ));
    }

    println!("Parsing installer index...");
    let old_index = parse_installer_index(&args.input).await?;
    println!("Original layout: {:?}", old_index);

    println!("Loading new base binary...");
    let mut new_base_data = Vec::new();
    let mut reader = get_reader_for_bundle().await.map_err(|e| e.to_string())?;
    tokio::io::copy(&mut reader, &mut new_base_data)
        .await
        .map_err(|e| e.to_string())?;

    println!("New base size: {} bytes", new_base_data.len());
    println!("Old base size: {} bytes", old_index.base_end);

    println!("Writing new installer...");
    let new_layout = replace_base(&args.input, &args.output, &new_base_data).await?;
    println!("New layout: {:?}", new_layout);
    println!(
        "Successfully created new installer: {}",
        args.output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::get_header_size;

    fn pe_with(marker: &[u8], pad: usize) -> Vec<u8> {
        let mut data = b"MZ\x90\x00".to_vec();
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(marker);
        data.extend(std::iter::repeat(0u8).take(pad));
        data
    }

    fn config_tlv(body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(TLV_MAGIC);
        let name = b"\0CONFIG";
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn pack_layout_roundtrip() {
        let layout = PackLayout {
            base_end: 1000,
            config_len: 40,
            theme_len: 0,
            index_len: 80,
            manifest_len: 120,
        };
        let parsed = parse_pack_layout(&layout.to_header()).unwrap();
        assert_eq!(parsed, layout);
        assert_eq!(layout.to_header().len(), DOS_STUB.len());
    }

    #[test]
    fn header_stores_lengths_not_end_offsets() {
        let header = gen_index_header(8000, 40, 0, 80, 120);
        let layout = parse_pack_layout(&header).unwrap();
        assert_eq!(layout.base_end, 8000);
        assert_eq!(layout.config_len, 40);
        assert_ne!(layout.config_len, 8000 + 40);
    }

    #[tokio::test]
    async fn replace_keeps_payload_and_section_lengths() {
        let config = config_tlv(br#"{"appName":"Demo"}"#);
        let mut old_base = pe_with(DOS_STUB, 16);
        let old_layout = PackLayout {
            base_end: old_base.len() as u32,
            config_len: config.len() as u32,
            theme_len: 0,
            index_len: 0,
            manifest_len: 0,
        };
        let header = old_layout.to_header();
        let pos = find_dos_stub(&old_base).unwrap();
        old_base[pos..pos + header.len()].copy_from_slice(&header);

        let mut input = old_base;
        input.extend_from_slice(&config);

        let new_base = pe_with(DOS_STUB, 64);
        assert_ne!(new_base.len(), old_layout.base_end as usize);

        let dir = std::env::temp_dir().join(format!(
            "kachina-replace-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let input_path = dir.join("old.exe");
        let output_path = dir.join("new.exe");
        tokio::fs::write(&input_path, &input).await.unwrap();

        let new_layout = replace_base(&input_path, &output_path, &new_base)
            .await
            .unwrap();
        assert_eq!(new_layout.base_end, new_base.len() as u32);
        assert_eq!(new_layout.config_len, config.len() as u32);
        assert_eq!(new_layout.index_len, 0);

        let output = tokio::fs::read(&output_path).await.unwrap();
        assert_eq!(&output[new_base.len()..], config.as_slice());
        assert_eq!(&output[new_base.len()..new_base.len() + 4], TLV_MAGIC);
        let parsed = parse_pack_layout(&output[..8192.min(output.len())]).unwrap();
        assert_eq!(parsed, new_layout);
        assert!(find_dos_stub(&output).is_err());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn config_tlv_header_size_matches_pack() {
        assert_eq!(get_header_size("\0CONFIG"), 4 + 2 + "\0CONFIG".len() + 4);
    }
}
