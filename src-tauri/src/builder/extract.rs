use std::collections::HashMap;
use std::path::Path;

use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};

use crate::{
    cli::ExtractArgs,
    local::{get_embedded, Embedded},
    utils::metadata::RepoMetadata,
};

#[derive(Debug)]
struct FileInfo {
    file_type: FileType,
    hash_name: String,
    metadata_name: Option<String>,
    size: usize,
}

#[derive(Debug)]
enum FileType {
    Config,
    Image,
    Meta,
    File,
    Patch,
}

impl std::fmt::Display for FileType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileType::Config => write!(f, "CONFIG"),
            FileType::Image => write!(f, "IMAGE"),
            FileType::Meta => write!(f, "META"),
            FileType::File => write!(f, "FILE"),
            FileType::Patch => write!(f, "PATCH"),
        }
    }
}

// 参数验证函数
fn validate_args(args: &ExtractArgs) -> Result<(), String> {
    // 计算使用的功能数量
    let feature_count = [
        !args.name.is_empty(),
        !args.meta_name.is_empty(),
        args.all.is_some(),
        args.list,
    ]
    .iter()
    .filter(|&&x| x)
    .count();

    if feature_count > 1 {
        return Err("Only one extraction mode can be used at a time".to_string());
    }

    // 原有功能的文件数量匹配检查
    if !args.name.is_empty() && args.file.len() != args.name.len() && !args.file.is_empty() {
        return Err(
            "Files length must equal to names length, or files length must be 0".to_string(),
        );
    }

    // meta-name 功能的文件数量检查
    if !args.meta_name.is_empty()
        && args.file.len() != args.meta_name.len()
        && !args.file.is_empty()
    {
        return Err(
            "Files length must equal to meta-names length, or files length must be 0".to_string(),
        );
    }

    Ok(())
}

// 解析metadata功能
async fn parse_metadata(file: &AsyncMmapFile) -> Result<Option<RepoMetadata>, String> {
    let embedded = get_embedded(file).await.map_err(|e| e.to_string())?;

    // 查找 \0META 文件
    let meta_file = embedded.iter().find(|e| e.name == "\0META");

    if let Some(meta) = meta_file {
        let mut data = file
            .range_reader(meta.offset, meta.size)
            .map_err(|e| e.to_string())?;

        let mut buffer = Vec::new();
        tokio::io::copy(&mut data, &mut buffer)
            .await
            .map_err(|e| e.to_string())?;

        let metadata: RepoMetadata = serde_json::from_slice(&buffer)
            .map_err(|e| format!("Failed to parse metadata: {}", e))?;

        Ok(Some(metadata))
    } else {
        Ok(None)
    }
}

// 文件类型分类
fn classify_file_type(name: &str) -> FileType {
    match name {
        "\0CONFIG" => FileType::Config,
        "\0IMAGE" => FileType::Image,
        "\0META" => FileType::Meta,
        name if name.contains('_') && !name.starts_with('\0') => FileType::Patch,
        _ => FileType::File,
    }
}

// 构建hash到文件名的映射
fn build_hash_to_name_map(metadata: &RepoMetadata) -> HashMap<String, String> {
    let mut map = HashMap::new();

    // 处理普通文件
    if let Some(hashed) = &metadata.hashed {
        for file in hashed {
            if let Some(hash) = file.xxh.as_ref().or(file.md5.as_ref()) {
                map.insert(hash.clone(), file.file_name.clone());
            }
        }
    }

    // 处理补丁文件
    if let Some(patches) = &metadata.patches {
        for patch in patches {
            let from_hash = patch.from.xxh.as_ref().or(patch.from.md5.as_ref());
            let to_hash = patch.to.xxh.as_ref().or(patch.to.md5.as_ref());

            if let (Some(from), Some(to)) = (from_hash, to_hash) {
                let patch_name = format!("{}_{}", from, to);
                map.insert(patch_name, patch.file_name.clone());
            }
        }
    }

    map
}

