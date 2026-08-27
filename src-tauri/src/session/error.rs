use super::i18n::tr;

pub const PKG_BROKEN: &str = "安装包损坏，请重新下载";
pub const META_FAILED: &str = "获取更新信息失败，请检查网络连接";
pub const HASH_INVALID: &str = "更新服务端配置有误，不支持的哈希算法";
pub const UNINSTALL_META_MISSING: &str = "未找到卸载配置文件，请重新安装后再卸载";
pub const PATH_INVALID: &str = "安装路径无效";
pub const TEMP_DIR: &str = "无法访问临时文件夹";
pub const NO_DOWNLOAD_NODE: &str = "没有可用的下载节点";
pub const DFS2_SESSION: &str = "创建下载会话失败";
pub const FILE_MISSING: &str = "无法获取所需文件，请检查网络连接或更换下载源";
pub const PLUGIN_NO_UI: &str = "该安装源需要图形界面";
pub const PLUGIN_NEED_WEBVIEW2: &str = "该安装源需要 WebView2";
pub const PLUGIN_HOST_FAILED: &str = "插件宿主启动失败";

// Localized getters. The consts above keep their Chinese values because they
// are also matched against as string markers (and used by the native UI);
// user-facing construction must go through these getters.
pub fn pkg_broken() -> &'static str {
    tr(PKG_BROKEN, "The installer package is corrupted. Please download it again")
}
pub fn meta_failed() -> &'static str {
    tr(
        META_FAILED,
        "Failed to fetch update information. Please check your network connection",
    )
}
pub fn hash_invalid() -> &'static str {
    tr(
        HASH_INVALID,
        "The update server is misconfigured: unsupported hash algorithm",
    )
}
pub fn uninstall_meta_missing() -> &'static str {
    tr(
        UNINSTALL_META_MISSING,
        "Uninstall metadata not found. Please reinstall before uninstalling",
    )
}
pub fn path_invalid() -> &'static str {
    tr(PATH_INVALID, "Invalid install path")
}
pub fn temp_dir() -> &'static str {
    tr(TEMP_DIR, "Cannot access the temporary folder")
}
pub fn no_download_node() -> &'static str {
    tr(NO_DOWNLOAD_NODE, "No available download nodes")
}
pub fn dfs2_session() -> &'static str {
    tr(DFS2_SESSION, "Failed to create download session")
}
pub fn file_missing() -> &'static str {
    tr(
        FILE_MISSING,
        "Failed to fetch required files. Check your network or switch to another download source",
    )
}
pub fn plugin_no_ui() -> &'static str {
    tr(PLUGIN_NO_UI, "This source requires a graphical interface")
}
pub fn plugin_need_webview2() -> &'static str {
    tr(PLUGIN_NEED_WEBVIEW2, "This source requires WebView2")
}
pub fn plugin_host_failed() -> &'static str {
    tr(PLUGIN_HOST_FAILED, "Failed to start the plugin host")
}

pub fn plugin_not_found(name: &str) -> anyhow::Error {
    user(trf!(
        "未找到插件 {name}",
        "Plugin \"{name}\" not found",
        "name" = name
    ))
}

pub fn user(msg: impl Into<String>) -> anyhow::Error {
    anyhow!(msg.into())
}

pub fn hide(user_msg: impl Into<String>, err: impl std::fmt::Display) -> anyhow::Error {
    tracing::error!("{err}");
    anyhow!(user_msg.into())
}

pub fn friendly(err: &impl std::fmt::Display) -> String {
    let err_str = err.to_string();
    let first_url = err_str.match_indices("http").find_map(|(i, _)| {
        let rest = &err_str[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ')' || c == ',')
                .unwrap_or(rest.len());
            Some(rest[..end].to_string())
        } else {
            None
        }
    });
    let without_url = {
        let mut s = err_str.clone();
        if let Some(url) = first_url.as_ref() {
            s = s.replace(url, "[url]");
        }
        s
    };
    let check = without_url.to_lowercase();
    let friendly = if err_str.contains("operation timed out") {
        Some(tr(
            "连接下载服务器超时，请检查你的网络连接或更换下载源",
            "Connection to the download server timed out. Please check your network or switch to another download source",
        ))
    } else if check.contains("connection refused") {
        Some(tr(
            "下载服务器出现问题，请重试或更换下载源",
            "The download server is having problems. Please retry or switch to another download source",
        ))
    } else if check.contains("connection reset") {
        Some(tr(
            "连接下载服务器失败，请重试或更换下载源",
            "Failed to connect to the download server. Please retry or switch to another download source",
        ))
    } else if check.contains("too_slow") || check.contains("stalled") {
        Some(tr(
            "检测到下载速度异常，请检查你的网络连接或更换下载源",
            "Abnormally slow download detected. Please check your network or switch to another download source",
        ))
    } else {
        None
    };
    let original = tr("原始错误：", "Original error: ");
    let server = tr("下载服务器：", "Download server: ");
    match (friendly, first_url) {
        (Some(msg), Some(url)) => {
            format!("{msg}\n\n{original}{without_url}\n\n{server}{url}")
        }
        (Some(msg), None) => format!("{msg}\n\n{original}{without_url}"),
        (None, Some(url)) => format!("{without_url}\n\n{server}{url}"),
        (None, None) => without_url,
    }
}

pub fn file_release(file_name: &str, err: &impl std::fmt::Display) -> anyhow::Error {
    anyhow!(trf!(
        "释放文件 {name} 失败：\n{err}",
        "Failed to release {name}:\n{err}",
        "name" = file_name,
        "err" = friendly(err)
    ))
}
