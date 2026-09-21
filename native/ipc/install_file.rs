use crate::utils::code::Attach;
use crate::{
    dfs::{apply_insight_error, InsightItem},
    fs::{
        create_http_stream, create_local_stream, create_staged_file, progressed_copy,
        progressed_hpatch, sync_staged_file, verify_hash,
    },
    ipc::{progress_notify, IpcError, Progress, ProgressNotify},
    utils::error::TAResult,
};

use super::file_progress::Reporter;
use crate::session::state::FileAction;
use anyhow::Result;
use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, BufReader};
use tracing::info;

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
            }
            | InstallFileSource::Sliced {
                skip_decompress, ..
            } => !skip_decompress,
        },
        InstallFileMode::Patch { source, .. } => match source {
            InstallFileSource::Url {
                skip_decompress, ..
            } => !skip_decompress,
            InstallFileSource::Local {
                skip_decompress, ..
            }
            | InstallFileSource::Sliced {
                skip_decompress, ..
            } => !skip_decompress,
        },
        InstallFileMode::HybridPatch { diff, .. } => match diff {
            InstallFileSource::Url {
                skip_decompress, ..
            } => !skip_decompress,
            InstallFileSource::Local {
                skip_decompress, ..
            }
            | InstallFileSource::Sliced {
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
    Sliced {
        parts: Vec<(u64, u64)>,
        skip_decompress: bool,
    },
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
        diff_size: usize,
        base_size: u64,
    },
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct InstallFileArgs {
    pub mode: InstallFileMode,
    /// Output path under the staging directory's `new\`. Never a path inside
    /// the install directory.
    pub target: String,
    pub output_size: u64,
    /// The file currently in the install directory; the base for `Patch`.
    pub old: Option<String>,
    pub md5: Option<String>,
    pub xxh: Option<String>,
    pub clear_installer_index_mark: Option<bool>,
}

/// Post-write steps shared by every mode: clear the packed index mark when
/// asked, verify the hash, flush to disk. Any failure deletes the staged file.
async fn finalize_staged(args: &InstallFileArgs, target: &Path, reporter: &Reporter) -> Result<()> {
    let res = async {
        reporter.action(FileAction::Verify, None);
        if args.md5.is_some() || args.xxh.is_some() {
            if args.clear_installer_index_mark.unwrap_or(false) {
                info!("Clearing installer index mark for: {}", target.display());
                crate::installer::uninstall::clear_index_mark(&target.to_path_buf()).await?;
            }
            verify_hash(
                &target.to_string_lossy(),
                args.md5.clone(),
                args.xxh.clone(),
            )
            .await?;
        }
        reporter.action(FileAction::Flush, None);
        sync_staged_file(target).await
    }
    .await;
    if res.is_err() {
        let _ = tokio::fs::remove_file(target).await;
    }
    res
}

struct TemporaryFile(PathBuf);
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn old_path(args: &InstallFileArgs) -> Result<PathBuf> {
    args.old
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("patch without an old file"))
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
            apply_insight_error(&mut insight, &err);
        }
        crate::utils::error::TACommandError::with_insight_handle(err, handle.clone())
    } else {
        crate::utils::error::TACommandError::new(err)
    }
}

async fn finalize_keep_insight(
    args: &InstallFileArgs,
    target: &Path,
    handle: &Option<Arc<Mutex<InsightItem>>>,
    reporter: &Reporter,
) -> TAResult<()> {
    match finalize_staged(args, target, reporter).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // verify_hash hangs HASH_MISMATCH itself; anything else here is a local io failure.
            let e = e.attach(crate::utils::code::FILE_IO_FAILED);
            if let Some(handle) = handle {
                if let Ok(mut insight) = handle.lock() {
                    apply_insight_error(&mut insight, &e);
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
        InstallFileSource::Sliced {
            parts,
            skip_decompress,
        } => {
            if let [(offset, len)] = parts.as_slice() {
                let url = super::download::resolve(*offset, *len).await?;
                let (reader, _, insight) = create_http_stream(
                    &url,
                    *offset as usize,
                    *len as usize,
                    skip_decompress,
                    None,
                )
                .await?;
                return Ok((reader, Some(insight)));
            }
            let reader = super::download::sliced(parts);
            let reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> = if skip_decompress {
                reader
            } else {
                Box::new(TokioZstdDecoder::new(BufReader::new(reader)))
            };
            Ok((reader, None))
        }
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
            Box::new(super::network::Reader::new(
                create_local_stream(offset, size, skip_decompress).await?,
                super::network::Reading::default(),
                None,
                None,
            )),
            None,
        )),
    }
}
fn work_total(args: &InstallFileArgs) -> u64 {
    match &args.mode {
        InstallFileMode::Direct(_) => args.output_size,
        InstallFileMode::Patch { diff_size, .. } => *diff_size as u64,
        InstallFileMode::HybridPatch {
            diff_size,
            base_size,
            ..
        } => *diff_size as u64 + base_size,
    }
}
fn source_action(source: &InstallFileSource) -> FileAction {
    if matches!(source, InstallFileSource::Local { .. }) {
        FileAction::Extract
    } else {
        FileAction::Download
    }
}

