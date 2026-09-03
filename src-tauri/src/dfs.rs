use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::{
    utils::{error::TAResult, url::HttpContextExt},
    REQUEST_CLIENT,
};

#[derive(Deserialize, Serialize, Debug)]
pub struct DownloadResp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct HttpGetResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub final_url: String,
}

// DFS2 data structures
#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2Metadata {
    pub resource_version: String,
    pub name: String,
    pub data: Option<Dfs2Data>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2Data {
    pub index: std::collections::HashMap<String, Dfs2FileInfo>,
    pub metadata: crate::utils::metadata::RepoMetadata,
    pub installer_end: u32,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2FileInfo {
    pub name: String,
    pub offset: u32,
    pub raw_offset: u32,
    pub size: u32,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2SessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2SessionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2ChunkResponse {
    pub url: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2BatchChunkRequest {
    pub chunks: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2ChunkUrlResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2BatchChunkResponse {
    pub urls: HashMap<String, Dfs2ChunkUrlResult>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct InsightItem {
    pub url: String,
    pub ttfb: u32, // 首字节时间(ms)
    pub time: u32, // 纯下载时间(ms) = 总时间 - TTFB
    pub size: u32, // 实际下载字节数
    pub error: Option<String>,
    #[serde(default)]
    pub range: Vec<(u32, u32)>, // HTTP Range请求范围
    #[serde(default)]
    pub mode: Option<String>, // 安装模式
}

/// Non-2xx answer from a DFS / metadata endpoint. Typed so classifiers can tell
/// "the server answered with an error" from "the server was unreachable"
/// without parsing text; `body` is capped so it can travel as `Coded.detail`.
#[derive(Debug)]
pub struct HttpStatus {
    pub status: u16,
    pub body: String,
}

impl HttpStatus {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        let mut body: String = body.into();
        if body.len() > 512 {
            let mut cut = 512;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
        }
        Self { status, body }
    }
}

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.body.is_empty() {
            write!(f, "{}", self.status)
        } else {
            write!(f, "{}: {}", self.status, self.body)
        }
    }
}

impl std::error::Error for HttpStatus {}

async fn status_error(res: reqwest::Response) -> anyhow::Error {
    let status = res.status().as_u16();
    let body = res.text().await.unwrap_or_default();
    anyhow::Error::new(HttpStatus::new(status, body))
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &str) -> anyhow::Result<T> {
    serde_json::from_str(body).with_context(|| {
        let mut cut = body.len().min(512);
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("parse JSON: {}", &body[..cut])
    })
}

/// Record the error class once: a code already present is not overwritten by
/// a later, less specific one.
pub fn apply_insight_error(insight: &mut InsightItem, err: &anyhow::Error) {
    if insight.error.is_some() {
        return;
    }
    insight.error = Some(crate::utils::code::insight_code(err).to_string());
}

pub fn apply_insight_io_error(insight: &mut InsightItem, err: &std::io::Error) {
    if insight.error.is_some() {
        return;
    }
    insight.error = Some(crate::utils::code::insight_code_for_io(err).to_string());
}

pub fn is_remote_insight_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("http3://")
        || lower.starts_with("h3://")
        || lower.starts_with("h3wt://")
        || lower.starts_with("sftp://")
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2SessionInsights {
    pub servers: Vec<InsightItem>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Dfs2DeleteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insights: Option<Dfs2SessionInsights>,
}

pub async fn get_dfs(
    url: String,
    range: Option<String>,
    extras: Option<String>,
) -> anyhow::Result<DownloadResp> {
    let url_with_range_in_query = if let Some(range) = range {
        format!("{url}?range={range}")
    } else {
        format!("{url}?")
    };
    let extras = if let Some(extras) = extras {
        extras
    } else {
        "".to_string()
    };
    let res = REQUEST_CLIENT
        .post(&url_with_range_in_query)
        .body(extras.clone())
        .send()
        .await
        .with_http_context("get_dfs", &url_with_range_in_query)
        ?;
    // 401 carries the challenge in the body
    if res.status() != reqwest::StatusCode::OK && res.status() != reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(status_error(res).await);
    }
    let body_text = res
        .text()
        .await
        .with_http_context("get_dfs", &url)
        .context("read response body")?;
    let json: DownloadResp = parse_json(&body_text)?;
    // directly return if not challenge
    if json.challenge.is_none() {
        return Ok(json);
    }
    let challenge = json.challenge.unwrap();
    // split challenge into "hash/source"
    let challenge: Vec<&str> = challenge.split('/').collect();
    if challenge.len() != 2 {
        return Err(anyhow!("Invalid challenge"));
    }
    let hash = challenge[0];
    let source = challenge[1];
    let mut solve = "".to_string();
    // loop 1 to 256
    for i in 0..=255 {
        // suffix i in source as hex 2 digits
        let new_src = format!("{source}{i:02x}");
        let new_hash = chksum_md5::hash(new_src.as_bytes()).to_hex_lowercase();
        if hash == new_hash {
            solve = new_src;
            break;
        }
    }
    if solve.is_empty() {
        return Err(anyhow!("Failed to solve challenge"));
    }
    let url = format!("{url_with_range_in_query}&sid={solve}");
    let res = REQUEST_CLIENT
        .post(&url)
        .body(extras)
        .send()
        .await
        .with_http_context("get_dfs", &url)
        ?;
    if res.status() != reqwest::StatusCode::OK && res.status() != reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(status_error(res).await);
    }
    let body_text = res
        .text()
        .await
        .with_http_context("get_dfs", &url)
        .context("read response body")?;
    let json: DownloadResp = parse_json(&body_text)?;
    if json.challenge.is_some() {
        return Err(anyhow!("Challenge not solved"));
    }
    Ok(json)
}

