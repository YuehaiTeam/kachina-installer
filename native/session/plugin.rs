use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use lazy_static::lazy_static;

use crate::dfs::http_get_request;
use crate::utils::code::{attach_metadata, Attach, VERSION_REGEX_INVALID};

struct UrlCache {
    resolved_url: String,
    expiry_ms: u128,
}

struct VersionCache {
    resolved_url: String,
}

lazy_static! {
    static ref URL_CACHE: Mutex<HashMap<String, UrlCache>> = Mutex::new(HashMap::new());
    static ref VERSION_CACHE: Mutex<HashMap<String, VersionCache>> = Mutex::new(HashMap::new());
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn is_github_source(source: &str) -> bool {
    let clean = clean_plugin_url(source);
    clean.contains("github.com") && clean.contains("${version}")
}

pub fn forced_plugin_name(source: &str) -> Option<String> {
    let protocol = source.find("://")?;
    let before = &source[..protocol];
    let idx = before.find("plugin-")?;
    let name = before[idx + "plugin-".len()..].split('+').next()?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

pub fn clean_plugin_url(source: &str) -> String {
    let Some(protocol) = source.find("://") else {
        return source.to_string();
    };
    let before = &source[..protocol];
    let after = &source[protocol..];
    if let Some((_, name_and_rest)) = before.split_once("plugin-") {
        if let Some((_, remaining)) = name_and_rest.split_once('+') {
            if remaining.is_empty() {
                return format!("https{after}");
            }
            return format!("{remaining}{after}");
        }
        return format!("https{after}");
    }
    if let Some(idx) = before.rfind('+') {
        return format!("{}{after}", &before[idx + 1..]);
    }
    source.to_string()
}

pub async fn resolve_github_file_url(source: &str) -> anyhow::Result<String> {
    let clean = clean_plugin_url(source);
    let parsed = parse_github_url(&clean)?;
    let cached = VERSION_CACHE
        .lock()
        .unwrap()
        .get(&parsed.cache_key)
        .map(|v| v.resolved_url.clone());
    let resolved = if let Some(url) = cached {
        url
    } else {
        let version =
            resolve_version(&parsed.releases_latest_url, parsed.version_regex.as_deref()).await?;
        let resolved = parsed.base_url.replace("${version}", &version);
        VERSION_CACHE.lock().unwrap().insert(
            parsed.cache_key.clone(),
            VersionCache {
                resolved_url: resolved.clone(),
            },
        );
        resolved
    };
    if parsed.should_cache {
        resolve_direct_url(&resolved, parsed.cache_time).await
    } else {
        Ok(resolved)
    }
}

struct GitHubUrl {
    base_url: String,
    version_regex: Option<String>,
    releases_latest_url: String,
    cache_key: String,
    should_cache: bool,
    cache_time: Option<u64>,
}

fn parse_github_url(url: &str) -> anyhow::Result<GitHubUrl> {
    let (base_url, params) = url
        .split_once('#')
        .map(|(b, p)| (b.to_string(), Some(p)))
        .unwrap_or_else(|| (url.to_string(), None));
    let mut version_regex = None;
    let mut cache_time = None;
    if let Some(params) = params {
        let parsed = url::Url::parse(&format!("https://dummy.local/?{params}"))
            .map_err(|e| attach_metadata(e.into()))?;
        for (k, v) in parsed.query_pairs() {
            match k.as_ref() {
                "versionRegex" => version_regex = Some(v.into_owned()),
                "cacheTime" => {
                    if let Ok(n) = v.parse::<u64>() {
                        if n > 0 {
                            cache_time = Some(n);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let releases_index = base_url.find("/releases/").ok_or_else(|| {
        anyhow::anyhow!("URL must contain /releases/").attach(crate::utils::code::SOURCE_INVALID)
    })?;
    let releases_prefix = format!("{}{}", &base_url[..releases_index], "/releases");
    let releases_latest_url = format!("{releases_prefix}/latest");
    owner_repo_from_releases_url(&base_url).ok_or_else(|| {
        anyhow::anyhow!("Invalid releases URL format").attach(crate::utils::code::SOURCE_INVALID)
    })?;
    let host_ok = url::Url::parse(&base_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "github.com"))
        .unwrap_or(false);
    let cache_key = format!("{}#{}", base_url, version_regex.as_deref().unwrap_or(""));
    Ok(GitHubUrl {
        base_url,
        version_regex,
        releases_latest_url,
        cache_key,
        should_cache: host_ok || cache_time.is_some(),
        cache_time,
    })
}

async fn resolve_version(
    releases_latest_url: &str,
    version_regex: Option<&str>,
) -> anyhow::Result<String> {
    let response = http_get_request(releases_latest_url.to_string(), Some(true), None, None)
        .await
        .map_err(attach_metadata)?;
    let redirect = if !response.final_url.is_empty() {
        response.final_url
    } else {
        response
            .headers
            .get("location")
            .cloned()
            .unwrap_or_default()
    };
    if redirect.is_empty() {
        return Err(
            anyhow::anyhow!("No redirect found for GitHub latest release")
                .attach(crate::utils::code::METADATA_UNREACHABLE),
        );
    }
    if let Some(custom) = version_regex {
        return capture_version(custom, &redirect);
    }
    tag_from_releases_redirect(&redirect).ok_or_else(|| {
        anyhow::anyhow!("Failed to extract tag from {redirect}")
            .attach(crate::utils::code::METADATA_INVALID)
    })
}

fn owner_repo_from_releases_url(url: &str) -> Option<(String, String)> {
    let idx = url.find("/releases")?;
    let mut parts = url[..idx].rsplit('/');
    let repo = parts.next()?.to_string();
    let owner = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

fn tag_from_releases_redirect(url: &str) -> Option<String> {
    const MARKER: &str = "/releases/tag/";
    let start = url.find(MARKER)? + MARKER.len();
    let rest = &url[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let tag = &rest[..end];
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

/// Extract version via plugin `versionRegex`. Group 1 wins; a pattern
/// without groups falls back to the whole match.
fn capture_version(pattern: &str, text: &str) -> anyhow::Result<String> {
    let re = regex_lite::Regex::new(pattern).map_err(|e| {
        anyhow::anyhow!("Invalid versionRegex {pattern:?}: {e}").attach(VERSION_REGEX_INVALID)
    })?;
    let caps = re.captures(text).ok_or_else(|| {
        anyhow::anyhow!("versionRegex {pattern:?} did not match {text}")
            .attach(crate::utils::code::METADATA_INVALID)
    })?;
    let m = caps.get(1).unwrap_or_else(|| caps.get(0).unwrap());
    if m.is_empty() {
        return Err(
            anyhow::anyhow!("versionRegex {pattern:?} matched empty version in {text}")
                .attach(crate::utils::code::METADATA_INVALID),
        );
    }
    Ok(m.as_str().to_string())
}

async fn resolve_direct_url(original_url: &str, cache_time: Option<u64>) -> anyhow::Result<String> {
    let now = now_ms();
    {
        let mut cache = URL_CACHE.lock().unwrap();
        cache.retain(|_, v| v.expiry_ms > now);
        if let Some(hit) = cache.get(original_url) {
            return Ok(hit.resolved_url.clone());
        }
    }
    // probe with redirects off; the response is dropped without consuming the
    // body so a direct 200 on the package itself does not download it whole
    let client = reqwest::ClientBuilder::new()
        .user_agent(crate::capabilities::ua_string())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| attach_metadata(e.into()))?;
    let response = client
        .get(original_url)
        .send()
        .await
        .map_err(|e| attach_metadata(e.into()))?;
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let redirect = match location {
        Some(loc) => match url::Url::parse(original_url).and_then(|base| base.join(&loc)) {
            Ok(joined) => joined.to_string(),
            Err(_) => return Ok(original_url.to_string()),
        },
        None => original_url.to_string(),
    };
    let expiry_ms = if let Some(secs) = cache_time {
        now + (secs as u128) * 1000
    } else {
        expiry_from_url(&redirect).unwrap_or(now + 300_000)
    };
    URL_CACHE.lock().unwrap().insert(
        original_url.to_string(),
        UrlCache {
            resolved_url: redirect.clone(),
            expiry_ms,
        },
    );
    Ok(redirect)
}

fn expiry_from_url(url: &str) -> Option<u128> {
    let parsed = url::Url::parse(url).ok()?;
    for key in ["ske", "se"] {
        if let Some(v) = parsed.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v) {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&v) {
                return Some(dt.timestamp_millis().max(0) as u128);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_repo_from_github_releases_url() {
        let (owner, repo) = owner_repo_from_releases_url(
            "https://github.com/YuehaiTeam/app/releases/download/${version}/foo.exe",
        )
        .unwrap();
        assert_eq!(owner, "YuehaiTeam");
        assert_eq!(repo, "app");
    }

    #[test]
    fn tag_from_latest_redirect() {
        assert_eq!(
            tag_from_releases_redirect("https://github.com/YuehaiTeam/app/releases/tag/v1.2.3")
                .as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(
            tag_from_releases_redirect(
                "https://github.com/YuehaiTeam/app/releases/tag/v1.2.3?foo=1#bar"
            )
            .as_deref(),
            Some("v1.2.3")
        );
    }

    #[test]
    fn custom_version_group() {
        assert_eq!(
            capture_version(
                r"/releases/tag/([^/?#]+)",
                "https://github.com/a/b/releases/tag/2.0.0"
            )
            .unwrap(),
            "2.0.0"
        );
        assert_eq!(
            capture_version(r"/releases/tag/v(.+)", "https://x/releases/tag/v3.1.4").unwrap(),
            "3.1.4"
        );
    }

    #[test]
    fn custom_version_full_regex_syntax() {
        assert_eq!(
            capture_version(r"/tag/v?([\d.]+)", "https://x/releases/tag/v3.1.4").unwrap(),
            "3.1.4"
        );
        assert_eq!(
            capture_version(
                r"tag/(?:release-)?(\d+\.\d+\.\d+)",
                "https://x/releases/tag/release-1.2.3"
            )
            .unwrap(),
            "1.2.3"
        );
        // No capture group: whole match is the version.
        assert_eq!(
            capture_version(r"\d+\.\d+\.\d+", "https://x/releases/tag/v3.1.4").unwrap(),
            "3.1.4"
        );
    }

    #[test]
    fn custom_version_bad_pattern_or_no_match_errors() {
        // 详细原因（invalid pattern / no match）are coded METADATA_INVALID / VERSION_REGEX_INVALID.
        assert!(capture_version(r"v([0-9.+", "https://x/releases/tag/v1").is_err());
        assert!(capture_version(r"/zzz/(\d+)", "https://x/releases/tag/v1").is_err());
        assert!(capture_version(r"(x*)", "https://x/releases/tag/v1").is_err());
    }

    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One-shot HTTP server: serves `respond` for each accepted connection,
    /// counting requests. Returns the port and the counter.
    async fn mock_server(
        respond: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> (u16, Arc<std::sync::Mutex<u32>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(std::sync::Mutex::new(0u32));
        let respond = Arc::new(respond);
        tokio::spawn({
            let count = count.clone();
            async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    *count.lock().unwrap() += 1;
                    let respond = respond.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).to_string();
                        let path = req
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();
                        let out = respond(&path);
                        let _ = socket.write_all(out.as_bytes()).await;
                    });
                }
            }
        });
        (port, count)
    }

    #[tokio::test]
    async fn direct_url_caches_200_without_reading_body() {
        let big = "x".repeat(8 * 1024 * 1024);
        let (port, count) = mock_server(move |_path| {
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                big.len() + 1,
                big
            )
        })
        .await;
        let url = format!("http://127.0.0.1:{port}/pkg.exe");
        // the probe returns fast although the body is 8MB: it is never read
        let resolved = resolve_direct_url(&url, Some(60)).await.unwrap();
        assert_eq!(resolved, url);
        assert_eq!(*count.lock().unwrap(), 1);
        // second call is served from URL_CACHE without any further request
        let again = resolve_direct_url(&url, Some(60)).await.unwrap();
        assert_eq!(again, url);
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn direct_url_joins_relative_redirects_keeping_port() {
        let (port, _count) = mock_server(|path| {
            if path == "/a/pkg.exe" {
                "HTTP/1.1 302 Found\r\nlocation: /dl/app.exe\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
            } else {
                "HTTP/1.1 302 Found\r\nlocation: ../dl2/app.exe\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
            }
        })
        .await;
        let root_rel = resolve_direct_url(&format!("http://127.0.0.1:{port}/a/pkg.exe"), Some(60))
            .await
            .unwrap();
        assert_eq!(root_rel, format!("http://127.0.0.1:{port}/dl/app.exe"));
        let path_rel = resolve_direct_url(&format!("http://127.0.0.1:{port}/x/y/pkg.exe"), Some(60))
            .await
            .unwrap();
        assert_eq!(path_rel, format!("http://127.0.0.1:{port}/x/dl2/app.exe"));
    }

    #[test]
    fn mirror_cache_keys_do_not_collide_across_hosts() {
        let a = parse_github_url(
            "http://mirror-a.local:1000/o/r/releases/download/v${version}/app.exe",
        )
        .unwrap();
        let b = parse_github_url(
            "http://mirror-b.local:2000/o/r/releases/download/v${version}/app.exe",
        )
        .unwrap();
        let c = parse_github_url(
            "http://mirror-a.local:1000/o/r/releases/download/v${version}/app.exe#versionRegex=v(.+)",
        )
        .unwrap();
        assert_ne!(a.cache_key, b.cache_key);
        assert_ne!(a.cache_key, c.cache_key);
    }
}