pub async fn ipc_install_file(
    args: InstallFileArgs,
    notify: ProgressNotify,
) -> TAResult<InstallResult> {
    let target = PathBuf::from(&args.target);
    let reporter = Reporter::new(work_total(&args), notify);
    let progress_reporter = reporter.clone();
    let progress_noti = move |downloaded: usize| progress_reporter.bytes(downloaded);
    match args.mode.clone() {
        InstallFileMode::Direct(source) => {
            reporter.action(source_action(&source), Some(args.output_size));
            let (mut stream, insight_handle) = create_stream_by_source(source).await?;
            let mut target_fs = create_staged_file(&target).await?;
            let bytes_transferred =
                match crate::fs::progressed_copy(stream.as_mut(), &mut target_fs, &progress_noti)
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        drop(target_fs);
                        let _ = tokio::fs::remove_file(&target).await;
                        return Err(fail_with_insight(e, &insight_handle));
                    }
                };
            drop(target_fs);
            let final_insight = snapshot_insight(&insight_handle);
            finalize_keep_insight(&args, &target, &insight_handle, &reporter).await?;
            reporter.finish();
            Ok(InstallResult {
                bytes_transferred,
                insight: final_insight,
            })
        }
        InstallFileMode::Patch { source, diff_size } => {
            reporter.action(FileAction::Patch, Some(diff_size as u64));
            let old = old_path(&args)?;
            let (stream, insight_handle) = create_stream_by_source(source).await?;
            let bytes_transferred =
                match progressed_hpatch(&old, stream, diff_size, &target, Box::new(progress_noti))
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return Err(fail_with_insight(e, &insight_handle)),
                };
            let final_insight = snapshot_insight(&insight_handle);
            finalize_keep_insight(&args, &target, &insight_handle, &reporter).await?;
            reporter.finish();
            Ok(InstallResult {
                bytes_transferred,
                insight: final_insight,
            })
        }
        InstallFileMode::HybridPatch {
            diff,
            source,
            diff_size,
            base_size,
        } => {
            reporter.action(FileAction::Extract, Some(base_size));
            // first extract the packed base next to the output (local, no insight)
            let mut base = target.as_os_str().to_owned();
            base.push(".hybrid-base");
            let base = PathBuf::from(base);
            let _cleanup = TemporaryFile(base.clone());
            let (mut source_stream, _) = create_stream_by_source(source).await?;
            let mut base_fs = create_staged_file(&base).await?;
            let copied =
                progressed_copy(source_stream.as_mut(), &mut base_fs, &progress_noti).await;
            drop(base_fs);
            if let Err(e) = copied {
                let _ = tokio::fs::remove_file(&base).await;
                return Err(e.into());
            }

            reporter.action(FileAction::Patch, Some(diff_size as u64));
            let (diff_stream, insight_handle) = create_stream_by_source(diff).await?;
            let patched = progressed_hpatch(
                &base,
                diff_stream,
                diff_size,
                &target,
                Box::new(progress_noti),
            )
            .await;
            let _ = tokio::fs::remove_file(&base).await;
            let diff_bytes = match patched {
                Ok(v) => v,
                Err(e) => return Err(fail_with_insight(e, &insight_handle)),
            };
            let final_insight = snapshot_insight(&insight_handle);
            finalize_keep_insight(&args, &target, &insight_handle, &reporter).await?;
            reporter.finish();
            Ok(InstallResult {
                bytes_transferred: diff_bytes,
                insight: final_insight,
            })
        }
    }
}

