use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use lazy_static::lazy_static;

use crate::dfs::http_get_request;
use crate::session::error::{self, hide};

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
        let version = resolve_version(&parsed.releases_latest_url, parsed.version_regex.as_deref())
            .await?;
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
            .map_err(|e| hide(error::META_FAILED, e))?;
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
    let releases_index = base_url
        .find("/releases/")
        .ok_or_else(|| hide(error::META_FAILED, "URL must contain /releases/"))?;
    let releases_prefix = format!("{}{}", &base_url[..releases_index], "/releases");
    let releases_latest_url = format!("{releases_prefix}/latest");
    let re = regex::Regex::new(r"/([^/]+)/([^/]+)/releases")
        .map_err(|e| hide(error::META_FAILED, e))?;
    let caps = re
        .captures(&base_url)
        .ok_or_else(|| hide(error::META_FAILED, "Invalid releases URL format"))?;
    let owner = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
    let repo = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
    let host_ok = url::Url::parse(&base_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "github.com"))
        .unwrap_or(false);
    Ok(GitHubUrl {
        base_url,
        version_regex,
        releases_latest_url,
        cache_key: format!("{owner}/{repo}"),
        should_cache: host_ok || cache_time.is_some(),
        cache_time,
    })
}

async fn resolve_version(
    releases_latest_url: &str,
    version_regex: Option<&str>,
) -> anyhow::Result<String> {
    let response = http_get_request(
        releases_latest_url.to_string(),
        Some(true),
        None,
        None,
    )
    .await
    .map_err(|e| hide(error::META_FAILED, e))?;
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
        return Err(hide(error::META_FAILED, "No redirect found for GitHub latest release"));
    }
    if let Some(custom) = version_regex {
        let re = regex::Regex::new(custom).map_err(|e| hide(error::META_FAILED, e))?;
        return re
            .captures(&redirect)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| hide(error::META_FAILED, format!("Failed to extract version from {redirect}")));
    }
    let re = regex::Regex::new(r"/releases/tag/([^/?#]+)")
        .map_err(|e| hide(error::META_FAILED, e))?;
    re.captures(&redirect)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .ok_or_else(|| hide(error::META_FAILED, format!("Failed to extract tag from {redirect}")))
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
    let response = http_get_request(original_url.to_string(), Some(true), None, None)
        .await
        .map_err(|e| hide(error::META_FAILED, e))?;
    let mut redirect = response
        .headers
        .get("location")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or(response.final_url);
    if redirect.is_empty() || redirect == original_url {
        return Ok(original_url.to_string());
    }
    if redirect.starts_with('/') {
        if let Ok(base) = url::Url::parse(original_url) {
            redirect = format!(
                "{}://{}{}",
                base.scheme(),
                base.host_str().unwrap_or(""),
                redirect
            );
        }
    }
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
