use anyhow::anyhow;

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

pub fn plugin_not_found(name: &str) -> anyhow::Error {
    user(format!("Plugin \"{name}\" not found"))
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
        Some("连接下载服务器超时，请检查你的网络连接或更换下载源")
    } else if check.contains("connection refused") {
        Some("下载服务器出现问题，请重试或更换下载源")
    } else if check.contains("connection reset") {
        Some("连接下载服务器失败，请重试或更换下载源")
    } else if check.contains("too_slow") || check.contains("stalled") {
        Some("检测到下载速度异常，请检查你的网络连接或更换下载源")
    } else {
        None
    };
    match (friendly, first_url) {
        (Some(msg), Some(url)) => format!("{msg}\n\n原始错误：{without_url}\n\n下载服务器：{url}"),
        (Some(msg), None) => format!("{msg}\n\n原始错误：{without_url}"),
        (None, Some(url)) => format!("{without_url}\n\n下载服务器：{url}"),
        (None, None) => without_url,
    }
}

pub fn file_release(file_name: &str, err: &impl std::fmt::Display) -> anyhow::Error {
    anyhow!("释放文件 {file_name} 失败：\n{}", friendly(err))
}
