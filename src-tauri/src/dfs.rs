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
    let json: Result<DownloadResp, reqwest::Error> = res.json().await;
    if json.is_err() {
        return Err(format!("Failed to parse json: {:?}", json.err()));
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
        return Err(format!("{}", res.status()));
    }
    let json: Result<DownloadResp, reqwest::Error> = res.json().await;
    if json.is_err() {
        return Err(format!("Failed to parse json: {:?}", json.err()));
    }
    let json = json.unwrap();
    if json.challenge.is_some() {
        return Err("Challenge not solved".to_string());
    }
    Ok(json)
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
