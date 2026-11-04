//! Writing one staged file from a packed, remote or merged source.
//!
//! Network sources are opened as raw byte streams in the async context and
//! bridged into a blocking task; local blocks are read straight from the
//! mapped package. Decompression, patching, hashing and the final
//! `FlushFileBuffers` all happen in that task on the one handle the file was
//! created with, so a staged file is opened and closed exactly once and is
//! never read back to verify it.

use crate::utils::code::{Attach, Cancelled, FILE_IO_FAILED, HASH_MISMATCH};
use crate::verified_write::{
    copy_into, is_cancel, Expected, Failure, HashingWriter, ProgressReader,
};
use crate::{
    dfs::{apply_insight_error, InsightItem},
    fs::{create_http_stream, create_resolved_stream, create_staged_file_blocking, read_failure},
    ipc::{progress_notify, IpcError, Progress, ProgressNotify},
    utils::error::TAResult,
};

use super::file_progress::Reporter;
use crate::session::state::FileAction;
use anyhow::{Context, Result};
use fmmap::tokio::AsyncMmapFileExt;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufWriter, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

fn default_as_false() -> bool {
    false
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

impl InstallFileSource {
    pub(crate) fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    fn skips_decompress(&self) -> bool {
        match self {
            Self::Sliced {
                skip_decompress, ..
            }
            | Self::Url {
                skip_decompress, ..
            }
            | Self::Local {
                skip_decompress, ..
            } => *skip_decompress,
        }
    }

    /// Package range of a merged-request member: a single-part `Sliced` source.
    fn positioned(&self) -> (usize, usize) {
        match self {
            Self::Sliced { parts, .. } if parts.len() == 1 => {
                (parts[0].0 as usize, parts[0].1 as usize)
            }
            _ => unreachable!("merged member is not a single package range"),
        }
    }
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

impl InstallFileMode {
    /// The stream read first: the file itself, the patch, or the hybrid diff.
    pub(crate) fn primary(&self) -> &InstallFileSource {
        match self {
            Self::Direct(source) | Self::Patch { source, .. } => source,
            Self::HybridPatch { diff, .. } => diff,
        }
    }

    /// True when this install reads any bytes over HTTP. A local patch, or a
    /// hybrid whose base and diff are both local, does not take a request slot.
    pub(crate) fn uses_network(&self) -> bool {
        match self {
            Self::Direct(source) | Self::Patch { source, .. } => !source.is_local(),
            Self::HybridPatch { diff, source, .. } => !diff.is_local() || !source.is_local(),
        }
    }

    /// Denominator of the file's progress: output bytes for a direct copy,
    /// diff bytes for a patch, base plus diff bytes for a hybrid.
    pub(crate) fn transfer_total(&self, output_size: u64) -> u64 {
        match self {
            Self::Direct(_) => output_size,
            Self::Patch { diff_size, .. } => *diff_size as u64,
            Self::HybridPatch {
                diff_size,
                base_size,
                ..
            } => *diff_size as u64 + *base_size,
        }
    }
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

impl InstallFileArgs {
    fn expected(&self) -> Expected {
        Expected {
            size: self.output_size,
            md5: self.md5.clone(),
            xxh: self.xxh.clone(),
        }
    }

    fn clears_layout(&self) -> bool {
        self.clear_installer_index_mark.unwrap_or(false)
    }
}

/// Deletes the file when dropped.
pub(crate) struct TemporaryFile(pub(crate) PathBuf);
impl TemporaryFile {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

type InsightHandle = Option<Arc<Mutex<InsightItem>>>;
type SyncInput = Box<dyn Read + Send>;

fn snapshot_insight(handle: &InsightHandle) -> Option<InsightItem> {
    handle
        .as_ref()
        .and_then(|h| h.lock().ok().map(|insight| insight.clone()))
}

fn fail_with_insight(
    err: anyhow::Error,
    handle: &InsightHandle,
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

fn write_failure(failure: Failure, target: &Path) -> anyhow::Error {
    match failure {
        Failure::Cancelled => Cancelled.into(),
        Failure::Read(e) => read_failure(e),
        Failure::Write(e) => anyhow::Error::new(e).context("WRITE_TARGET_ERR"),
        Failure::Size { expected, actual } => anyhow::anyhow!(
            "File {} size mismatch: expected {expected} bytes, got {actual}",
            target.display()
        )
        .attach(HASH_MISMATCH),
        Failure::Hash { expected, actual } => anyhow::anyhow!(
            "File {} hash mismatch: expected {expected}, got {actual}",
            target.display()
        )
        .attach(HASH_MISMATCH),
    }
}

/// A source opened for reading: packed blocks are slices of the mapped
/// package, everything else is a raw (still compressed) stream.
enum RawSource {
    Bytes(&'static [u8]),
    Stream(Box<dyn tokio::io::AsyncRead + Unpin + Send>),
}

async fn open_source(source: &InstallFileSource) -> TAResult<(RawSource, InsightHandle)> {
    match source {
        InstallFileSource::Sliced { parts, .. } => {
            if let [(offset, len)] = parts.as_slice() {
                let (reader, _, insight) =
                    create_resolved_stream(*offset as usize, *len as usize, None).await?;
                return Ok((RawSource::Stream(reader), Some(insight)));
            }
            Ok((
                RawSource::Stream(super::download::sliced(parts.clone())),
                None,
            ))
        }
        InstallFileSource::Url {
            url,
            offset,
            size,
            request_range,
            ..
        } => {
            let (stream, _content_length, insight) =
                create_http_stream(url, *offset, *size, request_range.as_deref()).await?;
            Ok((RawSource::Stream(stream), Some(insight)))
        }
        InstallFileSource::Local { offset, size, .. } => {
            let mmap = crate::local::mmap().await;
            let bytes = mmap
                .as_slice()
                .get(*offset..offset + size)
                .context("MMAP_ERR")?;
            Ok((RawSource::Bytes(bytes), None))
        }
    }
}

/// zstd runs in the reading thread; each block is one zstd frame.
fn decoded<R: std::io::BufRead + Send + 'static>(reader: R, decompress: bool) -> Result<SyncInput> {
    if !decompress {
        return Ok(Box::new(reader));
    }
    let decoder = zstd::stream::read::Decoder::with_buffer(reader).map_err(read_failure)?;
    Ok(Box::new(decoder.single_frame()))
}

/// Blocking reader over a source. Streams are bridged here, in the async
/// context the bridge needs.
fn sync_input(raw: RawSource, decompress: bool) -> Result<SyncInput> {
    match raw {
        RawSource::Bytes(bytes) => decoded(std::io::Cursor::new(bytes), decompress),
        RawSource::Stream(stream) => decoded(
            std::io::BufReader::new(tokio_util::io::SyncIoBridge::new(stream)),
            decompress,
        ),
    }
}

fn owned_input(bytes: Vec<u8>, decompress: bool) -> Result<SyncInput> {
    decoded(std::io::Cursor::new(bytes), decompress)
}

async fn open_input(source: &InstallFileSource) -> TAResult<(SyncInput, InsightHandle)> {
    let (raw, insight) = open_source(source).await?;
    let input =
        sync_input(raw, !source.skips_decompress()).map_err(|e| fail_with_insight(e, &insight))?;
    Ok((input, insight))
}

/// Starts `work` on the blocking pool with the download job's cancellation
/// (when the caller runs inside one; the job is a task-local, so it is read
/// here, not in the blocking thread). Any failure deletes `target`.
fn spawn_job<T: Send + 'static>(
    target: PathBuf,
    work: impl FnOnce(&dyn Fn() -> bool) -> Result<T> + Send + 'static,
) -> tokio::task::JoinHandle<Result<T>> {
    let stop = super::download::CURRENT
        .try_with(|ctx| ctx.stop.clone())
        .ok();
    tokio::task::spawn_blocking(move || {
        let cancelled = || stop.as_ref().is_some_and(|s| s.is_cancelled());
        let res = work(&cancelled);
        if res.is_err() {
            let _ = std::fs::remove_file(&target);
        }
        res
    })
}

async fn run_blocking<T: Send + 'static>(
    target: &Path,
    work: impl FnOnce(&dyn Fn() -> bool) -> Result<T> + Send + 'static,
) -> Result<T> {
    spawn_job(target.to_path_buf(), work)
        .await
        .context("INSTALL_THREAD_ERR")
        .and_then(|r| r)
}

type StagedWriter<'a> = HashingWriter<'a, BufWriter<std::fs::File>>;

fn staged_writer<'a>(
    args: &InstallFileArgs,
    target: &Path,
    cancelled: &'a dyn Fn() -> bool,
) -> Result<StagedWriter<'a>> {
    let file = create_staged_file_blocking(target)?;
    Ok(HashingWriter::new(
        BufWriter::with_capacity(256 * 1024, file),
        args.expected(),
        args.clears_layout(),
        cancelled,
    ))
}

/// Check the written content, flush it to disk on the same handle and close
/// it: rename is atomic only for metadata, so an unflushed file swapped in
/// right before power loss would come back truncated.
fn commit_output(writer: StagedWriter<'_>, target: &Path, reporter: &Reporter) -> Result<u64> {
    reporter.action(FileAction::Verify, None);
    let (buffered, written) = writer.finish().map_err(|f| write_failure(f, target))?;
    reporter.action(FileAction::Flush, None);
    let file = buffered
        .into_inner()
        .map_err(|e| anyhow::Error::new(e.into_error()).context("WRITE_TARGET_ERR"))?;
    file.sync_all()
        .context("SYNC_TARGET_ERR")
        .attach(FILE_IO_FAILED)?;
    Ok(written)
}

fn write_direct(
    input: &mut dyn Read,
    args: &InstallFileArgs,
    target: &Path,
    reporter: &Reporter,
    cancelled: &dyn Fn() -> bool,
) -> Result<usize> {
    let mut writer = staged_writer(args, target, cancelled)?
        .with_progress(|written| reporter.bytes(written as usize));
    copy_into(input, &mut writer).map_err(|f| write_failure(f, target))?;
    Ok(commit_output(writer, target, reporter)? as usize)
}

/// Apply `diff` to `old`, writing the result to `target`. Progress follows
/// the diff bytes read, matching the `transfer_total` of a patch.
#[allow(clippy::too_many_arguments)] // one patch job; the values come from different places
fn write_patched(
    old: &mut (impl Read + Seek),
    old_size: u64,
    diff: impl Read,
    diff_size: usize,
    args: &InstallFileArgs,
    target: &Path,
    reporter: &Reporter,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let mut writer = staged_writer(args, target, cancelled)?;
    let diff = ProgressReader::new(diff, diff_size as u64, |read| reporter.bytes(read as usize));
    hpatch_sys::safe_patch_single_stream(&mut writer, diff, diff_size, old, old_size as usize)
        .map_err(|e| {
            if is_cancel(&e) {
                Cancelled.into()
            } else {
                // the readers' own errors come back as they were raised, so a
                // network cancel or reset is still recognised
                anyhow::Error::new(e).context("PATCH_FAILED_ERR")
            }
        })?;
    commit_output(writer, target, reporter)?;
    Ok(())
}

fn open_old(args: &InstallFileArgs) -> Result<(std::fs::File, u64)> {
    let path = args
        .old
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("patch without an old file"))?;
    let file = std::fs::File::open(path).context("OPEN_TARGET_ERR")?;
    let size = file.metadata().context("GET_TARGET_SIZE_ERR")?.len();
    Ok((file, size))
}