// DFS2 API commands
pub async fn get_dfs2_metadata(api_url: String) -> anyhow::Result<Dfs2Metadata> {
    let url_with_metadata = if api_url.contains('?') {
        format!("{}&with_metadata=1", api_url)
    } else {
        format!("{}?with_metadata=1", api_url)
    };

    let res = REQUEST_CLIENT
        .get(&url_with_metadata)
        .send()
        .await
        .with_http_context("get_dfs2_metadata", &url_with_metadata)
        ?;

    if !res.status().is_success() {
        return Err(status_error(res).await);
    }

    let body_text = res
        .text()
        .await
        .with_http_context("get_dfs2_metadata", &url_with_metadata)
        .context("read response body")?;

    parse_json(&body_text)
}

pub async fn create_dfs2_session(
    api_url: String,
    chunks: Option<Vec<String>>,
    version: Option<String>,
    challenge_response: Option<String>,
    session_id: Option<String>,
    extras: Option<serde_json::Value>,
) -> anyhow::Result<Dfs2SessionResponse> {
    let request_body = Dfs2SessionRequest {
        chunks,
        sid: session_id,
        challenge: challenge_response,
        version,
        extras,
    };

    let res = REQUEST_CLIENT
        .post(&api_url)
        .json(&request_body)
        .send()
        .await
        .with_http_context("create_dfs2_session", &api_url)
        ?;

    let status = res.status();
    let body_text = res
        .text()
        .await
        .with_http_context("create_dfs2_session", &api_url)
        .context("read response body")?;

    tracing::info!("Response body: {}", body_text);

    // 402 carries the challenge in the body
    if !status.is_success() && status != reqwest::StatusCode::PAYMENT_REQUIRED {
        return Err(anyhow::Error::new(HttpStatus::new(status.as_u16(), body_text)));
    }

    parse_json(&body_text)
}

pub async fn get_dfs2_chunk_url(
    session_api_url: String,
    range: String,
) -> anyhow::Result<Dfs2ChunkResponse> {
    let url = format!("{}?range={}", session_api_url, range);

    let res = REQUEST_CLIENT
        .get(&url)
        .send()
        .await
        .with_http_context("get_dfs2_chunk_url", &url)
        ?;

    if !res.status().is_success() {
        return Err(status_error(res).await);
    }

    let body_text = res
        .text()
        .await
        .with_http_context("get_dfs2_chunk_url", &url)
        .context("read response body")?;

    parse_json(&body_text)
}