pub async fn install_file_by_reader(
    args: InstallFileArgs,
    mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    notify: ProgressNotify,
) -> Result<usize> {
    let target = PathBuf::from(&args.target);
    let reporter = Reporter::new(work_total(&args), notify);
    let progress_reporter = reporter.clone();
    let progress_noti = move |downloaded: usize| progress_reporter.bytes(downloaded);
    match args.mode.clone() {
        InstallFileMode::Direct(..) => {
            reporter.action(FileAction::Download, Some(args.output_size));
            let mut target_fs = create_staged_file(&target).await?;
            let copied = progressed_copy(reader.as_mut(), &mut target_fs, &progress_noti).await;
            drop(target_fs);
            let res = match copied {
                Ok(n) => n,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&target).await;
                    return Err(e);
                }
            };
            finalize_staged(&args, &target, &reporter).await?;
            reporter.finish();
            Ok(res)
        }
        InstallFileMode::Patch { diff_size, .. } => {
            reporter.action(FileAction::Patch, Some(diff_size as u64));
            let old = old_path(&args)?;
            let res = progressed_hpatch(&old, reader, diff_size, &target, Box::new(progress_noti))
                .await?;
            finalize_staged(&args, &target, &reporter).await?;
            reporter.finish();
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
            InstallFileSource::Sliced { .. } => unreachable!("sliced source in merged request"),
        },
        InstallFileMode::Patch { source, .. } => match source {
            InstallFileSource::Url { size, .. } | InstallFileSource::Local { size, .. } => *size,
            InstallFileSource::Sliced { .. } => unreachable!("sliced source in merged request"),
        },
        InstallFileMode::HybridPatch { diff, .. } => match diff {
            InstallFileSource::Url { size, .. } | InstallFileSource::Local { size, .. } => *size,
            InstallFileSource::Sliced { .. } => unreachable!("sliced source in merged request"),
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
            InstallFileSource::Sliced { .. } => unreachable!("sliced source in merged request"),
        },
        InstallFileMode::Patch { source, .. } => match source {
            InstallFileSource::Url { offset, .. } | InstallFileSource::Local { offset, .. } => {
                *offset
            }
            InstallFileSource::Sliced { .. } => unreachable!("sliced source in merged request"),
        },
        InstallFileMode::HybridPatch { diff, .. } => match diff {
            InstallFileSource::Url { offset, .. } | InstallFileSource::Local { offset, .. } => {
                *offset
            }
            InstallFileSource::Sliced { .. } => unreachable!("sliced source in merged request"),
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
    let mut chunks: Vec<_> = args
        .chunks
        .into_iter()
        .map(|args| ChunkWithPosition {
            position: get_chunk_position(&args),
            args,
        })
        .collect();
    chunks.sort_by_key(|chunk| chunk.position);
    let (start, end) = args
        .range
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("invalid merged range"))?;
    let start: usize = start.parse().map_err(anyhow::Error::from)?;
    let end: usize = end.parse().map_err(anyhow::Error::from)?;
    let (mut reader, _, insight_handle) =
        create_http_stream(&args.url, start, end - start + 1, true, Some(&args.range)).await?;
    let mut results = Vec::new();
    let mut position = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let input: anyhow::Result<Vec<u8>> = async {
            if chunk.position > position {
                let skip = (chunk.position - position) as u64;
                let read =
                    tokio::io::copy(&mut (&mut reader).take(skip), &mut tokio::io::sink()).await?;
                anyhow::ensure!(read == skip, "incomplete merged gap");
            }
            let mut buffer = vec![0; get_chunk_size(&chunk.args)];
            reader.read_exact(&mut buffer).await?;
            Ok(buffer)
        }
        .await;
        let buffer = match input {
            Ok(buffer) => buffer,
            Err(err) => {
                if results.is_empty() {
                    return Err(fail_with_insight(err, &Some(insight_handle)));
                }
                let error =
                    IpcError::from_ta(&fail_with_insight(err, &Some(insight_handle.clone())));
                results.extend((index..chunks.len()).map(|_| Err(error.clone())));
                break;
            }
        };
        position = chunk.position + buffer.len();
        let chunk_notify = {
            let notify = notify.clone();
            progress_notify(move |p| match p {
                Progress::File(mut file) => {
                    file.index = index as u32;
                    notify(Progress::File(file));
                }
                p => notify(p),
            })
        };
        let input: Box<dyn tokio::io::AsyncRead + Unpin + Send> =
            if should_decompress_chunk(&chunk.args) {
                Box::new(TokioZstdDecoder::new(BufReader::new(std::io::Cursor::new(
                    buffer,
                ))))
            } else {
                Box::new(std::io::Cursor::new(buffer))
            };
        let result = install_file_by_reader(chunk.args.clone(), input, chunk_notify).await;
        results.push(result.map_err(|err| {
            IpcError::from_ta(&fail_with_insight(err, &Some(insight_handle.clone())))
        }));
    }
    let insight = insight_handle.lock().unwrap().clone();
    Ok(MultichunkResult { results, insight })
}
