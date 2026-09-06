use std::collections::{HashMap, HashSet};
use std::path::Path;

use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};

use crate::{
    cli::ExtractArgs,
    local::{get_embedded, preferred_file_hash, Embedded},
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
    Index,
    File,
    Patch,
}

impl std::fmt::Display for FileType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileType::Config => write!(f, "CONFIG"),
            FileType::Image => write!(f, "IMAGE"),
            FileType::Meta => write!(f, "META"),
            FileType::Index => write!(f, "INDEX"),
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

    if feature_count == 0 {
        return Err("Specify one of --list, --all, --name, or --meta-name".to_string());
    }
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
        "\0INDEX" => FileType::Index,
        name if name.contains('_') && !name.starts_with('\0') => FileType::Patch,
        _ => FileType::File,
    }
}

/// hash → 所有声明该数据块的目标路径。相同内容只保存一个数据块，所以一个
/// hash 可以对应多个安装路径；补丁块不进这张表，避免占用完整文件的路径。
fn build_hash_to_names_map(metadata: &RepoMetadata) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for file in &metadata.hashed {
        if let Some(hash) = preferred_file_hash(&file.md5, &file.xxh) {
            let names = map.entry(hash.clone()).or_default();
            if !names.contains(&file.file_name) {
                names.push(file.file_name.clone());
            }
        }
    }
    map
}

/// 补丁块名 → 文件名，仅用于 `--list` 展示；导出路径规划不使用它。
fn build_patch_display_map(metadata: &RepoMetadata) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for patch in &metadata.patches {
        let from_hash = preferred_file_hash(&patch.from.md5, &patch.from.xxh);
        let to_hash = preferred_file_hash(&patch.to.md5, &patch.to.xxh);
        if let (Some(from), Some(to)) = (from_hash, to_hash) {
            map.insert(format!("{from}_{to}"), patch.file_name.clone());
        }
    }
    map
}

/// name → hash，供 `--meta-name` 反查。完整文件优先于补丁块：同名时
/// `--meta-name` 应还原完整数据而不是补丁内容。
fn build_name_to_hash_map(metadata: &RepoMetadata) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for file in &metadata.hashed {
        if let Some(hash) = preferred_file_hash(&file.md5, &file.xxh) {
            map.entry(file.file_name.clone())
                .or_insert_with(|| hash.clone());
        }
    }
    for patch in &metadata.patches {
        let from_hash = preferred_file_hash(&patch.from.md5, &patch.from.xxh);
        let to_hash = preferred_file_hash(&patch.to.md5, &patch.to.xxh);
        if let (Some(from), Some(to)) = (from_hash, to_hash) {
            map.entry(patch.file_name.clone())
                .or_insert_with(|| format!("{from}_{to}"));
        }
    }
    map
}