pub async fn get_dfs2_batch_chunk_urls(
    session_api_url: String,
    chunks: Vec<String>,
) -> anyhow::Result<Dfs2BatchChunkResponse> {
    let request_body = Dfs2BatchChunkRequest { chunks };

    let res = REQUEST_CLIENT
        .post(&session_api_url)
        .json(&request_body)
        .send()
        .await
        .with_http_context("get_dfs2_batch_chunk_urls", &session_api_url)
        ?;

    if !res.status().is_success() {
        return Err(status_error(res).await);
    }

    let body_text = res
        .text()
        .await
        .with_http_context("get_dfs2_batch_chunk_urls", &session_api_url)
        .context("read response body")?;

    parse_json(&body_text)
}

pub async fn end_dfs2_session(
    session_api_url: String,
    insights: Option<Dfs2SessionInsights>,
) -> anyhow::Result<()> {
    let request_body = Dfs2DeleteRequest { insights };

    let res = REQUEST_CLIENT
        .delete(&session_api_url)
        .json(&request_body)
        .send()
        .await
        .with_http_context("end_dfs2_session", &session_api_url)
        ?;

    if !res.status().is_success() {
        return Err(status_error(res).await);
    }
    Ok(())
}

pub async fn solve_dfs2_challenge(challenge_type: String, data: String) -> Result<String, String> {
    match challenge_type.as_str() {
        "md5" => {
            // Split data into "hash/source"
            let parts: Vec<&str> = data.split('/').collect();
            if parts.len() != 2 {
                return Err("Invalid challenge data format".to_string());
            }

            let target_hash = parts[0];
            let source = parts[1];

            // Try to find the solution by appending hex values
            for i in 0..=255 {
                let candidate = format!("{}{:02x}", source, i);
                let hash = chksum_md5::hash(candidate.as_bytes()).to_hex_lowercase();
                if hash == target_hash {
                    return Ok(candidate);
                }
            }

            Err("Failed to solve MD5 challenge".to_string())
        }
        "sha256" => {
            // Split data into "hash/source"
            let parts: Vec<&str> = data.split('/').collect();
            if parts.len() != 2 {
                return Err("Invalid challenge data format".to_string());
            }

            let target_hash = parts[0].to_string();
            let source = parts[1].to_string();

            // Use spawn_blocking for CPU-intensive SHA256 computation
            let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
                use sha2::{Digest, Sha256};

                // Try different suffix lengths - start with reasonable range
                for suffix_len in 1..=8u32 {
                    let max_val = 16_u64.pow(suffix_len);

                    for i in 0..max_val {
                        let suffix = format!("{:0width$x}", i, width = suffix_len as usize);
                        let candidate = format!("{}{}", source, suffix);

                        let mut hasher = Sha256::new();
                        hasher.update(candidate.as_bytes());
                        let hash = format!("{:x}", hasher.finalize());

                        if hash == target_hash {
                            return Ok(candidate);
                        }
                    }
                }

                Err("Failed to solve SHA256 challenge".to_string())
            })
            .await
            .map_err(|e| format!("SHA256 challenge task failed: {}", e))?;

            result
        }
        "web" => {
            // TODO: Web challenges need to be handled by the frontend
            // as they may require user interaction (captcha, browser popup, etc.)
            Err("Web challenges must be handled by the frontend".to_string())
        }
        _ => Err(format!("Unsupported challenge type: {}", challenge_type)),
    }
}

pub async fn get_http_with_range(url: String, offset: u64, size: u64) -> TAResult<(u16, Vec<u8>)> {
    let mut res = REQUEST_CLIENT.get(&url);
    if offset != 0 || size != 0 {
        res = res.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
    }
    let res = res
        .send()
        .await
        .with_http_context("get_http_with_range", &url)?;
    let status = res.status();
    let bytes = res
        .bytes()
        .await
        .map(|b| b.to_vec())
        .with_http_context("get_http_with_range", &url)?;

    Ok((status.as_u16(), bytes))
}

