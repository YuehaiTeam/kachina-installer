use crate::{
    dfs::{apply_insight_error, InsightItem},
    fs::{
        create_http_stream, create_local_stream, create_multi_http_stream, create_target_file,
        prepare_target, progressed_copy, progressed_hpatch, verify_hash,
    },
    ipc::{progress_notify, IpcError, Progress, ProgressNotify},
    utils::error::{IntoTAResult, TAResult},
};

use anyhow::Result;
use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, BufReader};
use tracing::{info, warn};

fn default_as_false() -> bool {
    false
}

// Helper function to check if decompression should be performed based on InstallFileArgs
fn should_decompress_chunk(args: &InstallFileArgs) -> bool {
    match &args.mode {
        InstallFileMode::Direct(source) => match source {
            InstallFileSource::Url {
                skip_decompress, ..
            } => !skip_decompress,
            InstallFileSource::Local {
                skip_decompress, ..
            } => !skip_decompress,
        },
        InstallFileMode::Patch { source, .. } => match source {
            InstallFileSource::Url {
                skip_decompress, ..
            } => !skip_decompress,
            InstallFileSource::Local {
                skip_decompress, ..
            } => !skip_decompress,
        },
        InstallFileMode::HybridPatch { diff, .. } => match diff {
            InstallFileSource::Url {
                skip_decompress, ..
            } => !skip_decompress,
            InstallFileSource::Local {
                skip_decompress, ..
            } => !skip_decompress,
        },
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstallResult {
    pub bytes_transferred: usize,
    pub insight: Option<InsightItem>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MultichunkResult {
    pub results: Vec<Result<usize, IpcError>>,
    pub insight: InsightItem,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub enum InstallFileSource {
    Url {
        url: String,
        offset: usize,
        size: usize,
        #[serde(default = "default_as_false")]
        skip_decompress: bool,
        #[serde(default)]
        request_range: Option<String>,
    },
    Local {
        offset: usize,
        size: usize,
        #[serde(default = "default_as_false")]
        skip_decompress: bool,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub enum InstallFileMode {
    Direct(InstallFileSource),
    Patch {
        source: InstallFileSource,
        diff_size: usize,
    },
    HybridPatch {
        diff: InstallFileSource,
        source: InstallFileSource,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct InstallFileArgs {
    pub mode: InstallFileMode,
    pub target: String,
    pub md5: Option<String>,
    pub xxh: Option<String>,
    pub clear_installer_index_mark: Option<bool>,
}
fn snapshot_insight(handle: &Option<Arc<Mutex<InsightItem>>>) -> Option<InsightItem> {
    handle
        .as_ref()
        .and_then(|h| h.lock().ok().map(|insight| insight.clone()))
}

fn fail_with_insight(
    err: anyhow::Error,
    handle: &Option<Arc<Mutex<InsightItem>>>,
) -> crate::utils::error::TACommandError {
    if let Some(handle) = handle {
        if let Ok(mut insight) = handle.lock() {
            apply_insight_error(&mut insight, &err.to_string());
        }
        crate::utils::error::TACommandError::with_insight_handle(err, handle.clone())
    } else {
        crate::utils::error::TACommandError::new(err)
    }
}

fn attach_insight(
    mut err: crate::utils::error::TACommandError,
    handle: &Option<Arc<Mutex<InsightItem>>>,
) -> crate::utils::error::TACommandError {
    if err.insight.is_none() {
        err.insight = snapshot_insight(handle);
    }
    err
}

async fn verify_hash_keep_insight(
    target: &str,
    md5: Option<String>,
    xxh: Option<String>,
    handle: &Option<Arc<Mutex<InsightItem>>>,
) -> TAResult<()> {
    match verify_hash(target, md5, xxh).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Some(handle) = handle {
                if let Ok(mut insight) = handle.lock() {
                    let code = crate::dfs::short_insight_code(&format!("{e:#}"));
                    insight.error = Some(if code == "ERR_NETWORK_OTHER" {
                        "HASH_MISMATCH_ERR".to_string()
                    } else {
                        code
                    });
                }
                return Err(crate::utils::error::TACommandError::with_insight_handle(
                    e,
                    handle.clone(),
                ));
            }
            Err(crate::utils::error::TACommandError::new(e))
        }
    }
}

async fn create_stream_by_source(
    source: InstallFileSource,
) -> TAResult<(
    Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>,
    Option<Arc<Mutex<InsightItem>>>,
)> {
    match source {
        InstallFileSource::Url {
            url,
            offset,
            size,
            skip_decompress,
            request_range,
        } => {
            let (stream, _content_length, insight_handle) = create_http_stream(
                &url,
                offset,
                size,
                skip_decompress,
                request_range.as_deref(),
            )
            .await?;
            Ok((stream, Some(insight_handle)))
        }
        InstallFileSource::Local {
            offset,
            size,
            skip_decompress,
        } => Ok((
            create_local_stream(offset, size, skip_decompress).await?,
            None,
        )),
    }
}
pub async fn ipc_install_file(
    args: InstallFileArgs,
    notify: ProgressNotify,
) -> TAResult<InstallResult> {
    let target = args.target;
    let override_old_path = prepare_target(&target).await?;
    let progress_noti = move |downloaded: usize| {
        notify(Progress::Bytes(downloaded as u64));
    };
    match args.mode {
        InstallFileMode::Direct(source) => {
            let (mut stream, insight_handle) = create_stream_by_source(source).await?;
            let mut target_fs = create_target_file(&target).await?;
            let bytes_transferred =
                match crate::fs::progressed_copy(stream.as_mut(), &mut target_fs, &progress_noti)
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(e) => return Err(fail_with_insight(e, &insight_handle)),
                };

            let final_insight = snapshot_insight(&insight_handle);

            if args.md5.is_some() || args.xxh.is_some() {
                if args.clear_installer_index_mark.unwrap_or(false) || override_old_path.is_some() {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    .into_ta_result()
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(attach_insight(e, &insight_handle));
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash_keep_insight(&target, args.md5, args.xxh, &insight_handle).await?;
            }

            Ok(InstallResult {
                bytes_transferred,
                insight: final_insight,
            })
        }
        InstallFileMode::Patch { source, diff_size } => {
            let is_self_update = override_old_path.is_some();
            let (stream, insight_handle) = create_stream_by_source(source).await?;
            let (bytes_transferred, _) = match progressed_hpatch(
                stream,
                &target,
                diff_size,
                Box::new(progress_noti),
                override_old_path,
                None,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => return Err(fail_with_insight(e, &insight_handle)),
            };

            let final_insight = snapshot_insight(&insight_handle);

            if args.md5.is_some() || args.xxh.is_some() {
                if args.clear_installer_index_mark.unwrap_or(false) || is_self_update {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    .into_ta_result()
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(attach_insight(e, &insight_handle));
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash_keep_insight(&target, args.md5, args.xxh, &insight_handle).await?;
            }

            Ok(InstallResult {
                bytes_transferred,
                insight: final_insight,
            })
        }
        InstallFileMode::HybridPatch { diff, source } => {
            // first extract source (local file, no insight needed)
            let (mut source_stream, _) = create_stream_by_source(source).await?;
            let mut target_fs = create_target_file(&target).await?;
            let _source_bytes =
                progressed_copy(source_stream.as_mut(), &mut target_fs, &progress_noti).await?;

            // then apply patch (only consider diff as URL)
            let size: usize = match diff {
                InstallFileSource::Url { size, .. } => size,
                InstallFileSource::Local { size, .. } => size,
            };
            let (diff_stream, insight_handle) = create_stream_by_source(diff).await?;
            let (diff_bytes, _) =
                match progressed_hpatch(diff_stream, &target, size, Box::new(|_| {}), None, None)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return Err(fail_with_insight(e, &insight_handle)),
                };

            let final_insight = snapshot_insight(&insight_handle);

            if args.md5.is_some() || args.xxh.is_some() {
                if args.clear_installer_index_mark.unwrap_or(false) || override_old_path.is_some() {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    .into_ta_result()
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(attach_insight(e, &insight_handle));
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash_keep_insight(&target, args.md5, args.xxh, &insight_handle).await?;
            }

            Ok(InstallResult {
                bytes_transferred: diff_bytes,
                insight: final_insight,
            })
        }
    }
}

pub async fn install_file_by_reader(
    args: InstallFileArgs,
    reader: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
    notify: ProgressNotify,
) -> Result<usize> {
    let target = args.target;
    let override_old_path = prepare_target(&target).await?;
    let progress_noti = move |downloaded: usize| {
        notify(Progress::Bytes(downloaded as u64));
    };
    match args.mode {
        InstallFileMode::Direct(..) => {
            let mut target_fs = create_target_file(&target).await?;
            let res = progressed_copy(reader, &mut target_fs, &progress_noti).await?;
            if args.md5.is_some() || args.xxh.is_some() {
                // 如果需要清理installer索引标记，先清理再进行hash校验
                if args.clear_installer_index_mark.unwrap_or(false) || override_old_path.is_some() {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(e);
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash(&target, args.md5, args.xxh).await?;
            }
            Ok(res)
        }
        InstallFileMode::Patch { diff_size, .. } => {
            // copy to local buffer using progressed_copy
            let mut buffer: Vec<u8> = vec![0; diff_size];
            progressed_copy(reader, &mut buffer, &progress_noti).await?;
            let reader = std::io::Cursor::new(buffer);
            let is_self_update = override_old_path.is_some();
            let res = progressed_hpatch(
                Box::new(reader),
                &target,
                diff_size,
                Box::new(|_| {}),
                override_old_path,
                None,
            )
            .await?
            .0;
            if args.md5.is_some() || args.xxh.is_some() {
                // 如果需要清理installer索引标记，先清理再进行hash校验
                if args.clear_installer_index_mark.unwrap_or(false) || is_self_update {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(e);
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash(&target, args.md5, args.xxh).await?;
            }
            Ok(res)
        }
        InstallFileMode::HybridPatch { .. } => {
            // Hybrid patch is not supported in this function
            Err(anyhow::anyhow!(
                "Hybrid patch is not supported in this function"
            ))
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct InstallMultiStreamArgs {
    pub url: String,
    pub range: String,
    pub chunks: Vec<InstallFileArgs>,
}
// Helper function to extract chunk size from InstallFileArgs
fn get_chunk_size(args: &InstallFileArgs) -> usize {
    match &args.mode {
        InstallFileMode::Direct(source) => match source {
            InstallFileSource::Url { size, .. } | InstallFileSource::Local { size, .. } => *size,
        },
        InstallFileMode::Patch { diff_size, .. } => *diff_size,
        InstallFileMode::HybridPatch { diff, .. } => match diff {
            InstallFileSource::Url { size, .. } | InstallFileSource::Local { size, .. } => *size,
        },
    }
}

// Helper function to extract chunk position from InstallFileArgs
fn get_chunk_position(args: &InstallFileArgs) -> usize {
    match &args.mode {
        InstallFileMode::Direct(source) => match source {
            InstallFileSource::Url { offset, .. } | InstallFileSource::Local { offset, .. } => {
                *offset
            }
        },
        InstallFileMode::Patch { source, .. } => match source {
            InstallFileSource::Url { offset, .. } | InstallFileSource::Local { offset, .. } => {
                *offset
            }
        },
        InstallFileMode::HybridPatch { diff, .. } => match diff {
            InstallFileSource::Url { offset, .. } | InstallFileSource::Local { offset, .. } => {
                *offset
            }
        },
    }
}

#[derive(Debug, Clone)]
struct ChunkWithPosition {
    position: usize,
    args: InstallFileArgs,
}

pub async fn ipc_install_multichunk_stream(
    args: InstallMultiStreamArgs,
    notify: ProgressNotify,
) -> TAResult<MultichunkResult> {
    // Extract chunk positions from InstallFileArgs
    let mut chunks_with_positions: Vec<ChunkWithPosition> = Vec::new();

    for chunk in &args.chunks {
        let position = get_chunk_position(chunk);
        chunks_with_positions.push(ChunkWithPosition {
            position,
            args: chunk.clone(),
        });
    }

    // Sort chunks by position to ensure proper streaming order
    chunks_with_positions.sort_by_key(|chunk| chunk.position);

    let mut results: Vec<Result<usize, IpcError>> = Vec::new();
    let mut stream_position = 0usize;
    let (insight_stream, _content_length, _content_type, insight_handle) =
        create_multi_http_stream(&args.url, &args.range).await?;

    // Convert the HTTP stream to AsyncRead
    let stream = insight_stream.map_err(std::io::Error::other);
    let mut reader = tokio_util::io::StreamReader::new(stream);

    for (chunk_index, chunk_info) in chunks_with_positions.iter().enumerate() {
        let chunk_size = get_chunk_size(&chunk_info.args);
        let chunk_offset = chunk_info.position;

        // Create enhanced notification callback with chunk info
        let chunk_notify = {
            let notify = notify.clone();
            let chunk_index = chunk_index as u32;
            progress_notify(move |progress| {
                if let Progress::Bytes(bytes) = progress {
                    notify(Progress::Chunk(chunk_index, bytes));
                }
            })
        };

        // Skip bytes until we reach the chunk position
        if stream_position < chunk_info.position {
            let skip_bytes = chunk_info.position - stream_position;
            let mut buffer = vec![0u8; 8192]; // 8KB buffer
            let mut remaining = skip_bytes;

            while remaining > 0 {
                let to_read = std::cmp::min(buffer.len(), remaining);
                let bytes_read = reader.read(&mut buffer[..to_read]).await.map_err(|e| {
                    if let Ok(mut insight) = insight_handle.lock() {
                        apply_insight_error(&mut insight, &e.to_string());
                    }
                    crate::utils::error::TACommandError::with_insight_handle(
                        anyhow::anyhow!("Failed to skip bytes: {}", e),
                        insight_handle.clone(),
                    )
                })?;

                if bytes_read == 0 {
                    return Err(crate::utils::error::TACommandError::with_insight_handle(
                        anyhow::anyhow!("Unexpected EOF while skipping bytes"),
                        insight_handle.clone(),
                    ));
                }

                remaining -= bytes_read;
            }

            stream_position = chunk_offset;
        }

        // Process chunk
        let should_decompress = should_decompress_chunk(&chunk_info.args);

        // Read chunk data into memory buffer first
        let mut chunk_buffer = vec![0u8; chunk_size];
        reader.read_exact(&mut chunk_buffer).await.map_err(|e| {
            if let Ok(mut insight) = insight_handle.lock() {
                apply_insight_error(&mut insight, &e.to_string());
            }
            crate::utils::error::TACommandError::with_insight_handle(
                anyhow::anyhow!("Failed to read chunk data: {}", e),
                insight_handle.clone(),
            )
        })?;

        let chunk_reader = std::io::Cursor::new(chunk_buffer);

        // Process chunk directly without timeout monitoring (NetworkInsightStream handles it)
        let chunk_result = if should_decompress {
            let buf_reader = BufReader::new(chunk_reader);
            let mut decompressed_reader = TokioZstdDecoder::new(buf_reader);
            install_file_by_reader(
                chunk_info.args.clone(),
                &mut decompressed_reader,
                chunk_notify,
            )
            .await
            .into_ta_result()
        } else {
            let mut raw_reader = chunk_reader;
            install_file_by_reader(chunk_info.args.clone(), &mut raw_reader, chunk_notify)
                .await
                .into_ta_result()
        };

        // Handle chunk result and update insight if there's an error
        let final_result = chunk_result.inspect_err(|e| {
            if let Ok(mut insight) = insight_handle.lock() {
                apply_insight_error(&mut insight, &e.to_string());
            }
        });

        results.push(match final_result {
            Ok(n) => Ok(n),
            Err(e) => Err(IpcError::from_ta(&e)),
        });
        stream_position += chunk_size;
    }

    // 获取最终的insight统计
    let final_insight = if let Ok(insight) = insight_handle.lock() {
        insight.clone()
    } else {
        InsightItem {
            url: args.url.clone(),
            ttfb: 0,
            time: 0,
            size: 0,
            error: Some("Failed to get insight".to_string()),
            range: vec![],
            mode: None,
        }
    };

    Ok(MultichunkResult {
        results,
        insight: final_insight,
    })
}
