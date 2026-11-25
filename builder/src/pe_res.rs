use std::path::Path;

/// 用 editpe（xytoki fork，含 compaction）替换 PE 资源中的版本字符串与图标。
/// 与 rcedit 语义一致：图标是替换而非追加，重写后文件体积变化等于真实增量。
pub fn update_pe_resources(
    path: &Path,
    strings: &[(&str, &str)],
    icon: Option<&Path>,
) -> Result<(), String> {
    let path_str = path.to_str().ok_or_else(|| "non-utf8 path".to_string())?;
    let mut image =
        editpe::Image::parse_file(path_str).map_err(|e| format!("editpe parse {path_str}: {e:?}"))?;
    let mut dir = image
        .resource_directory()
        .cloned()
        .ok_or_else(|| "executable has no resource directory".to_string())?;

    if !strings.is_empty() {
        let mut vi = dir
            .get_version_info()
            .map_err(|e| format!("editpe read version info: {e:?}"))?
            .ok_or_else(|| "executable has no version info".to_string())?;
        for table in vi.strings.iter_mut() {
            for (key, value) in strings {
                table.strings.insert((*key).into(), (*value).into());
            }
        }
        dir.set_version_info(&vi)
            .map_err(|e| format!("editpe set version info: {e:?}"))?;
    }

    if let Some(icon) = icon {
        let icon_bytes = std::fs::read(icon)
            .map_err(|e| format!("read icon {}: {e}", icon.display()))?;
        // 替换而非追加：editpe 的 set_main_icon 语义是新增一组图标，
        // 先移除旧图标组（与 rcedit/WinAPI 的替换语义一致）。
        dir.remove_main_icon()
            .map_err(|e| format!("editpe remove main icon: {e:?}"))?;
        dir.set_main_icon(icon_bytes)
            .map_err(|e| format!("editpe set main icon {}: {e:?}", icon.display()))?;
    }

    image
        .set_resource_directory(dir)
        .map_err(|e| format!("editpe write resources: {e:?}"))?;
    image
        .write_file(path_str)
        .map_err(|e| format!("editpe write {path_str}: {e:?}"))?;
    Ok(())
}
