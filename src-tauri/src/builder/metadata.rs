use std::path::{Path, PathBuf};

use futures::StreamExt;

use crate::utils::{hash::run_hash, metadata::Metadata};

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
                return Err(format!("Failed to read entry: {e:?}"));
            }
            None => break,
        }
    }

    let mut joinset = tokio::task::JoinSet::new();

    for file in files.iter() {
        let source = source.clone();
        let mut file = file.clone();
        joinset.spawn(async move {
            let real_path = source.join(&file.file_name);
            let hash = run_hash("xxh", real_path.to_str().unwrap()).await;
            if hash.is_err() {
                return Err(hash.err().unwrap());
            }
            let hash = hash.unwrap();
            file.xxh = Some(hash);
            println!("Hashed: {:?}", file.file_name);
            Ok(file)
        });
    }
    let mut finished_hashes = Vec::new();
    while let Some(res) = joinset.join_next().await {
        if let Err(e) = res {
            return Err(format!("Failed to run hashing thread: {e:?}"));
        }
        let res = res.unwrap();
        if let Err(e) = res {
            return Err(format!("Failed to finish hashing: {e:?}"));
        }
        let res = res.unwrap();
        finished_hashes.push(res);
    }
    Ok(finished_hashes)
}

pub async fn deep_get_filelist(source: &PathBuf) -> Result<Vec<String>, String> {
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
                    files.push(fin_path);
                }
            }
            Some(Err(e)) => {
                return Err(format!("Failed to read entry: {e:?}"));
            }
            None => break,
        }
    }
    Ok(files)
}
