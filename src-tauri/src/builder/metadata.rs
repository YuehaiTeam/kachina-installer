use std::path::{Path, PathBuf};

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::utils::hash::run_hash;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Metadata {
    pub file_name: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchItem {
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchInfo {
    pub file_name: String,
    pub size: u64,
    pub from: PatchItem,
    pub to: PatchItem,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RepoMetadata {
    pub repo_name: String,
    pub tag_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<Metadata>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashed: Option<Vec<Metadata>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patches: Option<Vec<PatchInfo>>,
}

pub async fn deep_generate_metadata(source: &PathBuf) -> Result<Vec<Metadata>, String> {
    let path = Path::new(&source);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut entries = async_walkdir::WalkDir::new(source);
    let mut files = Vec::new();
    loop {
        match entries.next().await {
            Some(Ok(entry)) => {
                let f = entry.file_type().await;
                if f.is_err() {
                    return Err(format!("Failed to get file type: {:?}", f.err()));
                }
                let f = f.unwrap();
                if f.is_file() {
                    let path = entry.path();
                    let path = path.to_str();
                    if path.is_none() {
                        return Err("Failed to convert path to string".to_string());
                    }
                    let path = path.unwrap();
                    let fin_path = path.replace("\\", "/").replacen(
                        format!("{}/", source.to_str().unwrap()).as_str(),
                        "",
                        1,
                    );
                    let size = entry.metadata().await.unwrap().len();
                    files.push(Metadata {
                        file_name: fin_path,
                        md5: None,
                        xxh: None,
                        size,
                    });
                }
            }
            Some(Err(e)) => {
                return Err(format!("Failed to read entry: {:?}", e));
            }
            None => break,
        }
    }
    for file in files.iter_mut() {
        let source = source.clone();
        let real_path = source.join(&file.file_name);
        println!("Hashing: {:?}", file.file_name);
        let hash = run_hash("xxh", &real_path.to_str().unwrap()).await?;
        file.xxh = Some(hash);
    }
    Ok(files)
}