// 收集文件信息
async fn collect_file_info(file: &AsyncMmapFile) -> Result<Vec<FileInfo>, String> {
    let embedded = get_embedded(file).await.map_err(|e| e.to_string())?;
    let metadata = parse_metadata(file).await?;

    let mut file_infos = Vec::new();

    let hash_to_names = metadata.as_ref().map(build_hash_to_names_map);
    let patch_names = metadata.as_ref().map(build_patch_display_map);

    for emb in embedded {
        let file_type = classify_file_type(&emb.name);
        let metadata_name = hash_to_names
            .as_ref()
            .and_then(|m| m.get(&emb.name))
            .and_then(|names| names.first().cloned())
            .or_else(|| patch_names.as_ref().and_then(|m| m.get(&emb.name)).cloned());

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
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
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
            path.set_file_name(sanitize_output_name(&embedded_file.name));
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
    let name_to_hash = build_name_to_hash_map(metadata);

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

/// 归档 metadata 派生的相对路径只允许 Normal 组件（子目录合法）；`..`、
/// 绝对路径、盘符、UNC 一律拒绝，防止包内路径把文件写到输出根之外。
fn relative_under_root(root: &Path, relative: &str) -> Result<std::path::PathBuf, String> {
    let mut path = root.to_path_buf();
    let mut normal_count = 0;
    for component in Path::new(relative).components() {
        match component {
            std::path::Component::Normal(part) => {
                path.push(part);
                normal_count += 1;
            }
            _ => return Err(format!("unsafe path in archive: {relative:?}")),
        }
    }
    if normal_count == 0 {
        return Err(format!("empty path in archive: {relative:?}"));
    }
    Ok(path)
}

/// 解析 symlink/junction 后仍须位于 root 内。从已 canonicalize 的 root 逐
/// 组件向下走，已存在的部分就地 canonicalize 并检查前缀；不存在的部分只
/// 可能挂在已验证位于 root 内的父目录下，由后续 create 落盘。
async fn verify_within_root(root: &Path, path: &Path) -> Result<(), String> {
    let root_canonical = tokio::fs::canonicalize(root).await.map_err(|e| {
        format!(
            "Failed to resolve output directory {}: {}",
            root.display(),
            e
        )
    })?;
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "path {:?} is not under {:?}",
            path.display(),
            root.display()
        )
    })?;
    let mut current = root_canonical.clone();
    for component in relative.components() {
        current.push(component);
        if let Ok(resolved) = tokio::fs::canonicalize(&current).await {
            if !resolved.starts_with(&root_canonical) {
                return Err(format!(
                    "archive path escapes output directory: {} -> {}",
                    path.display(),
                    resolved.display()
                ));
            }
            current = resolved;
        }
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

    let hash_to_names = metadata.map(build_hash_to_names_map);
    let claimed: HashSet<&str> = hash_to_names
        .as_ref()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();

    // 先完成全部路径解析与安全校验，再落盘：任何一条越界即整体失败，
    // 不留下半次提取。
    let mut plan: Vec<(String, std::path::PathBuf, &Embedded)> = Vec::new();
    let mut planned_paths: HashSet<String> = HashSet::new();

    // 以 metadata.hashed 的文件路径为主：一个数据块可服务多个目标路径
    // （pack 对相同内容去重存储），补丁块不会占用完整文件的输出路径。
    if let Some(hash_to_names) = hash_to_names.as_ref() {
        for (block_name, names) in hash_to_names {
            let Some(embedded_file) = embedded.iter().find(|e| &e.name == block_name) else {
                for file_name in names {
                    println!("Skipped (block not packed): {file_name}");
                }
                continue;
            };
            for file_name in names {
                let output_path = relative_under_root(output_dir, file_name)?;
                verify_within_root(output_dir, &output_path).await?;
                if planned_paths.insert(output_path.to_string_lossy().into_owned()) {
                    plan.push((file_name.clone(), output_path, embedded_file));
                }
            }
        }
    }

    // 未被 hashed 引用的数据块（追加的资源、补丁块）按自身名称导出。
    for embedded_file in &embedded {
        // 跳过内部文件
        if embedded_file.name.starts_with('\0') {
            continue;
        }
        if claimed.contains(embedded_file.name.as_str()) {
            continue;
        }
        let output_path = relative_under_root(output_dir, &embedded_file.name)?;
        verify_within_root(output_dir, &output_path).await?;
        if planned_paths.insert(output_path.to_string_lossy().into_owned()) {
            plan.push((embedded_file.name.clone(), output_path, embedded_file));
        }
    }

    for (file_name, output_path, embedded_file) in plan {
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

fn sanitize_output_name(name: &str) -> String {
    let name = name.replace('\0', "_");
    if name.is_empty() {
        "_unnamed".to_string()
    } else {
        name
    }
}

pub async fn extract_cli(args: ExtractArgs) -> Result<(), String> {
    validate_args(&args)?;

    let mmap = AsyncMmapFile::open(args.input.clone())
        .await
        .map_err(|e| format!("Failed to open input file {}: {}", args.input.display(), e))?;

    if args.list {
        return list_files(&mmap).await;
    }
    if let Some(output_dir) = args.all {
        let metadata = parse_metadata(&mmap).await?;
        return extract_all_files(&mmap, &output_dir, metadata.as_ref()).await;
    }
    if !args.meta_name.is_empty() {
        let metadata = parse_metadata(&mmap)
            .await?
            .ok_or_else(|| "No metadata found for meta-name extraction".to_string())?;
        return extract_by_meta_name(&mmap, &args.meta_name, &args.file, &metadata, &args.input)
            .await;
    }
    let embedded = get_embedded(&mmap).await.map_err(|e| e.to_string())?;
    extract_by_hash_name(&mmap, &embedded, &args.name, &args.file, &args.input).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ExtractArgs;
    use crate::utils::metadata::{FileMeta, PatchInfo, PatchSide, RepoMetadata};
    use std::path::PathBuf;

    fn extract_args() -> ExtractArgs {
        ExtractArgs {
            input: PathBuf::from("pkg.exe"),
            file: vec![],
            name: vec![],
            meta_name: vec![],
            all: None,
            list: false,
        }
    }

    #[test]
    fn validate_requires_exactly_one_mode() {
        assert!(validate_args(&extract_args()).is_err());

        let mut args = extract_args();
        args.list = true;
        assert!(validate_args(&args).is_ok());

        args.name = vec!["abc".into()];
        assert!(validate_args(&args).is_err());
    }

    #[test]
    fn hash_maps_prefer_md5_like_pack() {
        let metadata = RepoMetadata {
            repo_name: "r".into(),
            tag_name: "t".into(),
            hashed: vec![
                FileMeta {
                    file_name: "app.exe".into(),
                    size: 1,
                    md5: Some("md5hash".into()),
                    xxh: Some("xxhhash".into()),
                    installer: None,
                },
                FileMeta {
                    file_name: "two/app.exe".into(),
                    size: 1,
                    md5: Some("md5hash".into()),
                    xxh: Some("xxhhash".into()),
                    installer: None,
                },
            ],
            patches: vec![PatchInfo {
                file_name: "app.exe".into(),
                size: 1,
                from: PatchSide {
                    size: 1,
                    md5: Some("frommd5".into()),
                    xxh: Some("fromxxh".into()),
                },
                to: PatchSide {
                    size: 1,
                    md5: Some("tomd5".into()),
                    xxh: Some("tomd5xxh".into()),
                },
            }],
            installer: None,
            deletes: Vec::new(),
            packing_info: Vec::new(),
        };
        // 一个数据块映射到多个目标路径；补丁块不占用完整文件路径
        let names = build_hash_to_names_map(&metadata);
        assert_eq!(
            names.get("md5hash").map(Vec::as_slice),
            Some(&["app.exe".to_string(), "two/app.exe".to_string()][..])
        );
        assert!(!names.contains_key("xxhhash"));
        assert!(!names.contains_key("frommd5_tomd5"));

        // --meta-name 反查时完整文件优先于补丁块
        let name_to_hash = build_name_to_hash_map(&metadata);
        assert_eq!(
            name_to_hash.get("app.exe").map(String::as_str),
            Some("md5hash")
        );

        let patch_display = build_patch_display_map(&metadata);
        assert_eq!(
            patch_display.get("frommd5_tomd5").map(String::as_str),
            Some("app.exe")
        );
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_string("short", 32), "short");
        let long = "中文文件名很长很长很长很长很长很长很长";
        let truncated = truncate_string(long, 17);
        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), 17);
    }

    #[test]
    fn classify_internal_and_patch_names() {
        assert!(matches!(classify_file_type("\0INDEX"), FileType::Index));
        assert!(matches!(classify_file_type("aaa_bbb"), FileType::Patch));
        assert!(matches!(classify_file_type("deadbeef"), FileType::File));
    }

    #[test]
    fn sanitize_null_internal_names() {
        assert_eq!(sanitize_output_name("\0CONFIG"), "_CONFIG");
    }

    #[test]
    fn relative_paths_stay_under_root() {
        let root = Path::new("out");
        assert!(relative_under_root(root, "app.exe").is_ok());
        assert!(relative_under_root(root, "User/settings.json").is_ok());

        for evil in [
            "../outside.txt",
            "a/../../outside.txt",
            "..\\outside.txt",
            "C:\\Windows\\evil.exe",
            "\\\\server\\share\\evil.exe",
            "/abs/path.txt",
            "",
        ] {
            assert!(
                relative_under_root(root, evil).is_err(),
                "must reject {evil:?}"
            );
        }
    }
}
