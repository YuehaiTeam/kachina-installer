use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use fmmap::tokio::AsyncMmapFileExt;
use futures::StreamExt;
use serde::Serialize;
use std::{
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    dfs::InsightItem,
    installer::uninstall::DELETE_SELF_ON_EXIT_PATH,
    local::mmap,
    utils::{hash::run_hash, progressed_read::ReadWithCallback, download_monitor::DownloadMonitor},
    REQUEST_CLIENT,
};
use anyhow::{Context, Result};

#[derive(Serialize, Debug, Clone)]
pub struct Metadata {
    pub file_name: String,
    pub hash: String,
    pub size: u64,
    pub unwritable: bool,
}

pub async fn check_local_files(
    source: String,
    hash_algorithm: String,
    file_list: Vec<String>,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<Vec<Metadata>> {
    let path = Path::new(&source);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut entries = async_walkdir::WalkDir::new(source);
    let mut files = Vec::new();
    loop {
        match entries.next().await {
            Some(Ok(entry)) => {
                let f = entry.file_type().await.context("GET_FILE_TYPE_ERR")?;
                if f.is_file() {
                    let path = entry.path();
                    let path = path.to_str().context("PATH_TO_STRING_ERR")?;
                    let size = entry.metadata().await.context("GET_METADATA_ERR")?.len();
                    file_list.iter().for_each(|file| {
                        if path
                            .to_lowercase()
                            .replace("\\", "/")
                            .ends_with(&file.to_lowercase().replace("\\", "/"))
                        {
                            files.push(Metadata {
                                file_name: path.to_string(),
                                hash: "".to_string(),
                                size,
                                unwritable: false,
                            });
                        }
                    });
                }
            }
            Some(Err(e)) => {
                return Err(anyhow::Error::new(e).context("READ_DIR_ERR"));
            }
            None => break,
        }
    }
    // send first progress
    notify(serde_json::json!((0, files.len())));
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
            }
            let res = run_hash(&hash_algorithm, &file.file_name).await;
            if res.is_err() && writable {
                return Err(res.err().unwrap());
            }
            let hash = res.unwrap();
            file.hash = hash;

            Ok(file)
        });
    }

    let mut finished = 0;
    let mut finished_hashes = Vec::new();

    while let Some(res) = joinset.join_next().await {
        let res = res.context("HASH_THREAD_ERR")?;
        let res = res.context("HASH_COMPLETE_ERR")?;
        finished += 1;
        notify(serde_json::json!((finished, len)));
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
pub async fn ensure_dir(path: String) -> Result<(), anyhow::Error> {
    let path = Path::new(&path);
    tokio::fs::create_dir_all(path)
        .await
        .context("CREATE_DIR_ERR")?;
    Ok(())
}

pub async fn create_http_stream(
    url: &str,
    offset: usize,
    size: usize,
    skip_decompress: bool,
) -> Result<
    (
        Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>,
        u64,
        Option<InsightItem>,
    ),
    anyhow::Error,
> {
    let start_time = std::time::Instant::now();
    
    let mut res = REQUEST_CLIENT.get(url);
    let has_range = offset > 0 || size > 0;
    if has_range {
        res = res.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
    }
    
    let res = res.send().await.context("HTTP_REQUEST_ERR");
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            let insight = InsightItem {
                url: url.to_string(),
                ttfb: start_time.elapsed().as_millis() as u32,
                time: 0,
                size: 0,
                range: if has_range {
                    vec![(offset as u32, (offset + size - 1) as u32)]
                } else {
                    vec![]
                },
                error: Some(e.to_string()),
            };
            return Err(crate::utils::error::TACommandError::with_insight(
                e.context("HTTP_REQUEST_ERR"), 
                insight
            ).error);
        }
    };
    
    let ttfb = start_time.elapsed().as_millis() as u32;
    let code = res.status();
    
    if (!has_range && code != 200) || (has_range && code != 206) {
        let insight = InsightItem {
            url: url.to_string(),
            ttfb,
            time: 0,
            size: 0,
            range: if has_range {
                vec![(offset as u32, (offset + size - 1) as u32)]
            } else {
                vec![]
            },
            error: Some(format!("HTTP status error: {}", code)),
        };
        let error = anyhow::Error::new(std::io::Error::other(format!(
            "URL {url} returned {code}"
        )))
        .context("HTTP_STATUS_ERR");
        return Err(crate::utils::error::TACommandError::with_insight(error, insight).error);
    }
    
    let content_length = res.content_length().unwrap_or(0);
    let stream = futures::TryStreamExt::map_err(res.bytes_stream(), std::io::Error::other);
    let reader = tokio_util::io::StreamReader::new(stream);
    
    let insight = InsightItem {
        url: url.to_string(),
        ttfb,
        time: 0, // 将在progressed_copy中更新
        size: 0, // 将在progressed_copy中更新
        range: if has_range {
            vec![(offset as u32, (offset + size - 1) as u32)]
        } else {
            vec![]
        },
        error: None,
    };
    
    if skip_decompress {
        return Ok((Box::new(reader), content_length, Some(insight)));
    }
    let decoder = TokioZstdDecoder::new(reader);
    Ok((Box::new(decoder), content_length, Some(insight)))
}