// 收集文件信息
async fn collect_file_info(file: &AsyncMmapFile) -> Result<Vec<FileInfo>, String> {
    let embedded = get_embedded(file).await.map_err(|e| e.to_string())?;
    let metadata = parse_metadata(file).await?;

    let mut file_infos = Vec::new();

    // 构建hash到metadata name的映射
    let hash_to_name = if let Some(ref meta) = metadata {
        build_hash_to_name_map(meta)
    } else {
        HashMap::new()
    };

    for emb in embedded {
        let file_type = classify_file_type(&emb.name);
        let metadata_name = hash_to_name.get(&emb.name).cloned();

        file_infos.push(FileInfo {
            file_type,
            hash_name: emb.name,
            metadata_name,
            size: emb.size,
        });
    }

    Ok(file_infos)
}

// 文件大小格式化
fn format_file_size(size: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = size as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{}B", size as usize)
    } else {
        format!("{:.1}{}", size, UNITS[unit_index])
    }
}

// 字符串截断
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

// 实现--list功能
async fn list_files(file: &AsyncMmapFile) -> Result<(), String> {
    let file_infos = collect_file_info(file).await?;

    println!(
        "{:<10} {:<32} {:<20} {:<10}",
        "TYPE", "HASH NAME", "METADATA NAME", "SIZE"
    );
    println!("{}", "-".repeat(80));

    for info in file_infos {
        let type_str = format!("{}", info.file_type);
        let meta_name = info.metadata_name.unwrap_or_else(|| "-".to_string());
        let size_str = format_file_size(info.size);

        println!(
            "{:<10} {:<32} {:<20} {:<10}",
            type_str,
            truncate_string(&info.hash_name, 32),
            truncate_string(&meta_name, 20),
            size_str
        );
    }

    Ok(())
}

// 实现通过hash name提取（原有功能）
async fn extract_by_hash_name(
    file: &AsyncMmapFile,
    embedded: &[Embedded],
    names: &[String],
    output_files: &[std::path::PathBuf],
    input_path: &std::path::Path,
) -> Result<(), String> {
    for (i, cli_name) in names.iter().enumerate() {
        // 替换 '\0' 为实际的空字节
        let name = cli_name.replace("\\0", "\0");
        let embedded_file = embedded
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| format!("Failed to find embedded file: {}", name))?;

        // 输出文件路径
        let output_path = if let Some(output_file) = output_files.get(i) {
            output_file.clone()
        } else {
            let mut path = input_path.to_path_buf();
            path.set_file_name(&embedded_file.name);
            path
        };

        let mut output = tokio::fs::File::create(&output_path).await.map_err(|e| {
            format!(
                "Failed to create output file {}: {}",
                output_path.display(),
                e
            )
        })?;

        let mut data = file
            .range_reader(embedded_file.offset, embedded_file.size)
            .map_err(|e| format!("Failed to read embedded file: {}", e))?;

        tokio::io::copy(&mut data, &mut output)
            .await
            .map_err(|e| format!("Failed to write embedded file: {}", e))?;

        println!(
            "Extracted file: {} ({})",
            embedded_file.name,
            output_path.display()
        );
    }

    Ok(())
}

// 实现通过metadata name提取
async fn extract_by_meta_name(
    file: &AsyncMmapFile,
    meta_names: &[String],
    output_files: &[std::path::PathBuf],
    metadata: &RepoMetadata,
    input_path: &std::path::Path,
) -> Result<(), String> {
    let embedded = get_embedded(file).await.map_err(|e| e.to_string())?;
    let hash_to_name = build_hash_to_name_map(metadata);

    // 构建name到hash的反向映射
    let mut name_to_hash = HashMap::new();
    for (hash, name) in hash_to_name {
        name_to_hash.insert(name, hash);
    }

    for (i, meta_name) in meta_names.iter().enumerate() {
        let hash = name_to_hash
            .get(meta_name)
            .ok_or_else(|| format!("File not found in metadata: {}", meta_name))?;

        let embedded_file = embedded
            .iter()
            .find(|f| f.name == *hash)
            .ok_or_else(|| format!("Failed to find embedded file with hash: {}", hash))?;

        // 输出文件路径
        let output_path = if let Some(output_file) = output_files.get(i) {
            output_file.clone()
        } else {
            let mut path = input_path.to_path_buf();
            path.set_file_name(meta_name);
            path
        };

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
        }

        let mut output = tokio::fs::File::create(&output_path).await.map_err(|e| {
            format!(
                "Failed to create output file {}: {}",
                output_path.display(),
                e
            )
        })?;

        let mut data = file
            .range_reader(embedded_file.offset, embedded_file.size)
            .map_err(|e| format!("Failed to read embedded file: {}", e))?;

        tokio::io::copy(&mut data, &mut output)
            .await
            .map_err(|e| format!("Failed to write embedded file: {}", e))?;

        println!("Extracted file: {} -> {}", meta_name, output_path.display());
    }

    Ok(())
}

