use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use fmmap::tokio::AsyncMmapFileExt;
use futures::StreamExt;
use serde::Serialize;
use std::{os::windows::fs::MetadataExt, path::Path};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    local::mmap, utils::hash::run_hash, utils::progressed_read::ReadWithCallback, REQUEST_CLIENT,
};

#[derive(Serialize, Debug, Clone)]
pub struct Metadata {
    pub file_name: String,
    pub hash: String,
    pub size: u64,
    pub unwritable: bool,
}

#[tauri::command]
pub async fn deep_readdir_with_metadata(
    id: String,
    source: String,
    app: AppHandle,
    hash_algorithm: String,
    file_list: Vec<String>,
) -> Result<Vec<Metadata>, String> {
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
                    let size = entry.metadata().await.unwrap().len();
                    if file_list.contains(&path.to_string()) {
                        files.push(Metadata {
                            file_name: path.to_string(),
                            hash: "".to_string(),
                            size,
                            unwritable: false,
                        });
                    }
                }
            }
            Some(Err(e)) => {
                return Err(format!("Failed to read entry: {:?}", e));
            }
            None => break,
        }
    }
    // send first progress
    let _ = app.emit(&id, (0, files.len()));
    let len = files.len();
    let mut joinset = tokio::task::JoinSet::new();

    for file in files.iter() {
        let hash_algorithm = hash_algorithm.clone();
        let mut file = file.clone();
        joinset.spawn(async move {
            let exists = Path::new(&file.file_name).exists();
            let writable = !exists
                || tokio::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&file.file_name)
                    .await
                    .is_ok();
            if !writable {
                file.unwritable = true;
            } else {
                let res = run_hash(&hash_algorithm, &file.file_name).await;
                if res.is_err() {
                    return Err(res.err().unwrap());
                }
                let hash = res.unwrap();
                file.hash = hash;
            }
            Ok(file)
        });
    }

    let mut finished = 0;
    let mut finished_hashes = Vec::new();

    while let Some(res) = joinset.join_next().await {
        if let Err(e) = res {
            return Err(format!("Failed to run hashing thread: {:?}", e));
        }
        let res = res.unwrap();
        if let Err(e) = res {
            return Err(format!("Failed to finish hashing: {:?}", e));
        }
        let res = res.unwrap();
        finished += 1;
        let _ = app.emit(&id, (finished, len));
        finished_hashes.push(res);
    }
    Ok(finished_hashes)
}

#[tauri::command]
pub async fn is_dir_empty(path: String, exe_name: String) -> (bool, bool) {
    let path = Path::new(&path);
    if !path.exists() {
        return (true, false);
    }
    let entries = tokio::fs::read_dir(path).await;
    if entries.is_err() {
        return (true, false);
    }
    // check if exe exists
    let exe_path = path.join(exe_name);
    if exe_path.exists() {
        return (false, true);
    }
    let mut entries = entries.unwrap();
    if let Ok(Some(_entry)) = entries.next_entry().await {
        return (false, false);
    }
    (true, false)
}

#[tauri::command]
pub async fn ensure_dir(path: String) -> Result<(), String> {
    let path = Path::new(&path);
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| format!("Failed to create dir: {:?}", e))?;
    Ok(())
}

