use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use futures::StreamExt;
use serde::Serialize;
use std::{io::Read as _, os::windows::fs::MetadataExt, path::Path};
use tauri::{AppHandle, Emitter, WebviewWindow};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{progressed_read::ReadWithCallback, static_obj::REQUEST_CLIENT};

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
    if let Ok(exe_path) = exe_path {
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
    let mut boxed = Box::new([0u8; 256 * 1024]);
    let buffer = &mut *boxed;
    let mut now = std::time::Instant::now();
    loop {
        let read: Result<usize, std::io::Error> = decoder.read(buffer).await;
        if read.is_err() {
            return Err(format!("Failed to read from decoder: {:?}", read.err()));
        }
        let read = read.unwrap();
        if read == 0 {
            break;
        }
        downloaded += read;
        // emit only every 16 ms
        if now.elapsed().as_millis() >= 100 {
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

#[tauri::command]
pub async fn run_hpatch(target: String, diff: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let target = Path::new(&target);
        let new_target = target.with_extension("patching");
        let target_size = target
            .metadata()
            .map_err(|e| format!("Failed to get target size: {:?}", e))?;
        let target_file = std::fs::File::create(new_target.clone())
            .map_err(|e| format!("Failed to create new target: {:?}", e))?;
        let old_target_file =
            std::fs::File::open(target).map_err(|e| format!("Failed to open target: {:?}", e))?;
        let diff_size = std::fs::metadata(&diff)
            .map_err(|e| format!("Failed to get diff size: {:?}", e))?
            .file_size();
        let diff_file =
            std::fs::File::open(diff).map_err(|e| format!("Failed to open diff: {:?}", e))?;
        let res = hpatch_sys::safe_patch_single_stream(
            target_file,
            diff_file,
            diff_size as usize,
            old_target_file,
            target_size.file_size() as usize,
        );
        if res {
            // move target to target.old
            let old_target = target.with_extension("old");
            std::fs::rename(target, &old_target)
                .map_err(|e| format!("Failed to rename target: {:?}", e))?;
            std::fs::rename(new_target, target)
                .map_err(|e| format!("Failed to rename new target: {:?}", e))?;
            Ok(())
        } else {
            // delete new target
            std::fs::remove_file(new_target)
                .map_err(|e| format!("Failed to remove new target: {:?}", e))?;
            Err("Failed to run hpatch".to_string())
        }
    })
    .await
    .map_err(|e| format!("Failed to run hpatch: {:?}", e))?
}

#[tauri::command]
pub async fn download_and_decompress_and_hpatch(
    id: String,
    url: String,
    diff_size: usize,
    target: String,
    app: AppHandle,
) -> Result<usize, String> {
    let target_cl = target.clone();
    let target = Path::new(&target);
    let exe_path = std::env::current_exe();
    if let Ok(exe_path) = exe_path {
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
    let res = REQUEST_CLIENT.get(&url).send().await;
    if res.is_err() {
        return Err(format!("Failed to send http request: {:?}", res.err()));
    }
    let res = res.unwrap();
    let stream = futures::TryStreamExt::map_err(res.bytes_stream(), std::io::Error::other);
    let reader = tokio_util::io::StreamReader::new(stream);
    let decoder = TokioZstdDecoder::new(reader);
    let app_cl = app.clone();
    let id_cl = id.clone();
    let mut downloaded = 0;
    let decoder = ReadWithCallback {
        reader: decoder,
        callback: move |chunk| {
            downloaded += chunk;
            let _ = app_cl.emit(&id_cl, downloaded);
        },
    };
    tokio::task::spawn_blocking(move || {
        let target_cl = Path::new(&target_cl);
        let new_target = target_cl.with_extension("patching");
        let target_size = target_cl
            .metadata()
            .map_err(|e| format!("Failed to get target size: {:?}", e))?;
        let target_file = std::fs::File::create(new_target.clone());
        let old_target_file = std::fs::File::open(target_cl)
            .map_err(|e| format!("Failed to open target: {:?}", e))?;
        let diff_file = tokio_util::io::SyncIoBridge::new(decoder);
        let res = hpatch_sys::safe_patch_single_stream(
            target_file.map_err(|e| format!("Failed to create new target: {:?}", e))?,
            diff_file,
            diff_size,
            old_target_file,
            target_size.file_size() as usize,
        );
        if res {
            // move target to target.old
            let old_target = target_cl.with_extension("old");
            let exe_path = std::env::current_exe();
            let exe_path = exe_path.map_err(|e| format!("Failed to get exe path: {:?}", e))?;
            // rename to .old if the target is the same as exe
            if exe_path == target_cl {
                let old_target = target_cl.with_extension("old");
                std::fs::rename(target_cl, old_target)
                    .map_err(|e| format!("Failed to rename target: {:?}", e))?;
            } else {
                // delete old file
                std::fs::remove_file(old_target)
                    .map_err(|e| format!("Failed to remove old target: {:?}", e))?;
            }
            std::fs::rename(new_target, target_cl)
                .map_err(|e| format!("Failed to rename new target: {:?}", e))?;
            Ok(())
        } else {
            // delete new target
            std::fs::remove_file(new_target)
                .map_err(|e| format!("Failed to remove new target: {:?}", e))?;
            Err("Failed to run hpatch".to_string())
        }
    })
    .await
    .map_err(|e| format!("Failed to exec hpatch: {:?}", e))??;
    Ok(downloaded)
}

#[derive(Serialize, Debug)]
pub struct Metadata {
    pub file_name: String,
    pub hash: String,
    pub size: u64,
}

#[tauri::command]
pub async fn deep_readdir_with_metadata(
    id: String,
    source: String,
    app: AppHandle,
    hash_algorithm: String,
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
                        hash: "".to_string(),
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
        if hash_algorithm == "md5" {
            let md5 = chksum_md5::async_chksum(Path::new(&file.file_name)).await;
            if md5.is_err() {
                return Err(format!("Failed to calculate md5: {:?}", md5.err()));
            }
            let md5 = md5.unwrap();
            file.hash = md5.to_hex_lowercase();
        } else if hash_algorithm == "xxh" {
            let name = file.file_name.clone();
            let res = tokio::task::spawn_blocking(move || {
                use twox_hash::XxHash3_128;
                let mut hasher = XxHash3_128::new();
                let mut file = std::fs::File::open(&name).unwrap();
                let mut buffer = [0u8; 1024];
                loop {
                    let read = file.read(&mut buffer);
                    if read.is_err() {
                        return Err(format!("Failed to read file: {:?}", read.err()));
                    }
                    let read = read.unwrap();
                    if read == 0 {
                        break;
                    }
                    hasher.write(&buffer[..read]);
                }
                let hash = hasher.finish_128();
                Ok(format!("{:x}", hash))
            })
            .await;
            if res.is_err() {
                return Err(format!("Failed to calculate xxhash: {:?}", res.err()));
            }
            let res = res.unwrap();
            if res.is_err() {
                return Err(format!("Failed to calculate xxhash: {:?}", res.err()));
            }
            let res = res.unwrap();
            file.hash = res;
        }
        // send progress
        let _ = app.emit(&id, (i as u64 + 1, len));
    }
    Ok(files)
}

#[tauri::command]
pub async fn is_dir_empty(path: String, exe_name: String) -> bool {
    let path = Path::new(&path);
    if !path.exists() {
        return true;
    }
    let entries = tokio::fs::read_dir(path).await;
    if entries.is_err() {
        return true;
    }
    // check if exe exists
    let exe_path = path.join(exe_name);
    if exe_path.exists() {
        return true;
    }
    let mut entries = entries.unwrap();
    if let Ok(Some(_entry)) = entries.next_entry().await {
        return false;
    }
    true
}

#[tauri::command]
pub async fn ensure_dir(path: String) -> Result<(), String> {
    let path = Path::new(&path);
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| format!("Failed to create dir: {:?}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn select_dir(path: String) -> Option<String> {
    let res = rfd::AsyncFileDialog::new()
        .set_directory(path)
        .set_can_create_directories(true)
        .pick_folder()
        .await;
    res.as_ref()?;
    let res = res.unwrap();
    res.path().to_str().map(|s| s.to_string())
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