fn work_total(args: &InstallFileArgs) -> u64 {
    args.mode.transfer_total(args.output_size)
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
    let (bytes_transferred, insight) = match args.mode.clone() {
        InstallFileMode::Direct(source) => {
            reporter.action(source_action(&source), Some(args.output_size));
            let (mut input, insight) = open_input(&source).await?;
            let (job_args, job_target, job_reporter) =
                (args.clone(), target.clone(), reporter.clone());
            let written = run_blocking(&target, move |cancelled| {
                write_direct(
                    &mut *input,
                    &job_args,
                    &job_target,
                    &job_reporter,
                    cancelled,
                )
            })
            .await
            .map_err(|e| fail_with_insight(e, &insight))?;
            (written, insight)
        }
        InstallFileMode::Patch { source, diff_size } => {
            reporter.action(FileAction::Patch, Some(diff_size as u64));
            let (diff, insight) = open_input(&source).await?;
            let (job_args, job_target, job_reporter) =
                (args.clone(), target.clone(), reporter.clone());
            run_blocking(&target, move |cancelled| {
                let (mut old, old_size) = open_old(&job_args)?;
                write_patched(
                    &mut old,
                    old_size,
                    diff,
                    diff_size,
                    &job_args,
                    &job_target,
                    &job_reporter,
                    cancelled,
                )
            })
            .await
            .map_err(|e| fail_with_insight(e, &insight))?;
            (diff_size, insight)
        }
        InstallFileMode::HybridPatch {
            diff,
            source,
            diff_size,
            base_size,
        } => {
            reporter.action(FileAction::Extract, Some(base_size));
            // the packed base is extracted next to the output; the patch then
            // reads it back through the same handle as its old file
            let mut base = target.as_os_str().to_owned();
            base.push(".hybrid-base");
            let base = TemporaryFile(PathBuf::from(base));
            let (mut base_input, _) = open_input(&source).await?;
            let (base_path, job_reporter) = (base.path().to_path_buf(), reporter.clone());
            let (base_file, base_len) = run_blocking(&target, move |cancelled| {
                let mut base_file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&base_path)
                    .context("CREATE_TARGET_FILE_ERR")?;
                let mut writer = HashingWriter::new(
                    BufWriter::with_capacity(256 * 1024, &mut base_file),
                    Expected::default(),
                    false,
                    cancelled,
                )
                .with_progress(|written| job_reporter.bytes(written as usize));
                copy_into(&mut *base_input, &mut writer)
                    .map_err(|f| write_failure(f, &base_path))?;
                let (buffered, base_len) =
                    writer.finish().map_err(|f| write_failure(f, &base_path))?;
                drop(buffered);
                base_file
                    .rewind()
                    .context("OPEN_TARGET_ERR")
                    .attach(FILE_IO_FAILED)?;
                Ok((base_file, base_len))
            })
            .await?;

            // the diff request starts only once the base is ready
            reporter.action(FileAction::Patch, Some(diff_size as u64));
            let (diff, insight) = open_input(&diff).await?;
            let (job_args, job_target, job_reporter) =
                (args.clone(), target.clone(), reporter.clone());
            run_blocking(&target, move |cancelled| {
                let mut base_file = base_file;
                write_patched(
                    &mut base_file,
                    base_len,
                    diff,
                    diff_size,
                    &job_args,
                    &job_target,
                    &job_reporter,
                    cancelled,
                )
            })
            .await
            .map_err(|e| fail_with_insight(e, &insight))?;
            drop(base);
            (diff_size, insight)
        }
    };
    let insight = snapshot_insight(&insight);
    reporter.finish();
    Ok(InstallResult {
        bytes_transferred,
        insight,
    })
}