// 实现提取所有文件
async fn extract_all_files(
    file: &AsyncMmapFile,
    output_dir: &Path,
    metadata: Option<&RepoMetadata>,
) -> Result<(), String> {
    let embedded = get_embedded(file).await.map_err(|e| e.to_string())?;

    // 确保输出目录存在
    tokio::fs::create_dir_all(output_dir).await.map_err(|e| {
        format!(
            "Failed to create output directory {}: {}",
            output_dir.display(),
            e
        )
    })?;

    let hash_to_name = if let Some(meta) = metadata {
        build_hash_to_name_map(meta)
    } else {
        HashMap::new()
    };

    for embedded_file in embedded {
        // 跳过内部文件
        if embedded_file.name.starts_with('\0') {
            continue;
        }

        // 确定输出文件名和路径
        let file_name = if let Some(meta_name) = hash_to_name.get(&embedded_file.name) {
            meta_name.clone()
        } else {
            embedded_file.name.clone()
        };

        let output_path = output_dir.join(&file_name);

        // 确保父目录存在
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
        }

        let mut output = tokio::fs::File::create(&output_path).await.map_err(|e| {
            format!(
                "Failed to create output file {}: {}",
                output_path.display(),
                e
            )
        })?;

        let mut data = file
            .range_reader(embedded_file.offset, embedded_file.size)
            .map_err(|e| format!("Failed to read embedded file: {}", e))?;

        tokio::io::copy(&mut data, &mut output)
            .await
            .map_err(|e| format!("Failed to write embedded file: {}", e))?;

        println!("Extracted file: {} -> {}", file_name, output_path.display());
    }

    Ok(())
}

pub async fn extract_cli(args: ExtractArgs) {
    // 参数验证
    if let Err(err) = validate_args(&args) {
        eprintln!("Error: {}", err);
        return;
    }

    // 打开文件
    let mmap = match AsyncMmapFile::open(args.input.clone()).await {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Failed to open input file {}: {}", args.input.display(), e);
            return;
        }
    };

    // 根据参数选择功能
    let result = if args.list {
        list_files(&mmap).await
    } else if let Some(output_dir) = args.all {
        let metadata = match parse_metadata(&mmap).await {
            Ok(meta) => meta,
            Err(e) => {
                eprintln!("Failed to parse metadata: {}", e);
                return;
            }
        };
        extract_all_files(&mmap, &output_dir, metadata.as_ref()).await
    } else if !args.meta_name.is_empty() {
        let metadata = match parse_metadata(&mmap).await {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                eprintln!("No metadata found for meta-name extraction");
                return;
            }
            Err(e) => {
                eprintln!("Failed to parse metadata: {}", e);
                return;
            }
        };
        extract_by_meta_name(&mmap, &args.meta_name, &args.file, &metadata, &args.input).await
    } else {
        // 原有功能保持不变
        let embedded = match get_embedded(&mmap).await {
            Ok(emb) => emb,
            Err(e) => {
                eprintln!("Failed to get embedded files: {}", e);
                return;
            }
        };
        extract_by_hash_name(&mmap, &embedded, &args.name, &args.file, &args.input).await
    };

    if let Err(err) = result {
        eprintln!("Extraction failed: {}", err);
    }
}