fn parse_range_string(range: &str) -> Vec<(u32, u32)> {
    range
        .split(',')
        .filter_map(|part| {
            let mut split = part.trim().split('-');
            let start = split.next()?.parse::<u32>().ok()?;
            let end = split.next()?.parse::<u32>().ok()?;
            Some((start, end))
        })
        .collect()
}

pub async fn create_multi_http_stream(
    url: &str,
    range: &str,
) -> Result<
    (
        Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Unpin>,
        u64,
        String,
        InsightItem,
    ),
    anyhow::Error,
> {
    let start_time = std::time::Instant::now();
    let range_info = parse_range_string(range);
    
    let res = REQUEST_CLIENT
        .get(url)
        .header("Range", format!("bytes={range}"))
        .send()
        .await;
    
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            let insight = InsightItem {
                url: url.to_string(),
                ttfb: start_time.elapsed().as_millis() as u32,
                time: 0,
                size: 0,
                range: range_info,
                error: Some(e.to_string()),
            };
            return Err(crate::utils::error::TACommandError::with_insight(
                anyhow::Error::new(e).context("HTTP_REQUEST_ERR"), 
                insight
            ).error);
        }
    };
    
    let ttfb = start_time.elapsed().as_millis() as u32;
    let code = res.status();
    
    if code != 206 {
        let insight = InsightItem {
            url: url.to_string(),
            ttfb,
            time: 0,
            size: 0,
            range: range_info,
            error: Some(format!("HTTP status error: {}", code)),
        };
        let error = anyhow::Error::new(std::io::Error::other(format!(
            "URL {url} returned {code}"
        )))
        .context("HTTP_STATUS_ERR");
        return Err(crate::utils::error::TACommandError::with_insight(error, insight).error);
    }
    
    let content_length = res.content_length().unwrap_or(0);
    let content_type = res
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let insight = InsightItem {
        url: url.to_string(),
        ttfb,
        time: 0, // 将在multipart处理中更新
        size: 0, // 将在multipart处理中更新
        range: range_info,
        error: None,
    };

    Ok((
        Box::new(Box::pin(res.bytes_stream())),
        content_length,
        content_type,
        insight,
    ))
}

