use crate::{
    dfs::InsightItem,
    fs::{
        create_http_stream, create_local_stream, create_multi_http_stream, create_target_file,
        prepare_target, progressed_copy, progressed_hpatch, verify_hash,
    },
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
        InstallFileMode::Direct { source } => match source {
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

#[derive(Serialize, Deserialize, Debug)]
pub struct InstallResult {
    pub bytes_transferred: usize,
    pub insight: Option<InsightItem>,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(untagged)]
enum InstallFileSource {
    Url {
        url: String,
        offset: usize,
        size: usize,
        #[serde(default = "default_as_false")]
        skip_decompress: bool,
    },
    Local {
        offset: usize,
        size: usize,
        #[serde(default = "default_as_false")]
        skip_decompress: bool,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(tag = "type")]
enum InstallFileMode {
    Direct {
        source: InstallFileSource,
    },
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
    mode: InstallFileMode,
    target: String,
    md5: Option<String>,
    xxh: Option<String>,
    clear_installer_index_mark: Option<bool>,
}
async fn create_stream_by_source(
    source: InstallFileSource,
) -> Result<(
    Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>,
    Option<Arc<Mutex<InsightItem>>>,
)> {
    match source {
        InstallFileSource::Url {
            url,
            offset,
            size,
            skip_decompress,
        } => {
            let (stream, _content_length, insight_handle) =
                create_http_stream(&url, offset, size, skip_decompress).await?;
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
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> TAResult<serde_json::Value> {
    let target = args.target;
    let override_old_path = prepare_target(&target).await?;
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!(downloaded));
    };
    match args.mode {
        InstallFileMode::Direct { source } => {
            let (stream, insight_handle) = create_stream_by_source(source).await?;
            let bytes_transferred = match crate::fs::progressed_copy(
                stream,
                create_target_file(&target).await?,
                progress_noti,
            )
            .await
            {
                Ok(bytes) => bytes,
                Err(e) => {
                    if let Some(handle) = &insight_handle {
                        if let Ok(mut insight) = handle.lock() {
                            insight.error = Some(e.to_string());
                        }
                        return Err(crate::utils::error::TACommandError::with_insight_handle(
                            e,
                            handle.clone(),
                        ));
                    } else {
                        return Err(crate::utils::error::TACommandError::new(e));
                    }
                }
            };

            // 获取最终的insight
            let final_insight = if let Some(handle) = insight_handle {
                if let Ok(insight) = handle.lock() {
                    Some(insight.clone())
                } else {
                    None
                }
            } else {
                None
            };

            if args.md5.is_some() || args.xxh.is_some() {
                // 如果需要清理installer索引标记，先清理再进行hash校验
                if args.clear_installer_index_mark.unwrap_or(false) || override_old_path.is_some() {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    .into_ta_result()
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(e);
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash(&target, args.md5, args.xxh).await?;
            }

            let result = InstallResult {
                bytes_transferred,
                insight: final_insight,
            };
            serde_json::to_value(result).into_ta_result()
        }
        InstallFileMode::Patch { source, diff_size } => {
            let is_self_update = override_old_path.is_some();
            let (stream, insight_handle) = create_stream_by_source(source).await?;
            let (bytes_transferred, _) = progressed_hpatch(
                stream,
                &target,
                diff_size,
                progress_noti,
                override_old_path,
                None, // 传入None，因为现在insight由handle管理
            )
            .await?;

            // 获取最终的insight
            let final_insight = if let Some(handle) = insight_handle {
                if let Ok(insight) = handle.lock() {
                    Some(insight.clone())
                } else {
                    None
                }
            } else {
                None
            };

            if args.md5.is_some() || args.xxh.is_some() {
                // 如果需要清理installer索引标记，先清理再进行hash校验
                if args.clear_installer_index_mark.unwrap_or(false) || is_self_update {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    .into_ta_result()
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(e);
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash(&target, args.md5, args.xxh).await?;
            }

            let result = InstallResult {
                bytes_transferred,
                insight: final_insight,
            };
            serde_json::to_value(result).into_ta_result()
        }
        InstallFileMode::HybridPatch { diff, source } => {
            // first extract source (local file, no insight needed)
            let (source_stream, _) = create_stream_by_source(source).await?;
            let target_fs = create_target_file(&target).await?;
            let _source_bytes = progressed_copy(source_stream, target_fs, progress_noti).await?;

            // then apply patch (only consider diff as URL)
            let size: usize = match diff {
                InstallFileSource::Url { size, .. } => size,
                InstallFileSource::Local { size, .. } => size,
            };
            let (diff_stream, insight_handle) = create_stream_by_source(diff).await?;
            let (diff_bytes, _) =
                progressed_hpatch(diff_stream, &target, size, |_| {}, None, None).await?;

            // 获取最终的insight
            let final_insight = if let Some(handle) = insight_handle {
                if let Ok(insight) = handle.lock() {
                    Some(insight.clone())
                } else {
                    None
                }
            } else {
                None
            };

            if args.md5.is_some() || args.xxh.is_some() {
                // 如果需要清理installer索引标记，先清理再进行hash校验
                if args.clear_installer_index_mark.unwrap_or(false) || override_old_path.is_some() {
                    info!("Clearing installer index mark for: {}", target);
                    if let Err(e) = crate::installer::uninstall::clear_index_mark(
                        &std::path::PathBuf::from(&target),
                    )
                    .await
                    .into_ta_result()
                    {
                        warn!("Failed to clear index mark: {:?}", e);
                        return Err(e);
                    }
                    info!("Index mark cleared successfully");
                }
                verify_hash(&target, args.md5, args.xxh).await?;
            }

            let result = InstallResult {
                bytes_transferred: diff_bytes, // 只统计diff文件的网络传输
                insight: final_insight,        // 只统计diff文件的网络统计
            };
            serde_json::to_value(result).into_ta_result()
        }
    }
}

pub async fn install_file_by_reader<C>(
    args: InstallFileArgs,
    reader: &mut C,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<serde_json::Value>
where
    C: tokio::io::AsyncRead + Unpin + std::marker::Send,
{
    let target = args.target;
    let override_old_path = prepare_target(&target).await?;
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!(downloaded));
    };
    match args.mode {
        InstallFileMode::Direct { .. } => {
            let res =
                progressed_copy(reader, create_target_file(&target).await?, progress_noti).await?;
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
            Ok(serde_json::json!(res))
        }
        InstallFileMode::Patch { diff_size, .. } => {
            // copy to local buffer using progressed_copy
            let mut buffer: Vec<u8> = vec![0; diff_size];
            progressed_copy(reader, &mut buffer, progress_noti).await?;
            let reader = std::io::Cursor::new(buffer);
            let is_self_update = override_old_path.is_some();
            let res =
                progressed_hpatch(reader, &target, diff_size, |_| {}, override_old_path, None)
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
            Ok(serde_json::json!(res))
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
    url: String,
    range: String,
    chunks: Vec<InstallFileArgs>,
}
pub async fn ipc_install_multipart_stream(
    args: InstallMultiStreamArgs,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static + Clone,
) -> TAResult<serde_json::Value> {
    let (http_stream, content_length, content_type, insight_handle) =
        create_multi_http_stream(&args.url, &args.range).await?;
    // check if content-type is multipart
    if content_type.starts_with("multipart/") {
        // get boundary from content-type: multipart/byteranges; boundary=
        let boundary = content_type.split("boundary=").nth(1).ok_or_else(|| {
            crate::utils::error::TACommandError::new(anyhow::anyhow!(
                "Content-Type does not contain boundary"
            ))
        })?;
        let boundary = boundary.split(';').next().unwrap_or(boundary).trim();

        // Create multipart reader
        let mut multipart = multer::Multipart::new(http_stream, boundary);

        // Process multipart stream
        let mut mult_res = Vec::new();
        let mut chunk_index = 0usize;
        while let Some(mut field) = multipart.next_field().await.map_err(|e| {
            crate::utils::error::TACommandError::new(anyhow::anyhow!(
                "Multipart parsing error: {}",
                e
            ))
        })? {
            // field should has Content-Range
            let content_range = field
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    crate::utils::error::TACommandError::new(anyhow::anyhow!(
                        "Field does not contain Content-Range"
                    ))
                })?;

            // Parse content_range and match with corresponding chunk
            // content_range format: bytes start-end/total
            let parts: Vec<&str> = content_range.split('/').collect();
            // must have the first part as range
            if parts.is_empty() {
                return Err(crate::utils::error::TACommandError::new(anyhow::anyhow!(
                    "Invalid Content-Range format: {}",
                    content_range
                )));
            }
            let range = parts[0]
                .split("bytes ")
                .nth(1)
                .ok_or_else(|| {
                    crate::utils::error::TACommandError::new(anyhow::anyhow!(
                        "Content-Range does not contain range: {}",
                        content_range
                    ))
                })?
                .trim();
            let range_parts: Vec<&str> = range.split('-').collect();
            if range_parts.len() != 2 {
                return Err(crate::utils::error::TACommandError::new(anyhow::anyhow!(
                    "Invalid range format in Content-Range: {}",
                    content_range
                )));
            }
            let start: usize = range_parts[0].parse().map_err(|_| {
                crate::utils::error::TACommandError::new(anyhow::anyhow!(
                    "Invalid start range: {}",
                    content_range
                ))
            })?;
            let end: usize = range_parts[1].parse().map_err(|_| {
                crate::utils::error::TACommandError::new(anyhow::anyhow!(
                    "Invalid end range: {}",
                    content_range
                ))
            })?;

            // Match the chunk with the corresponding range
            let chunk = args
                .chunks
                .iter()
                .find(|c| {
                    let source_size = get_chunk_size(c);
                    let source_pos = get_chunk_position(c);
                    let source_target = source_pos + source_size - 1;
                    start == source_pos && end == source_target
                })
                .ok_or_else(|| {
                    crate::utils::error::TACommandError::new(anyhow::anyhow!(
                        "No matching chunk found for range: {}",
                        content_range
                    ))
                })?;

            // Create enhanced notification callback with chunk info
            let chunk_range = format!("{start}-{end}");
            let current_chunk_index = chunk_index;
            let chunk_notify = {
                let notify = notify.clone();
                let chunk_range = chunk_range.clone();
                move |progress: serde_json::Value| {
                    notify(serde_json::json!({
                        "progress": progress,
                        "chunk_index": current_chunk_index,
                        "chunk_range": chunk_range
                    }));
                }
            };

            // 获取chunk的skip_decompress参数
            let should_decompress = should_decompress_chunk(chunk);

            // Read field data
            let mut field_data = Vec::new();
            while let Some(chunk_bytes) = field.chunk().await.map_err(|e| {
                if let Ok(mut insight) = insight_handle.lock() {
                    insight.error = Some(e.to_string());
                }
                crate::utils::error::TACommandError::with_insight_handle(
                    anyhow::anyhow!("Field chunk read error: {}", e),
                    insight_handle.clone(),
                )
            })? {
                field_data.extend_from_slice(&chunk_bytes);
            }

            // Create reader from collected field data
            let reader = std::io::Cursor::new(field_data);

            // 根据参数决定是否解压缩并安装chunk (disable timeout in install_file_by_reader)
            let chunk_result = if should_decompress {
                let mut decompressed_reader = TokioZstdDecoder::new(reader);
                install_file_by_reader(chunk.clone(), &mut decompressed_reader, chunk_notify)
                    .await
                    .into_ta_result()
            } else {
                let mut raw_reader = reader;
                install_file_by_reader(chunk.clone(), &mut raw_reader, chunk_notify)
                    .await
                    .into_ta_result()
            };

            mult_res.push(chunk_result);

            chunk_index += 1;
        }
        // 获取最终的insight统计
        let final_insight = if let Ok(insight) = insight_handle.lock() {
            insight.clone()
        } else {
            InsightItem {
                url: args.url.clone(),
                ttfb: 0,
                time: 0,
                size: content_length as u32,
                error: Some("Failed to get insight".to_string()),
                range: vec![],
                mode: None,
            }
        };

        let response = serde_json::json!({
            "results": mult_res,
            "insight": final_insight
        });
        Ok(response)
    } else {
        // server does not support multipart range, maybe it returns the first chunk only
        if let Some(first_chunk) = args.chunks.first() {
            // check if size equals to content-length
            let source_size = get_chunk_size(first_chunk);
            let source_pos = get_chunk_position(first_chunk);
            if content_length == source_size as u64 {
                // 获取first_chunk的skip_decompress参数
                let should_decompress = should_decompress_chunk(first_chunk);

                // proceed with the first chunk
                let stream = http_stream.map_err(std::io::Error::other);
                let reader = tokio_util::io::StreamReader::new(stream);

                // Create enhanced notification callback for the first chunk
                let chunk_notify = {
                    let notify = notify.clone();
                    move |progress: serde_json::Value| {
                        notify(serde_json::json!({
                            "progress": progress,
                            "chunk_index": 0,
                            "chunk_range": format!("{}-{}", source_pos, source_pos + source_size - 1)
                        }));
                    }
                };

                // 根据参数决定是否解压缩
                let res = if should_decompress {
                    let mut decompressed_reader = TokioZstdDecoder::new(reader);
                    install_file_by_reader(
                        first_chunk.clone(),
                        &mut decompressed_reader,
                        chunk_notify,
                    )
                    .await
                    .into_ta_result()
                } else {
                    let mut raw_reader = reader;
                    install_file_by_reader(first_chunk.clone(), &mut raw_reader, chunk_notify)
                        .await
                        .into_ta_result()
                };

                // 获取最终的insight统计
                let final_insight = if let Ok(insight) = insight_handle.lock() {
                    insight.clone()
                } else {
                    InsightItem {
                        url: args.url.clone(),
                        ttfb: 0,
                        time: 0,
                        size: content_length as u32,
                        error: Some("Failed to get insight".to_string()),
                        range: vec![],
                        mode: None,
                    }
                };

                let response = serde_json::json!({
                    "results": vec![res],
                    "insight": final_insight
                });
                Ok(response)
            } else {
                Err(crate::utils::error::TACommandError::new(anyhow::anyhow!(
                    "Server does not support multipart range, and cannot send the first chunk correctly (expected size: {}, got: {})",
                    source_size,
                    content_length
                )))
            }
        } else {
            Err(crate::utils::error::TACommandError::new(anyhow::anyhow!(
                "No chunks provided for multi-stream installation"
            )))
        }
    }
}

// Helper function to extract chunk size from InstallFileArgs
fn get_chunk_size(args: &InstallFileArgs) -> usize {
    match &args.mode {
        InstallFileMode::Direct { source } => match source {
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
        InstallFileMode::Direct { source } => match source {
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
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static + Clone,
) -> TAResult<serde_json::Value> {
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

    let mut results: Vec<TAResult<serde_json::Value>> = Vec::new();
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
        let chunk_range = format!("{}-{}", chunk_offset, chunk_offset + chunk_size - 1);
        let chunk_notify = {
            let notify = notify.clone();
            let chunk_range = chunk_range.clone();
            move |progress: serde_json::Value| {
                notify(serde_json::json!({
                    "progress": progress,
                    "chunk_index": chunk_index,
                    "chunk_range": chunk_range
                }));
            }
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
                        insight.error = Some(e.to_string());
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
                insight.error = Some(e.to_string());
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
                insight.error = Some(e.to_string());
            }
        });

        results.push(final_result);
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

    let response = serde_json::json!({
        "results": results,
        "insight": final_insight
    });
    Ok(response)
}
