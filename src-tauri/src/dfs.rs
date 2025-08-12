use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{utils::error::TAResult, REQUEST_CLIENT};

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
    pub metadata: serde_json::Value,
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

#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct InsightItem {
    pub url: String,
    pub ttfb: u32,        // 首字节时间(ms)
    pub time: u32,        // 纯下载时间(ms) = 总时间 - TTFB
    pub size: u32,        // 实际下载字节数
    pub range: Vec<(u32, u32)>, // HTTP Range请求范围
    pub error: Option<String>,
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

#[tauri::command]
pub async fn get_dfs(
    url: String,
    range: Option<String>,
    extras: Option<String>,
) -> Result<DownloadResp, String> {
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
    let res: Result<reqwest::Response, reqwest::Error> = REQUEST_CLIENT
        .post(&url_with_range_in_query)
        .body(extras.clone())
        .send()
        .await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    // check status code if is not 200 or 401
    if res.status() != reqwest::StatusCode::OK && res.status() != reqwest::StatusCode::UNAUTHORIZED
    {
        let status = res.status();
        // check if body exists
        let body = res.text().await;
        if body.is_err() {
            return Err(format!("{status}"));
        } else {
            return Err(format!("{}: {}", status, body.unwrap()));
        }
    }
    let body_text = res.text().await;
    if body_text.is_err() {
        return Err(format!("Failed to read response body: {:?}", body_text.err()));
    }
    let body_text = body_text.unwrap();
    let json: Result<DownloadResp, serde_json::Error> = serde_json::from_str(&body_text);
    if json.is_err() {
        return Err(format!("Failed to parse JSON ({}): {}", json.err().unwrap(), body_text));
    }
    let json = json.unwrap();
    // directly return if not challenge
    if json.challenge.is_none() {
        return Ok(json);
    }
    let challenge = json.challenge.unwrap();
    // split challenge into "hash/source"
    let challenge: Vec<&str> = challenge.split('/').collect();
    if challenge.len() != 2 {
        return Err("Invalid challenge".to_string());
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
        return Err("Failed to solve challenge".to_string());
    }
    let url = format!("{url_with_range_in_query}&sid={solve}");
    let res: Result<reqwest::Response, reqwest::Error> =
        REQUEST_CLIENT.post(&url).body(extras).send().await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    // check status code if is not 200 or 401
    if res.status() != reqwest::StatusCode::OK && res.status() != reqwest::StatusCode::UNAUTHORIZED
    {
        let status = res.status();
        let body = res.text().await;
        if body.is_err() {
            return Err(format!("{status}"));
        } else {
            return Err(format!("{}: {}", status, body.unwrap()));
        }
    }
    let body_text = res.text().await;
    if body_text.is_err() {
        return Err(format!("Failed to read response body: {:?}", body_text.err()));
    }
    let body_text = body_text.unwrap();
    let json: Result<DownloadResp, serde_json::Error> = serde_json::from_str(&body_text);
    if json.is_err() {
        return Err(format!("Failed to parse JSON ({}): {}", json.err().unwrap(), body_text));
    }
    let json = json.unwrap();
    if json.challenge.is_some() {
        return Err("Challenge not solved".to_string());
    }
    Ok(json)
}

// DFS2 API commands
#[tauri::command]
pub async fn get_dfs2_metadata(api_url: String) -> Result<Dfs2Metadata, String> {
    let url_with_metadata = if api_url.contains('?') {
        format!("{}&with_metadata=1", api_url)
    } else {
        format!("{}?with_metadata=1", api_url)
    };
    
    let res = REQUEST_CLIENT
        .get(&url_with_metadata)
        .send()
        .await
        .map_err(|e| format!("Failed to send request: {:?}", e))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, body));
    }

    let body_text = res
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {:?}", e))?;

    let metadata: Dfs2Metadata = serde_json::from_str(&body_text)
        .map_err(|e| format!("Failed to parse JSON ({}): {}", e, body_text))?;

    Ok(metadata)
}

#[tauri::command]
pub async fn create_dfs2_session(
    api_url: String,
    chunks: Option<Vec<String>>,
    version: Option<String>,
    challenge_response: Option<String>,
    session_id: Option<String>,
    extras: Option<serde_json::Value>,
) -> Result<Dfs2SessionResponse, String> {
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
        .map_err(|e| format!("Failed to send request: {:?}", e))?;

    let status = res.status();
    let body_text = res
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {:?}", e))?;

    let response: Dfs2SessionResponse = serde_json::from_str(&body_text)
        .map_err(|e| format!("Failed to parse JSON ({}): {}", e, body_text))?;

    // Return response directly - let frontend handle challenges
    if !status.is_success() && status != reqwest::StatusCode::PAYMENT_REQUIRED {
        return Err(format!("Session creation failed: {}", status));
    }

    Ok(response)
}

#[tauri::command]
pub async fn get_dfs2_chunk_url(
    session_api_url: String,
    range: String,
) -> Result<Dfs2ChunkResponse, String> {
    let url = format!("{}?range={}", session_api_url, range);

    let res = REQUEST_CLIENT
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to send request: {:?}", e))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, body));
    }

    let body_text = res
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {:?}", e))?;

    let response: Dfs2ChunkResponse = serde_json::from_str(&body_text)
        .map_err(|e| format!("Failed to parse JSON ({}): {}", e, body_text))?;

    Ok(response)
}

#[tauri::command]
pub async fn end_dfs2_session(
    session_api_url: String,
    insights: Option<Dfs2SessionInsights>,
) -> Result<(), String> {
    let request_body = Dfs2DeleteRequest { insights };

    let res = REQUEST_CLIENT
        .delete(&session_api_url)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("Failed to send request: {:?}", e))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, body));
    }
    Ok(())
}

#[tauri::command]
pub async fn solve_dfs2_challenge(
    challenge_type: String,
    data: String,
) -> Result<String, String> {
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
                use sha2::{Sha256, Digest};
                
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
            }).await.map_err(|e| format!("SHA256 challenge task failed: {}", e))?;
            
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

#[tauri::command]
pub async fn get_http_with_range(url: String, offset: u64, size: u64) -> TAResult<(u16, Vec<u8>)> {
    let mut res = REQUEST_CLIENT.get(&url);
    if offset != 0 || size != 0 {
        res = res.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
    }
    let res = res.send().await.context(format!("HTTP_GET_ERR: {}", url))?;
    let status = res.status();
    let bytes = res
        .bytes()
        .await
        .map(|b| b.to_vec())
        .context("HTTP_READ_ERR")?;

    Ok((status.as_u16(), bytes))
}