pub async fn create_http_stream(
    url: &str,
    offset: usize,
    size: usize,
    skip_decompress: bool,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>, String> {
    let mut res = REQUEST_CLIENT.get(url);
    let has_range = offset > 0 || size > 0;
    if has_range {
        res = res.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
        println!("Range: bytes={}-{}", offset, offset + size - 1);
    }
    let res = res.send().await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    let code = res.status();
    if (!has_range && code != 200) || (has_range && code != 206) {
        return Err(format!("Failed to download: URL {} returned {}", url, code));
    }
    let stream = futures::TryStreamExt::map_err(res.bytes_stream(), std::io::Error::other);
    let reader = tokio_util::io::StreamReader::new(stream);
    if skip_decompress {
        return Ok(Box::new(reader));
    }
    let decoder = TokioZstdDecoder::new(reader);
    Ok(Box::new(decoder))
}

pub async fn create_local_stream(
    offset: usize,
    size: usize,
    skip_decompress: bool,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>, String> {
    let mmap_file = mmap().await;
    let reader = mmap_file
        .range_reader(offset, size)
        .map_err(|e| format!("Failed to mmap: {:?}", e))?;
    if skip_decompress {
        return Ok(Box::new(reader));
    }
    let decoder = TokioZstdDecoder::new(reader);
    Ok(Box::new(decoder))
}

pub async fn prepare_target(target: &str) -> Result<(), String> {
    let target = Path::new(&target);
    let exe_path = std::env::current_exe();
    if let Ok(exe_path) = exe_path {
        // check if target is the same as exe path
        if exe_path == target && exe_path.exists() {
            // if same, rename the exe to exe.old
            let old_exe = exe_path.with_extension("old");
            let res = tokio::fs::rename(&exe_path, &old_exe).await;
            if res.is_err() {
                return Err(format!("Failed to rename current exe: {:?}", res.err()));
            }
        }
    }
    // ensure dir
    let parent = target.parent();
    if parent.is_none() {
        return Err("Failed to get parent dir".to_string());
    }
    let parent = parent.unwrap();
    let res = tokio::fs::create_dir_all(parent).await;
    if res.is_err() {
        return Err(format!("Failed to create parent dir: {:?}", res.err()));
    }
    Ok(())
}

pub async fn create_target_file(target: &str) -> Result<impl AsyncWrite, String> {
    let target_file = tokio::fs::File::create(target).await;
    if target_file.is_err() {
        return Err(format!(
            "Failed to create target file: {:?}",
            target_file.err()
        ));
    }
    let target_file = target_file.unwrap();
    let target_file = tokio::io::BufWriter::new(target_file);
    Ok(target_file)
}

pub async fn progressed_copy(
    mut source: impl AsyncRead + std::marker::Unpin,
    mut target: impl AsyncWrite + std::marker::Unpin,
    on_progress: impl Fn(usize),
) -> Result<usize, String> {
    let mut downloaded = 0;
    let mut boxed = Box::new([0u8; 256 * 1024]);
    let buffer = &mut *boxed;
    let mut now = std::time::Instant::now();
    loop {
        let read: Result<usize, std::io::Error> = source.read(buffer).await;
        if read.is_err() {
            return Err(format!("Failed to read from decoder: {:?}", read.err()));
        }
        let read = read.unwrap();
        if read == 0 {
            break;
        }
        downloaded += read;
        // emit only every 16 ms
        if now.elapsed().as_millis() >= 20 {
            now = std::time::Instant::now();
            on_progress(downloaded);
        }
        let write = target.write_all(&buffer[..read]).await;
        if write.is_err() {
            return Err(format!("Failed to write to target file: {:?}", write.err()));
        }
    }
    // flush the buffer
    let res = target.flush().await;
    if res.is_err() {
        return Err(format!("Failed to flush target file: {:?}", res.err()));
    }
    // emit the final progress
    on_progress(downloaded);
    Ok(downloaded)
}

pub async fn progressed_hpatch<R, F>(
    source: R,
    target: &str,
    diff_size: usize,
    on_progress: F,
) -> Result<usize, String>
where
    R: AsyncRead + std::marker::Unpin + Send + 'static,
    F: Fn(usize) + Send + 'static,
{
    let mut downloaded = 0;
    let decoder = ReadWithCallback {
        reader: source,
        callback: move |chunk| {
            downloaded += chunk;
            on_progress(downloaded);
        },
    };
    let target = target.to_string();
    let target_cl = Path::new(&target);
    let old_target_old = target_cl.with_extension("old");
    // try remove old_target_old, do not throw error if failed
    let _ = tokio::fs::remove_file(old_target_old).await;
    let new_target = target_cl.with_extension("patching");
    let target_size = target_cl
        .metadata()
        .map_err(|e| format!("Failed to get target size: {:?}", e))?;
    let target_file = std::fs::File::create(new_target.clone())
        .map_err(|e| format!("Failed to create new target: {:?}", e))?;
    let old_target_file = std::fs::File::open(target.clone())
        .map_err(|e| format!("Failed to open target: {:?}", e))?;
    let diff_file = tokio_util::io::SyncIoBridge::new(decoder);
    let res = tokio::task::spawn_blocking(move || {
        hpatch_sys::safe_patch_single_stream(
            target_file,
            diff_file,
            diff_size,
            old_target_file,
            target_size.file_size() as usize,
        )
    })
    .await
    .map_err(|e| format!("Failed to exec hpatch: {:?}", e))?;
    if res {
        // move target to target.old
        let old_target = target_cl.with_extension("old");
        let exe_path = std::env::current_exe();
        let exe_path = exe_path.map_err(|e| format!("Failed to get exe path: {:?}", e))?;
        // rename to .old
        tokio::fs::rename(target_cl, old_target.clone())
            .await
            .map_err(|e| format!("Failed to rename target: {:?}", e))?;
        // rename new file to original
        tokio::fs::rename(new_target, target_cl)
            .await
            .map_err(|e| format!("Failed to rename new target: {:?}", e))?;
        if exe_path != target_cl {
            // if old file is not self, delete old file
            tokio::fs::remove_file(old_target)
                .await
                .map_err(|e| format!("Failed to remove old target: {:?}", e))?;
        }
    } else {
        // delete new target
        tokio::fs::remove_file(new_target)
            .await
            .map_err(|e| format!("Failed to remove new target: {:?}", e))?;
        return Err("Failed to run hpatch".to_string());
    }
    Ok(diff_size)
}

pub async fn verify_hash(
    target: &str,
    md5: Option<String>,
    xxh: Option<String>,
) -> Result<(), String> {
    let alg = if md5.is_some() {
        "md5"
    } else if xxh.is_some() {
        "xxh"
    } else {
        return Ok(());
    };
    let expected = if let Some(md5) = md5 {
        md5
    } else if let Some(xxh) = xxh {
        xxh
    } else {
        return Err("No hash provided".to_string());
    };
    let hash = run_hash(alg, target).await?;
    if hash != expected {
        return Err(format!(
            "File {} hash mismatch: expected {}, got {}",
            target, expected, hash
        ));
    }
    Ok(())
}
