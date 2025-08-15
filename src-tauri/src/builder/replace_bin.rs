use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::pack::gen_index_header;
use crate::{cli::ReplaceBinArgs, local::get_reader_for_bundle};

#[derive(Debug)]
struct InstallerIndex {
    base_end: u32,
    config_end: u32,
    theme_end: u32,
    index_end: u32,
    manifest_end: u32,
}

#[derive(Debug)]
struct FileIndexEntry {
    name: String,
    size: u32,
    offset: u32,
}

// 解析 PE 头中的安装程序索引信息
async fn parse_installer_index(input: &Path) -> Result<InstallerIndex, String> {
    let file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;

    // 查找 "!KachinaInstaller!" 标识
    let file_size = file.len();
    let search_data = file.slice(0, file_size.min(8192)); // 在前8KB中搜索
    let pattern = b"!KachinaInstaller!";

    let pattern_pos = search_data
        .windows(pattern.len())
        .position(|window| window == pattern)
        .ok_or("Failed to find !KachinaInstaller! pattern in file")?;

    let data_start = pattern_pos + pattern.len();
    if data_start + 20 > search_data.len() {
        return Err("Not enough data after pattern".to_string());
    }

    let base_end = u32::from_be_bytes(search_data[data_start..data_start + 4].try_into().unwrap());
    let config_end = u32::from_be_bytes(
        search_data[data_start + 4..data_start + 8]
            .try_into()
            .unwrap(),
    );
    let theme_end = u32::from_be_bytes(
        search_data[data_start + 8..data_start + 12]
            .try_into()
            .unwrap(),
    );
    let index_end = u32::from_be_bytes(
        search_data[data_start + 12..data_start + 16]
            .try_into()
            .unwrap(),
    );
    let manifest_end = u32::from_be_bytes(
        search_data[data_start + 16..data_start + 20]
            .try_into()
            .unwrap(),
    );

    Ok(InstallerIndex {
        base_end,
        config_end,
        theme_end,
        index_end,
        manifest_end,
    })
}

// 解析文件索引数据
async fn parse_file_index(
    input: &Path,
    index_offset: u64,
    index_size: u64,
) -> Result<Vec<FileIndexEntry>, String> {
    let file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;

    // 跳过 TLV 头部 ("!IN\0" + name_len + name + size)
    // 查找 \0INDEX 的数据部分
    let mut current_pos = index_offset;
    let _end_pos = index_offset + index_size;

    // 跳过 TLV 头部: !IN\0 (4) + name_len (2) + \0INDEX (6) + size (4) = 16 bytes
    current_pos += 4; // !IN\0

    let name_len_data = file.slice(current_pos as usize, 2);
    let name_len = u16::from_be_bytes(name_len_data.try_into().unwrap()) as u64;
    current_pos += 2 + name_len; // name_len + name

    let content_size_data = file.slice(current_pos as usize, 4);
    let content_size = u32::from_be_bytes(content_size_data.try_into().unwrap()) as u64;
    current_pos += 4; // content size

    // 现在 current_pos 指向索引数据的开始
    let index_data = file.slice(current_pos as usize, content_size as usize);

    let mut entries = Vec::new();
    let mut pos = 0;

    while pos < index_data.len() {
        if pos >= index_data.len() {
            break;
        }

        // 读取名称长度 (u8)
        let name_len = index_data[pos] as usize;
        pos += 1;

        if pos + name_len > index_data.len() {
            break;
        }

        // 读取名称
        let name = String::from_utf8_lossy(&index_data[pos..pos + name_len]).to_string();
        pos += name_len;

        if pos + 8 > index_data.len() {
            break;
        }

        // 读取大小 (u32 big endian)
        let size = u32::from_be_bytes(index_data[pos..pos + 4].try_into().unwrap());
        pos += 4;

        // 读取偏移量 (u32 big endian)
        let offset = u32::from_be_bytes(index_data[pos..pos + 4].try_into().unwrap());
        pos += 4;

        entries.push(FileIndexEntry { name, size, offset });
    }

    Ok(entries)
}

