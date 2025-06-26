use crate::{
    fs::{
        create_http_stream, create_local_stream, create_multi_http_stream, create_target_file,
        prepare_target, progressed_copy, progressed_hpatch, verify_hash,
    },
    utils::error::{IntoTAResult, TAResult},
};

use anyhow::Result;
use futures::TryStreamExt;
use tokio::io::AsyncReadExt;

fn default_as_false() -> bool {
    false
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
}
async fn create_stream_by_source(
    source: InstallFileSource,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>> {
    match source {
        InstallFileSource::Url {
            url,
            offset,
            size,
            skip_decompress,
        } => Ok(create_http_stream(&url, offset, size, skip_decompress)
            .await?
            .0),
        InstallFileSource::Local {
            offset,
            size,
            skip_decompress,
        } => Ok(create_local_stream(offset, size, skip_decompress).await?),
    }
}
pub async fn ipc_install_file(
    args: InstallFileArgs,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<serde_json::Value> {
    let target = args.target;
    let override_old_path = prepare_target(&target).await?;
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!(downloaded));
    };
    match args.mode {
        InstallFileMode::Direct { source } => {
            let res = progressed_copy(
                create_stream_by_source(source).await?,
                create_target_file(&target).await?,
                progress_noti,
            )
            .await?;
            if args.md5.is_some() || args.xxh.is_some() {
                verify_hash(&target, args.md5, args.xxh).await?;
            }
            Ok(serde_json::json!(res))
        }
        InstallFileMode::Patch { source, diff_size } => {
            let res = progressed_hpatch(
                create_stream_by_source(source).await?,
                &target,
                diff_size,
                progress_noti,
                override_old_path,
            )
            .await?;
            if args.md5.is_some() || args.xxh.is_some() {
                verify_hash(&target, args.md5, args.xxh).await?;
            }
            Ok(serde_json::json!(res))
        }
        InstallFileMode::HybridPatch { diff, source } => {
            // first extract source
            let source = create_stream_by_source(source).await?;
            let target_fs = create_target_file(&target).await?;
            progressed_copy(source, target_fs, progress_noti).await?;
            // then apply patch
            let size: usize = match diff {
                InstallFileSource::Url { size, .. } => size,
                InstallFileSource::Local { size, .. } => size,
            };
            progressed_hpatch(
                create_stream_by_source(diff).await?,
                &target,
                size,
                |_| {},
                None,
            )
            .await?;
            if args.md5.is_some() || args.xxh.is_some() {
                verify_hash(&target, args.md5, args.xxh).await?;
            }
            Ok(serde_json::json!(()))
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
                verify_hash(&target, args.md5, args.xxh).await?;
            }
            Ok(serde_json::json!(res))
        }
        InstallFileMode::Patch { diff_size, .. } => {
            // copy to local buffer using progressed_copy
            let mut buffer: Vec<u8> = vec![0; diff_size];
            progressed_copy(reader, &mut buffer, progress_noti).await?;
            let reader = std::io::Cursor::new(buffer);
            let res =
                progressed_hpatch(reader, &target, diff_size, |_| {}, override_old_path).await?;
            if args.md5.is_some() || args.xxh.is_some() {
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
) -> Result<serde_json::Value> {
    let (http_stream, content_length, content_type) =
        create_multi_http_stream(&args.url, &args.range).await?;
    // check if content-type is multipart
    if content_type.starts_with("multipart/") {
        // get boundary from content-type: multipart/byteranges; boundary=
        let boundary = content_type
            .split("boundary=")
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("Content-Type does not contain boundary"))?;
        let boundary = boundary.split(';').next().unwrap_or(boundary).trim();

        // Create multipart reader
        let mut multipart = multer::Multipart::new(http_stream, boundary);

        // Process multipart stream and handle chunks
        let mut mult_res = Vec::new();
        let mut chunk_index = 0usize;
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| anyhow::anyhow!("Multipart parsing error: {}", e))?
        {
            // field should has Content-Range
            let content_range = field
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("Field does not contain Content-Range"))?;

            // Parse content_range and match with corresponding chunk
            // content_range format: bytes start-end/total
            let parts: Vec<&str> = content_range.split('/').collect();
            // must have the first part as range
            if parts.is_empty() {
                return Err(anyhow::anyhow!(
                    "Invalid Content-Range format: {}",
                    content_range
                ));
            }
            let range = parts[0]
                .split("bytes ")
                .nth(1)
                .ok_or_else(|| {
                    anyhow::anyhow!("Content-Range does not contain range: {}", content_range)
                })?
                .trim();
            let range_parts: Vec<&str> = range.split('-').collect();
            if range_parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid range format in Content-Range: {}",
                    content_range
                ));
            }
            let start: usize = range_parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid start range: {}", content_range))?;
            let end: usize = range_parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid end range: {}", content_range))?;

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
                    anyhow::anyhow!("No matching chunk found for range: {}", content_range)
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

            // Map multer::Error to std::io::Error for StreamReader compatibility
            let stream = field.into_stream();
            let stream = stream.map_err(std::io::Error::other);
            let mut reader = tokio_util::io::StreamReader::new(stream);
            // Install the chunk using the reader
            mult_res.push(
                install_file_by_reader(chunk.clone(), &mut reader, chunk_notify)
                    .await
                    .into_ta_result(),
            );

            chunk_index += 1;
        }
        Ok(serde_json::json!(mult_res))
    } else {
        // server does not support multipart range, maybe it returns the first chunk only
        if let Some(first_chunk) = args.chunks.first() {
            // check if size equals to content-length
            let source_size = get_chunk_size(first_chunk);
            let source_pos = get_chunk_position(first_chunk);
            if content_length == source_size as u64 {
                // proceed with the first chunk
                let stream = http_stream.map_err(std::io::Error::other);
                let mut reader = tokio_util::io::StreamReader::new(stream);

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

                let res = install_file_by_reader(first_chunk.clone(), &mut reader, chunk_notify)
                    .await
                    .into_ta_result();
                let mult_reslut = vec![res];
                Ok(serde_json::json!(mult_reslut))
            } else {
                Err(anyhow::anyhow!(
                    "Server does not support multipart range, and cannot send the first chunk correctly (expected size: {}, got: {})",
                    source_size,
                    content_length
                ))
            }
        } else {
            Err(anyhow::anyhow!(
                "No chunks provided for multi-stream installation"
            ))
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
) -> Result<serde_json::Value> {
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
    let (http_stream, _content_length, _content_type) =
        create_multi_http_stream(&args.url, &args.range).await?;

    // Convert the HTTP stream to AsyncRead
    let stream = http_stream.map_err(std::io::Error::other);
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
            reader
                .read_exact(&mut vec![0; skip_bytes])
                .await
                .map_err(|e| anyhow::anyhow!("Failed to skip bytes: {}", e))?;
            stream_position = chunk_offset;
        }

        let mut chunk_reader = (&mut reader).take(chunk_size as u64);
        let chunk_result =
            install_file_by_reader(chunk_info.args.clone(), &mut chunk_reader, chunk_notify)
                .await
                .into_ta_result();
        results.push(chunk_result);
        stream_position += chunk_size;
    }

    Ok(serde_json::json!(results))
}