/// One member of a merged request, written from its slice of the shared
/// response.
fn install_chunk(
    args: &InstallFileArgs,
    mut input: SyncInput,
    reporter: &Reporter,
    cancelled: &dyn Fn() -> bool,
) -> Result<usize> {
    let target = PathBuf::from(&args.target);
    match &args.mode {
        InstallFileMode::Direct(..) => {
            reporter.action(FileAction::Download, Some(args.output_size));
            let written = write_direct(&mut *input, args, &target, reporter, cancelled)?;
            reporter.finish();
            Ok(written)
        }
        InstallFileMode::Patch { diff_size, .. } => {
            reporter.action(FileAction::Patch, Some(*diff_size as u64));
            let (mut old, old_size) = open_old(args)?;
            write_patched(
                &mut old, old_size, input, *diff_size, args, &target, reporter, cancelled,
            )?;
            reporter.finish();
            Ok(*diff_size)
        }
        InstallFileMode::HybridPatch { .. } => {
            anyhow::bail!("Hybrid patch is not supported in a merged request")
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct InstallMultiStreamArgs {
    pub range: String,
    pub chunks: Vec<InstallFileArgs>,
}
fn chunk_span(args: &InstallFileArgs) -> (usize, usize) {
    args.mode.primary().positioned()
}

#[derive(Debug, Clone)]
struct ChunkWithPosition {
    position: usize,
    args: InstallFileArgs,
}

/// Members written while the shared response keeps being read.
const CHUNKS_IN_FLIGHT: usize = 4;

pub async fn ipc_install_multichunk_stream(
    args: InstallMultiStreamArgs,
    notify: ProgressNotify,
) -> TAResult<MultichunkResult> {
    let (start, end) = args
        .range
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("invalid merged range"))?;
    let start: usize = start.parse().map_err(anyhow::Error::from)?;
    let end: usize = end.parse().map_err(anyhow::Error::from)?;
    let mut chunks = args
        .chunks
        .into_iter()
        .map(|args| {
            let position = chunk_span(&args)
                .0
                .checked_sub(start)
                .ok_or_else(|| anyhow::anyhow!("merged member before range start"))?;
            Ok(ChunkWithPosition { position, args })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    chunks.sort_by_key(|chunk| chunk.position);
    let (mut reader, _, insight_handle) =
        create_resolved_stream(start, end - start + 1, Some(&args.range)).await?;
    let insight = Some(insight_handle.clone());
    let chunk_error = |err: anyhow::Error| IpcError::from_ta(&fail_with_insight(err, &insight));

    #[allow(clippy::result_large_err)] // IpcError carries InsightItem across the pipe
    let mut results: Vec<Result<usize, IpcError>> = Vec::new();
    // members are written in order of their position, so awaiting the oldest
    // job first keeps `results` in member order
    let mut writing: VecDeque<tokio::task::JoinHandle<Result<usize>>> = VecDeque::new();
    #[allow(clippy::result_large_err)] // IpcError carries InsightItem across the pipe
    let joined = |job: Result<Result<usize>, tokio::task::JoinError>| {
        job.context("INSTALL_THREAD_ERR")
            .and_then(|r| r)
            .map_err(chunk_error)
    };
    let mut position = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let input: anyhow::Result<Vec<u8>> = async {
            if chunk.position > position {
                let skip = (chunk.position - position) as u64;
                let read =
                    tokio::io::copy(&mut (&mut reader).take(skip), &mut tokio::io::sink()).await?;
                anyhow::ensure!(read == skip, "incomplete merged gap");
            }
            let mut buffer = vec![0; chunk_span(&chunk.args).1];
            reader.read_exact(&mut buffer).await?;
            Ok(buffer)
        }
        .await;
        let buffer = match input {
            Ok(buffer) => buffer,
            Err(err) => {
                if index == 0 {
                    return Err(fail_with_insight(err, &insight));
                }
                for job in writing.drain(..) {
                    results.push(joined(job.await));
                }
                let error = chunk_error(err);
                #[allow(clippy::result_large_err)] // IpcError carries InsightItem across the pipe
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
        let decompress = !chunk.args.mode.primary().skips_decompress();
        let job_args = chunk.args.clone();
        if writing.len() >= CHUNKS_IN_FLIGHT {
            if let Some(job) = writing.pop_front() {
                results.push(joined(job.await));
            }
        }
        writing.push_back(spawn_job(
            PathBuf::from(&job_args.target),
            move |cancelled| {
                let reporter = Reporter::new(work_total(&job_args), chunk_notify);
                let input = owned_input(buffer, decompress)?;
                install_chunk(&job_args, input, &reporter, cancelled)
            },
        ));
    }
    for job in writing.drain(..) {
        results.push(joined(job.await));
    }
    let insight = insight_handle.lock().unwrap().clone();
    Ok(MultichunkResult { results, insight })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> InstallFileSource {
        InstallFileSource::Local {
            offset: 0,
            size: 1,
            skip_decompress: false,
        }
    }

    fn url() -> InstallFileSource {
        InstallFileSource::Url {
            url: "http://example".into(),
            offset: 0,
            size: 1,
            skip_decompress: false,
            request_range: None,
        }
    }

    #[test]
    fn local_reads_do_not_take_a_request_slot() {
        assert!(!InstallFileMode::Direct(local()).uses_network());
        assert!(!InstallFileMode::Patch {
            source: local(),
            diff_size: 1
        }
        .uses_network());
        assert!(!InstallFileMode::HybridPatch {
            diff: local(),
            source: local(),
            diff_size: 1,
            base_size: 1,
        }
        .uses_network());
        assert!(InstallFileMode::Direct(url()).uses_network());
        assert!(InstallFileMode::Patch {
            source: url(),
            diff_size: 1
        }
        .uses_network());
        assert!(InstallFileMode::HybridPatch {
            diff: local(),
            source: url(),
            diff_size: 1,
            base_size: 1,
        }
        .uses_network());
        assert!(InstallFileMode::HybridPatch {
            diff: url(),
            source: local(),
            diff_size: 1,
            base_size: 1,
        }
        .uses_network());
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachina-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn xxh(bytes: &[u8]) -> String {
        let mut h = twox_hash::XxHash3_128::new();
        h.write(bytes);
        format!("{:x}", h.finish_128())
    }

    fn args_for(target: &Path, new: &[u8]) -> InstallFileArgs {
        InstallFileArgs {
            mode: InstallFileMode::Direct(local()),
            target: target.to_string_lossy().into_owned(),
            output_size: new.len() as u64,
            old: None,
            md5: None,
            xxh: Some(xxh(new)),
            clear_installer_index_mark: None,
        }
    }

    /// 1 MiB 伪随机旧文件，新文件把前后两半对调并插入一段新内容：打补丁时要跳着读旧文件。
    fn patch_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let old: Vec<u8> = (0..1 << 20)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        let mid = old.len() / 2;
        let mut new = old[mid..].to_vec();
        new.extend_from_slice(b"inserted between the halves");
        new.extend_from_slice(&old[..mid]);
        let mut diff = std::io::Cursor::new(Vec::new());
        hdiff_sys::safe_create_single_patch(&new, &old, &mut diff, 7).unwrap();
        (old, new, diff.into_inner())
    }

    fn reporter(log: Arc<Mutex<Vec<Progress>>>) -> Reporter {
        Reporter::new(0, progress_notify(move |p| log.lock().unwrap().push(p)))
    }

    #[test]
    fn patch_output_is_verified_and_progress_reaches_the_diff_size() {
        let (old, new, diff) = patch_fixture();
        let dir = temp_dir();
        let target = dir.join("new.bin");
        let args = args_for(&target, &new);
        let log = Arc::new(Mutex::new(Vec::new()));
        let never = || false;
        write_patched(
            &mut std::io::Cursor::new(&old),
            old.len() as u64,
            std::io::Cursor::new(&diff),
            diff.len(),
            &args,
            &target,
            &reporter(log.clone()),
            &never,
        )
        .unwrap();
        assert!(std::fs::read(&target).unwrap() == new);
        let last_done = log.lock().unwrap().iter().rev().find_map(|p| match p {
            Progress::File(f) => f.done,
            _ => None,
        });
        assert_eq!(last_done, Some(diff.len() as u64));

        // 期望 hash 不符时报 HASH_MISMATCH
        let wrong = InstallFileArgs {
            xxh: Some("0".into()),
            ..args
        };
        let err = write_patched(
            &mut std::io::Cursor::new(&old),
            old.len() as u64,
            std::io::Cursor::new(&diff),
            diff.len(),
            &wrong,
            &target,
            &reporter(Arc::new(Mutex::new(Vec::new()))),
            &never,
        )
        .unwrap_err();
        assert!(matches!(
            crate::utils::code::extract(&err),
            crate::utils::code::Extracted::Coded(c) if c.code == HASH_MISMATCH
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_returns_the_diff_reader_error() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "diff stream reset",
                ))
            }
        }
        let (old, new, diff) = patch_fixture();
        let dir = temp_dir();
        let target = dir.join("new.bin");
        let never = || false;
        let reader = Read::chain(std::io::Cursor::new(&diff[..diff.len() / 2]), Broken);
        let err = write_patched(
            &mut std::io::Cursor::new(&old),
            old.len() as u64,
            reader,
            diff.len(),
            &args_for(&target, &new),
            &target,
            &reporter(Arc::new(Mutex::new(Vec::new()))),
            &never,
        )
        .unwrap_err();
        let io = err
            .chain()
            .find_map(|e| e.downcast_ref::<std::io::Error>())
            .expect("io error in chain");
        assert_eq!(io.kind(), std::io::ErrorKind::ConnectionReset);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_patch_reports_cancel() {
        let (old, new, diff) = patch_fixture();
        let dir = temp_dir();
        let target = dir.join("new.bin");
        let cancelled = || true;
        let err = write_patched(
            &mut std::io::Cursor::new(&old),
            old.len() as u64,
            std::io::Cursor::new(&diff),
            diff.len(),
            &args_for(&target, &new),
            &target,
            &reporter(Arc::new(Mutex::new(Vec::new()))),
            &cancelled,
        )
        .unwrap_err();
        assert!(matches!(
            crate::utils::code::extract(&err),
            crate::utils::code::Extracted::Cancelled
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compressed_direct_write_clears_the_layout_header() {
        let mut image: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
        image[0x4E..0x4E + 18].copy_from_slice(crate::pack_format::LAYOUT_MARKER);
        image[0x4E + 18..0x4E + 38].fill(0x5A);
        let mut cleared = image.clone();
        crate::pack_format::clear_layout(&mut cleared);
        let packed = zstd::encode_all(&image[..], 3).unwrap();

        let dir = temp_dir();
        let target = dir.join("sub").join("updater.exe");
        let args = InstallFileArgs {
            clear_installer_index_mark: Some(true),
            ..args_for(&target, &cleared)
        };
        let never = || false;
        let mut input = owned_input(packed, true).unwrap();
        let written = write_direct(
            &mut *input,
            &args,
            &target,
            &reporter(Arc::new(Mutex::new(Vec::new()))),
            &never,
        )
        .unwrap();
        assert_eq!(written, cleared.len());
        assert_eq!(std::fs::read(&target).unwrap(), cleared);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