pub async fn http_get_request(
    url: String,
    ignore_redirects: Option<bool>,
    headers: Option<HashMap<String, String>>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<HttpGetResponse> {
    // Send request — use a one-off raw client when redirect policy differs
    let response = if ignore_redirects.unwrap_or(false) {
        let client = reqwest::ClientBuilder::new()
            .user_agent(crate::capabilities::ua_string())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("create HTTP client")?;

        let mut rb = client.get(&url);
        if let Some(timeout) = timeout_ms {
            rb = rb.timeout(Duration::from_millis(timeout));
        }
        if let Some(ref custom_headers) = headers {
            for (key, value) in custom_headers {
                rb = rb.header(key, value);
            }
        }
        rb.send()
            .await
            .with_http_context("http_get_request", &url)
            ?
    } else {
        let mut rb = REQUEST_CLIENT.get(&url);
        if let Some(timeout) = timeout_ms {
            rb = rb.timeout(Duration::from_millis(timeout));
        }
        if let Some(ref custom_headers) = headers {
            for (key, value) in custom_headers {
                rb = rb.header(key, value);
            }
        }
        rb.send()
            .await
            .with_http_context("http_get_request", &url)
            ?
    };

    // Get final URL (after redirects)
    let final_url = if let Some(redirected_url) = response.headers().get("Location") {
        redirected_url.to_str().unwrap_or("").to_string()
    } else {
        response.url().to_string()
    };

    // Get status code
    let status_code = response.status().as_u16();

    // Extract headers
    let mut response_headers = HashMap::new();
    for (name, value) in response.headers() {
        if let Ok(value_str) = value.to_str() {
            response_headers.insert(name.to_string(), value_str.to_string());
        }
    }

    // Get response body
    let body = response
        .text()
        .await
        .with_http_context("http_get_request", &url)
        .context("read response body")?;

    Ok(HttpGetResponse {
        status_code,
        headers: response_headers,
        body,
        final_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_url_gate_rejects_local_paths() {
        assert!(is_remote_insight_url("https://cdn.example.com/file"));
        assert!(is_remote_insight_url("http://127.0.0.1/a"));
        assert!(is_remote_insight_url("h3wt://node.example/path"));
        assert!(!is_remote_insight_url(""));
        assert!(!is_remote_insight_url("unknown"));
        assert!(!is_remote_insight_url("file:///C:/pack.bin"));
        assert!(!is_remote_insight_url("C:\\pack.bin"));
    }

    #[test]
    fn insight_error_is_the_code_and_first_wins() {
        use crate::utils::code::{Attach, DOWNLOAD_STALLED, HASH_MISMATCH, INTERNAL_ERROR};
        let mut item = InsightItem {
            url: "https://x".to_string(),
            ttfb: 1,
            time: 1,
            size: 1,
            error: None,
            range: vec![],
            mode: None,
        };
        apply_insight_error(&mut item, &anyhow::anyhow!("mismatch").attach(HASH_MISMATCH));
        assert_eq!(item.error.as_deref(), Some(HASH_MISMATCH));
        apply_insight_error(&mut item, &anyhow::anyhow!("x").attach(DOWNLOAD_STALLED));
        assert_eq!(item.error.as_deref(), Some(HASH_MISMATCH));

        item.error = None;
        apply_insight_error(&mut item, &anyhow::anyhow!("Failed to skip bytes"));
        assert_eq!(item.error.as_deref(), Some(INTERNAL_ERROR));

        item.error = None;
        apply_insight_io_error(&mut item, &std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert_eq!(item.error.as_deref(), Some(crate::utils::code::DOWNLOAD_TIMEOUT));

        let status = HttpStatus::new(503, "a".repeat(1000));
        assert_eq!(status.body.len(), 512);
        assert!(status.to_string().starts_with("503: "));
    }
}
