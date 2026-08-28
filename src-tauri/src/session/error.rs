use anyhow::anyhow;

/// fail counter 的粗分类维度，与错误上报过滤共用（见 docs/notes 遥测通道职责收敛）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    Network,
    Disk,
    Permission,
    Hash,
    Cancelled,
    Other,
}

impl FailKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailKind::Network => "network",
            FailKind::Disk => "disk",
            FailKind::Permission => "permission",
            FailKind::Hash => "hash",
            FailKind::Cancelled => "cancelled",
            FailKind::Other => "other",
        }
    }
}

/// 环境/用户错误标记：携带者不进入错误上报后端。
/// 作为 anyhow 链的根错误存在，Display 即用户可见消息，不污染 `{:#}` 输出。
#[derive(Debug)]
pub struct Expected {
    pub kind: FailKind,
    msg: String,
}

impl std::fmt::Display for Expected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Expected {}

/// 分类结果：`kind` 供 fail counter，`report` 供错误上报过滤。
/// 两个消费方共用此函数，避免判断逻辑漂移。
pub struct Classified {
    pub kind: FailKind,
    pub report: bool,
}

pub fn classify(err: &anyhow::Error) -> Classified {
    let mut expected = false;
    let mut kind = None;
    for cause in err.chain() {
        if let Some(mark) = cause.downcast_ref::<Expected>() {
            expected = true;
            if kind.is_none() && mark.kind != FailKind::Other {
                kind = Some(mark.kind);
            }
            continue;
        }
        if kind.is_none() {
            kind = kind_from_cause(cause);
        }
    }
    let kind = kind
        .or_else(|| kind_from_text(&format!("{err:#}")))
        .unwrap_or(FailKind::Other);
    Classified {
        kind,
        report: !expected,
    }
}

fn kind_from_cause(cause: &(dyn std::error::Error + 'static)) -> Option<FailKind> {
    if let Some(io) = cause.downcast_ref::<std::io::Error>() {
        use std::io::ErrorKind as K;
        return Some(match io.kind() {
            K::PermissionDenied => FailKind::Permission,
            K::TimedOut
            | K::ConnectionRefused
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::NotConnected
            | K::UnexpectedEof => FailKind::Network,
            _ => FailKind::Disk,
        });
    }
    if cause.downcast_ref::<reqwest::Error>().is_some() {
        return Some(FailKind::Network);
    }
    None
}

/// 从错误文本推导分类。只匹配仓库内常量短码与用户可见消息常量，不匹配自由文本。
fn kind_from_text(text: &str) -> Option<FailKind> {
    if text.contains("HASH_MISMATCH_ERR") || text.contains(PKG_BROKEN) {
        return Some(FailKind::Hash);
    }
    if text.contains(crate::utils::error::DOWNLOAD_STALLED)
        || text.contains(crate::utils::error::DOWNLOAD_TOO_SLOW)
        || text.contains(META_FAILED)
        || text.contains(NO_DOWNLOAD_NODE)
        || text.contains(DFS2_SESSION)
        || text.contains(FILE_MISSING)
    {
        return Some(FailKind::Network);
    }
    if text.contains("cancelled") {
        return Some(FailKind::Cancelled);
    }
    if text.contains("Access is denied")
        || text.contains("拒绝访问")
        || text.contains("(os error 5)")
    {
        return Some(FailKind::Permission);
    }
    if text.contains(TEMP_DIR) {
        return Some(FailKind::Disk);
    }
    None
}

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

pub fn plugin_not_found(name: &str) -> anyhow::Error {
    user(format!("Plugin \"{name}\" not found"))
}

pub fn user(msg: impl Into<String>) -> anyhow::Error {
    expected(FailKind::Other, msg)
}

pub fn expected(kind: FailKind, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Expected {
        kind,
        msg: msg.into(),
    })
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

/// 下载/释放失败属环境错误，携带 Expected 标记。
pub fn file_release(file_name: &str, err: &impl std::fmt::Display) -> anyhow::Error {
    let raw = err.to_string();
    let check = raw.to_lowercase();
    let kind = kind_from_text(&raw).unwrap_or(
        if check.contains("timed out")
            || check.contains("connection")
            || check.contains("too_slow")
            || check.contains("stalled")
        {
            FailKind::Network
        } else {
            FailKind::Other
        },
    );
    expected(
        kind,
        format!("释放文件 {file_name} 失败：\n{}", friendly(err)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_error_is_expected_and_display_clean() {
        let err = user(META_FAILED).context("outer context");
        let c = classify(&err);
        assert!(!c.report);
        assert_eq!(c.kind, FailKind::Network);
        assert_eq!(format!("{err:#}"), format!("outer context: {META_FAILED}"));
    }

    #[test]
    fn hide_error_is_reported() {
        let err = hide(DFS2_SESSION, "invalid session response format");
        let c = classify(&err);
        assert!(c.report);
        assert_eq!(c.kind, FailKind::Network);
    }

    #[test]
    fn io_permission_denied_maps_to_permission() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let err = anyhow::Error::new(io).context("write file");
        let c = classify(&err);
        assert!(c.report);
        assert_eq!(c.kind, FailKind::Permission);
    }

    #[test]
    fn io_disk_full_maps_to_disk() {
        let io = std::io::Error::from(std::io::ErrorKind::StorageFull);
        let err = anyhow::Error::new(io);
        assert_eq!(classify(&err).kind, FailKind::Disk);
    }

    #[test]
    fn hash_mismatch_context_maps_to_hash() {
        let err = anyhow!("File a hash mismatch").context("HASH_MISMATCH_ERR");
        assert_eq!(classify(&err).kind, FailKind::Hash);
    }

    #[test]
    fn cancelled_text_maps_to_cancelled() {
        let err = anyhow!("merged fallback cancelled");
        assert_eq!(classify(&err).kind, FailKind::Cancelled);
    }

    #[test]
    fn file_release_download_error_is_expected_network() {
        let err = file_release("a.dll", &"error sending request: operation timed out");
        let c = classify(&err);
        assert!(!c.report);
        assert_eq!(c.kind, FailKind::Network);
    }

    #[test]
    fn plain_error_maps_to_other_and_reports() {
        let err = anyhow!("ipc protocol violation");
        let c = classify(&err);
        assert!(c.report);
        assert_eq!(c.kind, FailKind::Other);
    }
}
