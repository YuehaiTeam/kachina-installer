use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use futures::StreamExt;
use serde::Serialize;
use std::path::Path;
use tauri::{AppHandle, Emitter, WebviewWindow};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::static_obj::REQUEST_CLIENT;

#[tauri::command]
pub async fn decompress(source: String, target: String) -> String {
    let source = Path::new(&source);
    let target = Path::new(&target);
    let source_file = tokio::fs::File::open(source).await;
    if source_file.is_err() {
        return format!("Failed to open source file: {:?}", source_file.err());
    }
    let source_file = source_file.unwrap();
    let mut source_file = tokio::io::BufReader::new(source_file);
    let mut decoder = TokioZstdDecoder::new(&mut source_file);
    let target_file: Result<tokio::fs::File, std::io::Error> =
        tokio::fs::File::create(target).await;
    if target_file.is_err() {
        return format!("Failed to create target file: {:?}", target_file.err());
    }
    let mut target_file = target_file.unwrap();
    let res = tokio::io::copy(&mut decoder, &mut target_file).await;
    if res.is_err() {
        return format!("Failed to decompress: {:?}", res.err());
    }
    let bytes = res.unwrap();
    bytes.to_string()
}

#[tauri::command]
pub async fn md5_file(path: String) -> Result<String, String> {
    let path = Path::new(&path);
    let res = chksum_md5::async_chksum(path).await;
    if res.is_err() {
        return Err(format!("Failed to calculate md5: {:?}", res.err()));
    }
    let res = res.unwrap();
    Ok(res.to_hex_lowercase())
}

#[tauri::command]
pub async fn download_and_decompress(
    id: String,
    url: String,
    target: String,
    app: AppHandle,
) -> Result<usize, String> {
    let target = Path::new(&target);
    let exe_path = std::env::current_exe();
    if let Some(exe_path) = exe_path.ok() {
        // check if target is the same as exe path
        if exe_path == target {
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
    let target_file = tokio::fs::File::create(target).await;
    if target_file.is_err() {
        return Err(format!(
            "Failed to create target file: {:?}",
            target_file.err()
        ));
    }
    // download and decompress with progress using reqwest, pass progress with emit("id", downloaded)
    let target_file = target_file.unwrap();
    let mut target_file = tokio::io::BufWriter::new(target_file);
    let res = REQUEST_CLIENT.get(&url).send().await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    let stream = futures::TryStreamExt::map_err(res.bytes_stream(), std::io::Error::other);
    let mut reader = tokio_util::io::StreamReader::new(stream);
    let mut decoder = TokioZstdDecoder::new(&mut reader);
    let mut downloaded = 0;
    let buffer = &mut [0u8; 1024];
    let mut now = std::time::Instant::now();
    loop {
        let read = decoder.read(buffer).await;
        if read.is_err() {
            return Err(format!("Failed to read from decoder: {:?}", read.err()));
        }
        let read = read.unwrap();
        if read == 0 {
            break;
        }
        downloaded += read;
        // emit only every 16 ms
        if now.elapsed().as_millis() >= 16 {
            now = std::time::Instant::now();
            let _ = app.emit(&id, downloaded);
        }
        let write = target_file.write_all(&buffer[..read]).await;
        if write.is_err() {
            return Err(format!("Failed to write to target file: {:?}", write.err()));
        }
    }
    // flush the buffer
    let res = target_file.flush().await;
    if res.is_err() {
        return Err(format!("Failed to flush target file: {:?}", res.err()));
    }
    // emit the final progress
    let _ = app.emit(&id, downloaded);
    Ok(downloaded)
}

#[derive(Serialize, Debug)]
pub struct Metadata {
    pub file_name: String,
    pub md5: String,
    pub size: u64,
}

#[tauri::command]
pub async fn deep_readdir_with_metadata(
    id: String,
    source: String,
    app: AppHandle,
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
                    files.push(Metadata {
                        file_name: path.to_string(),
                        md5: "".to_string(),
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
    // send first progress
    let _ = app.emit(&id, (0, files.len()));
    let len = files.len();
    for (i, file) in files.iter_mut().enumerate() {
        let md5 = chksum_md5::async_chksum(Path::new(&file.file_name)).await;
        if md5.is_err() {
            return Err(format!("Failed to calculate md5: {:?}", md5.err()));
        }
        let md5 = md5.unwrap();
        file.md5 = md5.to_hex_lowercase();
        // send progress
        let _ = app.emit(&id, (i as u64 + 1, len));
    }
    Ok(files)
}

#[tauri::command]
pub async fn is_dir_empty(path: String) -> bool {
    let path = Path::new(&path);
    if !path.exists() {
        return true;
    }
    let entries = tokio::fs::read_dir(path).await;
    if entries.is_err() {
        return true;
    }
    let mut entries = entries.unwrap();
    while let Ok(Some(_entry)) = entries.next_entry().await {
        return false;
    }
    true
}

#[tauri::command]
pub async fn ensure_dir(path: String) -> Result<(), String> {
    let path = Path::new(&path);
    let res = tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| format!("Failed to create dir: {:?}", e))?;
    Ok(res)
}

#[tauri::command]
pub async fn select_dir(path: String) -> Option<String> {
    let res = rfd::AsyncFileDialog::new()
        .set_directory(path)
        .set_can_create_directories(true)
        .pick_folder()
        .await;
    if res.is_none() {
        return None;
    }
    let res = res.unwrap();
    return res.path().to_str().map(|s| s.to_string());
}

#[tauri::command]
pub async fn error_dialog(title: String, message: String, window: WebviewWindow) {
    rfd::MessageDialog::new()
        .set_title(&title)
        .set_description(&message)
        .set_level(rfd::MessageLevel::Error)
        .set_parent(&window)
        .show();
}
