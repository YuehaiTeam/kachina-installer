use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::REQUEST_CLIENT;

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
pub async fn get_dfs(url: String) -> Result<DownloadResp, String> {
    let res: Result<reqwest::Response, reqwest::Error> = REQUEST_CLIENT.post(&url).send().await;
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
        let new_src = format!("{}{:02x}", source, i);
        let new_hash = chksum_md5::hash(new_src.as_bytes()).to_hex_lowercase();
        if hash == new_hash {
            solve = new_src;
            break;
        }
    }
    if solve.is_empty() {
        return Err("Failed to solve challenge".to_string());
    }
    let url = format!("{}?sid={}", url, solve);
    let res: Result<reqwest::Response, reqwest::Error> = REQUEST_CLIENT.post(&url).send().await;
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
pub async fn get_http_with_range(
    url: String,
    offset: u64,
    size: u64,
) -> Result<(u16, Vec<u8>), String> {
    let mut res = REQUEST_CLIENT.get(&url);
    if offset != 0 || size != 0 {
        res = res.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
    }
    let res = res.send().await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    let status = res.status();
    let bytes: Result<Vec<u8>, reqwest::Error> = res.bytes().await.map(|b| b.to_vec());
    if bytes.is_err() {
        return Err(format!("Failed to get bytes: {:?}", bytes.err()));
    }
    Ok((status.as_u16(), bytes.unwrap()))
}

#[tauri::command]
pub async fn get_dfs_metadata(prefix: String) -> Result<Value, String> {
    // retry for 3 times
    let mut res: Result<Value, String> = run_get_dfs_metadata(prefix.clone()).await;
    if res.is_err() {
        for _ in 0..2 {
            res = run_get_dfs_metadata(prefix.clone()).await;
            if res.is_ok() {
                break;
            }
        }
    }
    res
}

pub async fn run_get_dfs_metadata(prefix: String) -> Result<Value, String> {
    let url = format!("https://77.cocogoat.cn/v2/dfs/{}/.metadata.json", prefix);
    let res: Result<reqwest::Response, reqwest::Error> = REQUEST_CLIENT.get(&url).send().await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    // check status code if is not 200 or 401
    if res.status() != reqwest::StatusCode::OK {
        return Err(format!("{}", res.status()));
    }
    let json: Result<Value, reqwest::Error> = res.json().await;
    if json.is_err() {
        return Err(format!("Failed to parse json: {:?}", json.err()));
    }
    let json = json.unwrap();
    Ok(json)
}