pub async fn create_local_stream(
    offset: usize,
    size: usize,
    skip_decompress: bool,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>, anyhow::Error> {
    let mmap_file = mmap().await;
    let reader = mmap_file.range_reader(offset, size).context("MMAP_ERR")?;
    if skip_decompress {
        return Ok(Box::new(reader));
    }
    let decoder = TokioZstdDecoder::new(reader);
    Ok(Box::new(decoder))
}

pub async fn prepare_target(target: &str) -> Result<Option<PathBuf>, anyhow::Error> {
    let target = Path::new(&target);
    let exe_path = std::env::current_exe().context("GET_EXE_PATH_ERR")?;
    let mut override_path = None;

    // check if target is the same as exe path
    if exe_path == target && exe_path.exists() {
        // if same, rename the exe to exe.old
        let old_exe = exe_path.with_extension("instbak");
        // delete old_exe if exists
        let _ = tokio::fs::remove_file(&old_exe).await;
        // rename current exe to old_exe
        tokio::fs::rename(&exe_path, &old_exe)
            .await
            .context("RENAME_EXE_ERR")?;
        override_path = Some(old_exe.clone());
        DELETE_SELF_ON_EXIT_PATH
            .write()
            .unwrap()
            .replace(old_exe.to_string_lossy().to_string());
    }

    // ensure dir
    let parent = target.parent().context("GET_PARENT_DIR_ERR")?;
    tokio::fs::create_dir_all(parent)
        .await
        .context("CREATE_PARENT_DIR_ERR")?;
    Ok(override_path)
}

pub async fn create_target_file(target: &str) -> Result<impl AsyncWrite, anyhow::Error> {
    let target_file = tokio::fs::File::create(target)
        .await
        .context("CREATE_TARGET_FILE_ERR")?;
    let target_file = tokio::io::BufWriter::new(target_file);
    Ok(target_file)
}

pub async fn progressed_copy(
    source: impl AsyncRead + std::marker::Unpin,
    target: impl AsyncWrite + std::marker::Unpin,
    on_progress: impl Fn(usize),
) -> Result<usize, anyhow::Error> {
    progressed_copy_with_insight(source, target, on_progress, None, true).await.map(|(size, _)| size)
}

pub async fn progressed_copy_with_insight(
    mut source: impl AsyncRead + std::marker::Unpin,
    mut target: impl AsyncWrite + std::marker::Unpin,
    on_progress: impl Fn(usize),
    mut insight: Option<InsightItem>,
    disable_timeout_check: bool,
) -> Result<(usize, Option<InsightItem>), anyhow::Error> {
    let download_start = std::time::Instant::now();
    let mut downloaded = 0;
    let mut boxed = Box::new([0u8; 256 * 1024]);
    let buffer = &mut *boxed;
    let mut now = std::time::Instant::now();
    
    // Create monitor only if timeout checking is enabled
    let mut monitor = if disable_timeout_check { None } else { Some(DownloadMonitor::new()) };
    
    loop {
        let read = source.read(buffer).await.context("DECOMPRESS_ERR")?;
        if read == 0 {
            // 网络流读取完成，更新时间统计
            if let Some(ref mut insight) = insight {
                insight.time = download_start.elapsed().as_millis() as u32;
                insight.size = downloaded as u32;
            }
            break;
        }
        downloaded += read;
        
        // Check for timeout if monitor is enabled
        if let Some(ref mut monitor) = monitor {
            if let Err(e) = monitor.check_stall(downloaded) {
                // Update insight with error information
                if let Some(ref mut insight) = insight {
                    insight.error = Some(e.to_string());
                    insight.time = download_start.elapsed().as_millis() as u32;
                    insight.size = downloaded as u32;
                }
                return Err(e);
            }
        }
        
        if now.elapsed().as_millis() >= 20 {
            now = std::time::Instant::now();
            on_progress(downloaded);
        }
        target
            .write_all(&buffer[..read])
            .await
            .context("WRITE_TARGET_ERR")?;
    }
    
    // 本地I/O操作（不计入网络时间）
    target.flush().await.context("FLUSH_TARGET_ERR")?;
    on_progress(downloaded);
    
    Ok((downloaded, insight))
}

pub async fn progressed_hpatch<R, F>(
    source: R,
    target: &str,
    diff_size: usize,
    on_progress: F,
    override_old_path: Option<PathBuf>,
    mut insight: Option<InsightItem>,
) -> Result<(usize, Option<InsightItem>), anyhow::Error>
where
    R: AsyncRead + std::marker::Unpin + Send + 'static,
    F: Fn(usize) + Send + 'static,
{
    let download_start = std::time::Instant::now();
    let mut downloaded = 0;
    
    let decoder = ReadWithCallback {
        reader: source,
        callback: move |chunk| {
            downloaded += chunk;
            on_progress(downloaded);
        },
    };
    let target = target.to_string();
    let target_cl = if let Some(override_old_path) = override_old_path.as_ref() {
        Path::new(override_old_path)
    } else {
        Path::new(&target)
    };
    let old_target_old = target_cl.with_extension("patchold");
    // try remove old_target_old, do not throw error if failed
    let _ = tokio::fs::remove_file(old_target_old).await;
    let new_target = target_cl.with_extension("patching");
    let target_size = target_cl.metadata().context("GET_TARGET_SIZE_ERR")?;
    let target_file = std::fs::File::create(new_target.clone()).context("CREATE_NEW_TARGET_ERR")?;
    let old_target_file = std::fs::File::open(
        if let Some(override_old_path) = override_old_path.as_ref() {
            override_old_path.clone()
        } else {
            PathBuf::from(target.clone())
        },
    )
    .context("OPEN_TARGET_ERR")?;
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
    .context("RUN_HPATCH_ERR")?;
    if res {
        // move target to target.old
        let old_target = target_cl.with_extension("old");
        let exe_path = std::env::current_exe().context("GET_EXE_PATH_ERR")?;
        // if old file is not self
        if exe_path != target_cl {
            // rename to .old
            tokio::fs::rename(target_cl, old_target.clone())
                .await
                .context("RENAME_TARGET_ERR")?;
            // rename new file to original
            tokio::fs::rename(new_target, target_cl)
                .await
                .context("RENAME_NEW_TARGET_ERR")?;
            // delete old file
            tokio::fs::remove_file(old_target)
                .await
                .context("REMOVE_OLD_TARGET_ERR")?;
        } else {
            if override_old_path.is_none() {
                // rename to .old
                tokio::fs::rename(target_cl, old_target.clone())
                    .await
                    .context("RENAME_TARGET_ERR")?;
            }
            // self is already renamed and cannot be deleted, just replace the new file
            tokio::fs::rename(new_target, target_cl)
                .await
                .context("RENAME_NEW_TARGET_ERR")?;
        }
    } else {
        // delete new target
        tokio::fs::remove_file(new_target)
            .await
            .context("REMOVE_NEW_TARGET_ERR")?;
        return Err(
            anyhow::Error::new(std::io::Error::other("Patch operation failed"))
                .context("PATCH_FAILED_ERR"),
        );
    }
    // 更新网络下载统计信息
    if let Some(ref mut insight) = insight {
        insight.time = download_start.elapsed().as_millis() as u32;
        insight.size = diff_size as u32;
    }
    
    Ok((diff_size, insight))
}

pub async fn verify_hash(
    target: &str,
    md5: Option<String>,
    xxh: Option<String>,
) -> Result<(), anyhow::Error> {
    let alg = if md5.is_some() {
        "md5"
    } else if xxh.is_some() {
        "xxh"
    } else {
        return Err(
            anyhow::Error::new(std::io::Error::other("No hash algorithm specified"))
                .context("NO_HASH_ALGO_ERR"),
        );
    };
    let expected = if let Some(md5) = md5 {
        md5
    } else if let Some(xxh) = xxh {
        xxh
    } else {
        return Err(
            anyhow::Error::new(std::io::Error::other("No hash data provided"))
                .context("NO_HASH_DATA_ERR"),
        );
    };
    let hash = run_hash(alg, target).await.context("HASH_CHECK_ERR")?;
    if hash != expected {
        return Err(anyhow::Error::new(std::io::Error::other(format!(
            "File {target} hash mismatch: expected {expected}, got {hash}"
        )))
        .context("HASH_MISMATCH_ERR"));
    }
    Ok(())
}
