use std::path::Path;

/// 用 editpe（xytoki fork，含 compaction）替换 PE 资源中的版本字符串与图标。
/// 与 rcedit 语义一致：图标是替换而非追加，重写后文件体积变化等于真实增量。
///
/// - 可执行文件没有版本资源时跳过版本字符串（rcedit 在此场景是成功 no-op）。
/// - `icon_required=false` 时图标失败仅警告并保留原图标（pack 的历史行为）；
///   `true` 时图标失败整体报错（replace-bin 的历史行为）。
pub fn update_pe_resources(
    path: &Path,
    strings: &[(&str, &str)],
    icon: Option<&Path>,
    icon_required: bool,
) -> Result<(), String> {
    let path_str = path.to_str().ok_or_else(|| "non-utf8 path".to_string())?;
    let mut image =
        editpe::Image::parse_file(path_str).map_err(|e| format!("editpe parse {path_str}: {e:?}"))?;
    let mut dir = image
        .resource_directory()
        .cloned()
        .ok_or_else(|| "executable has no resource directory".to_string())?;

    if !strings.is_empty() {
        match dir.get_version_info() {
            Ok(Some(mut vi)) => {
                for table in vi.strings.iter_mut() {
                    for (key, value) in strings {
                        table.strings.insert((*key).into(), (*value).into());
                    }
                }
                dir.set_version_info(&vi)
                    .map_err(|e| format!("editpe set version info: {e:?}"))?;
            }
            Ok(None) => {
                eprintln!("Warning: executable has no version info, skipping version strings");
            }
            Err(e) => return Err(format!("editpe read version info: {e:?}")),
        }
    }

    if let Some(icon) = icon {
        // 图标步骤失败且非必需时回滚到图标操作前的资源树：
        // 否则 remove_main_icon 成功而 set 失败会把图标整个弄丢，比保留原图标更糟。
        let backup = dir.clone();
        let result = (|| -> Result<(), String> {
            let icon_bytes = std::fs::read(icon)
                .map_err(|e| format!("read icon {}: {e}", icon.display()))?;
            // 替换而非追加：editpe 的 set_main_icon 语义是新增一组图标，
            // 先移除旧图标组（与 rcedit/WinAPI 的替换语义一致）。
            dir.remove_main_icon()
                .map_err(|e| format!("editpe remove main icon: {e:?}"))?;
            dir.set_main_icon(icon_bytes)
                .map_err(|e| format!("editpe set main icon {}: {e:?}", icon.display()))?;
            Ok(())
        })();
        if let Err(e) = result {
            if icon_required {
                return Err(e);
            }
            dir = backup;
            eprintln!("Warning: failed to set icon: {e}");
        }
    }

    image
        .set_resource_directory(dir)
        .map_err(|e| format!("editpe write resources: {e:?}"))?;
    image
        .write_file(path_str)
        .map_err(|e| format!("editpe write {path_str}: {e:?}"))?;
    Ok(())
}