// 更新偏移量
fn update_offsets(
    old_index: &InstallerIndex,
    file_entries: &mut [FileIndexEntry],
    size_diff: i64,
) -> InstallerIndex {
    // 更新 PE 头索引
    let new_index = InstallerIndex {
        base_end: (old_index.base_end as i64 + size_diff) as u32,
        config_end: (old_index.config_end as i64 + size_diff) as u32,
        theme_end: if old_index.theme_end > 0 {
            (old_index.theme_end as i64 + size_diff) as u32
        } else {
            0
        },
        index_end: (old_index.index_end as i64 + size_diff) as u32,
        manifest_end: if old_index.manifest_end > 0 {
            (old_index.manifest_end as i64 + size_diff) as u32
        } else {
            0
        },
    };

    // 更新文件索引中的偏移量（除了 \0CONFIG 和 \0IMAGE）
    for entry in file_entries.iter_mut() {
        if entry.name != "\\0CONFIG" && entry.name != "\\0IMAGE" {
            entry.offset = (entry.offset as i64 + size_diff) as u32;
        }
    }

    new_index
}

// 序列化文件索引
fn serialize_file_index(entries: &[FileIndexEntry]) -> Vec<u8> {
    let mut data = Vec::new();

    for entry in entries {
        let name_bytes = entry.name.as_bytes();
        let name_len = name_bytes.len() as u8;

        data.push(name_len);
        data.extend_from_slice(name_bytes);
        data.extend_from_slice(&entry.size.to_be_bytes());
        data.extend_from_slice(&entry.offset.to_be_bytes());
    }

    data
}

// 复制数据范围
async fn copy_data_range(
    input: &Path,
    output: &mut File,
    start: u64,
    len: u64,
) -> Result<(), String> {
    let input_file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;

    let chunk_size = 8192; // 8KB chunks
    let mut copied = 0u64;

    while copied < len {
        let to_copy = (len - copied).min(chunk_size);
        let data = input_file.slice((start + copied) as usize, to_copy as usize);

        output.write_all(data).await.map_err(|e| e.to_string())?;
        copied += to_copy;
    }

    Ok(())
}

// 更新 PE 头
async fn update_pe_header(output: &mut File, new_index_header: &[u8]) -> Result<(), String> {
    // 查找 "This program cannot be run in DOS mode" 位置
    output
        .seek(SeekFrom::Start(0))
        .await
        .map_err(|e| e.to_string())?;

    let mut buffer = vec![0u8; 8192]; // 读取前8KB
    let bytes_read = output.read(&mut buffer).await.map_err(|e| e.to_string())?;
    buffer.truncate(bytes_read);

    let target_str = b"This program cannot be run in DOS mode";
    let pos = buffer
        .windows(target_str.len())
        .position(|window| window == target_str)
        .ok_or("Failed to find DOS mode string in PE header")?;

    // 检查长度是否匹配
    if new_index_header.len() != target_str.len() {
        return Err(format!(
            "Index header length ({}) doesn't match target string length ({})",
            new_index_header.len(),
            target_str.len()
        ));
    }

    // 替换数据
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

// 写入新的安装程序
async fn write_new_installer(
    input: &Path,
    output: &Path,
    new_base: &[u8],
    old_index: &InstallerIndex,
    new_pe_index: &InstallerIndex,
    new_file_index: &[FileIndexEntry],
) -> Result<(), String> {
    let mut output_file = File::create(output).await.map_err(|e| e.to_string())?;

    // 1. 写入新的基础二进制
    output_file
        .write_all(new_base)
        .await
        .map_err(|e| e.to_string())?;

    // 2. 复制配置数据 (\0CONFIG)
    let config_start = old_index.base_end as u64;
    let config_len = old_index.config_end as u64 - config_start;
    copy_data_range(input, &mut output_file, config_start, config_len).await?;

    // 3. 复制图片数据 (\0IMAGE) - 如果存在
    if old_index.theme_end > old_index.config_end {
        let image_start = old_index.config_end as u64;
        let image_len = old_index.theme_end as u64 - image_start;
        copy_data_range(input, &mut output_file, image_start, image_len).await?;
    }

    // 4. 写入新的文件索引
    let index_data = serialize_file_index(new_file_index);

    // 写入 TLV 头部
    let header = b"!IN\0";
    let name = b"\\0INDEX";
    let name_len = (name.len() as u16).to_be_bytes();
    let content_len = (index_data.len() as u32).to_be_bytes();

    output_file
        .write_all(header)
        .await
        .map_err(|e| e.to_string())?;
    output_file
        .write_all(&name_len)
        .await
        .map_err(|e| e.to_string())?;
    output_file
        .write_all(name)
        .await
        .map_err(|e| e.to_string())?;
    output_file
        .write_all(&content_len)
        .await
        .map_err(|e| e.to_string())?;
    output_file
        .write_all(&index_data)
        .await
        .map_err(|e| e.to_string())?;

    // 5. 复制元数据和文件数据
    let data_start = old_index.index_end as u64;
    let input_file = AsyncMmapFile::open(input)
        .await
        .map_err(|e| e.to_string())?;
    let total_size = input_file.len() as u64;

    if data_start < total_size {
        let data_len = total_size - data_start;
        copy_data_range(input, &mut output_file, data_start, data_len).await?;
    }

    // 6. 更新 PE 头中的索引信息
    output_file.flush().await.map_err(|e| e.to_string())?;
    drop(output_file);

    let mut output_file = File::options()
        .write(true)
        .open(output)
        .await
        .map_err(|e| e.to_string())?;

    let new_index_header = gen_index_header(
        new_pe_index.base_end,
        new_pe_index.config_end - new_pe_index.base_end,
        if new_pe_index.theme_end > 0 {
            new_pe_index.theme_end - new_pe_index.config_end
        } else {
            0
        },
        new_pe_index.index_end - new_pe_index.theme_end.max(new_pe_index.config_end),
        if new_pe_index.manifest_end > 0 {
            new_pe_index.manifest_end - new_pe_index.index_end
        } else {
            0
        },
    );

    update_pe_header(&mut output_file, &new_index_header).await?;

    output_file.flush().await.map_err(|e| e.to_string())?;

    Ok(())
}

// 主要的替换函数
pub async fn replace_bin_cli(args: ReplaceBinArgs) -> Result<(), String> {
    // 验证输入文件存在
    if !args.input.exists() {
        return Err(format!(
            "Input file does not exist: {}",
            args.input.display()
        ));
    }

    // 验证输出目录可写
    if let Some(parent) = args.output.parent() {
        if !parent.exists() {
            return Err(format!(
                "Output directory does not exist: {}",
                parent.display()
            ));
        }
    }

    println!("Parsing installer index...");

    // 1. 解析原始安装程序的索引信息
    let old_index = parse_installer_index(&args.input).await?;
    println!("Original index: {:?}", old_index);

    // 2. 解析文件索引数据
    let index_start = if old_index.theme_end > 0 {
        old_index.theme_end as u64
    } else {
        old_index.config_end as u64
    };
    let index_size = old_index.index_end as u64 - index_start;

    let mut file_entries = if index_size > 0 {
        parse_file_index(&args.input, index_start, index_size).await?
    } else {
        Vec::new()
    };

    println!("Found {} file entries", file_entries.len());

    // 3. 获取新的基础二进制
    println!("Loading new base binary...");
    let mut new_base_data = Vec::new();
    let mut reader = get_reader_for_bundle().await.map_err(|e| e.to_string())?;
    tokio::io::copy(&mut reader, &mut new_base_data)
        .await
        .map_err(|e| e.to_string())?;

    println!("New base size: {} bytes", new_base_data.len());
    println!("Old base size: {} bytes", old_index.base_end);

    // 4. 计算大小差异并更新偏移量
    let size_diff = new_base_data.len() as i64 - old_index.base_end as i64;
    println!("Size difference: {} bytes", size_diff);

    let new_pe_index = update_offsets(&old_index, &mut file_entries, size_diff);
    println!("New index: {:?}", new_pe_index);

    // 5. 写入新的安装程序
    println!("Writing new installer...");
    write_new_installer(
        &args.input,
        &args.output,
        &new_base_data,
        &old_index,
        &new_pe_index,
        &file_entries,
    )
    .await?;

    println!(
        "Successfully created new installer: {}",
        args.output.display()
    );
    Ok(())
}
