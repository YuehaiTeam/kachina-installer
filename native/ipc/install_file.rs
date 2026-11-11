//! Writing one staged file from a packed, remote or merged source.
//!
//! Network sources are opened as raw byte streams in the async context and
//! bridged into a blocking task; local blocks are read straight from the
//! mapped package. Decompression, patching, hashing, the read back that checks
//! what the file system holds, and the final `FlushFileBuffers` all happen in
//! that task on the one handle the file was created with, so a staged file is
//! opened and closed exactly once.

use crate::utils::code::{extract, Attach, Cancelled, Extracted, FILE_IO_FAILED, HASH_MISMATCH};
use crate::verified_write::{
    copy_into, is_cancel, Expected, Failure, HashingWriter, ProgressReader,
};
use crate::{
    dfs::{apply_insight_error, InsightItem},
    fs::{
        create_http_stream, create_resolved_stream, create_staged_file_blocking,
        ClassifiedNetworkError,
    },
    ipc::{progress_notify, IpcError, Progress, ProgressNotify},
    utils::{error::TAResult, net_failure::NetFailure},
};

use super::file_progress::Reporter;
use anyhow::{Context, Result};
use fmmap::tokio::AsyncMmapFileExt;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufWriter, Read, Seek};
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
        if !altered_after_write(&err) {
            if let Ok(mut insight) = handle.lock() {
                apply_insight_error(&mut insight, &err);
            }
        }
        crate::utils::error::TACommandError::with_insight_handle(err, handle.clone())
    } else {
        crate::utils::error::TACommandError::new(err)
    }
}

fn write_failure(failure: Failure, target: &Path) -> anyhow::Error {
    match failure {
        Failure::Cancelled => Cancelled.into(),
        Failure::Read(e) => source_failure(e),
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
        Failure::ReadBack(e) => anyhow::Error::new(e).context("VERIFY_TARGET_ERR"),
        Failure::Altered { written, read } => anyhow::Error::new(AlteredAfterWrite {
            target: target.display().to_string(),
            written,
            read,
        })
        .attach(HASH_MISMATCH),
    }
}

/// The staged file read back differently from what was written. The source
/// delivered the right bytes (they passed the size and hash checks on the way
/// in), so the fault is on this machine: the file is retried like any
/// `HASH_MISMATCH`, and no download request's insight records it.
#[derive(Debug)]
struct AlteredAfterWrite {
    target: String,
    written: String,
    read: String,
}

impl std::fmt::Display for AlteredAfterWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "File {} read back as {}, written as {}",
            self.target, self.read, self.written
        )
    }
}

impl std::error::Error for AlteredAfterWrite {}

fn altered_after_write(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| e.downcast_ref::<AlteredAfterWrite>().is_some())
}

/// A failed read of the input. Data the decoder rejected is `HASH_MISMATCH`:
/// the source delivered all its bytes, so the bytes are wrong, as when the
/// hash does not match. Anything else (a network failure, a local read error,
/// a cancel) passes through, and the caller infers its code.
fn source_failure(e: std::io::Error) -> anyhow::Error {
    let Some(payload) = e.get_ref() else {
        return anyhow::Error::new(e);
    };
    if !payload.is::<Undecodable>() {
        return anyhow::Error::new(e);
    }
    let undecodable = e
        .into_inner()
        .and_then(|p| p.downcast::<Undecodable>().ok())
        .expect("payload checked above");
    anyhow::Error::new(*undecodable).attach(HASH_MISMATCH)
}

/// Compressed data the zstd decoder rejected, as opposed to a failed read of
/// the source under it.
#[derive(Debug)]
struct Undecodable {
    /// Source bytes handed to the decoder when it gave up; the rejected data
    /// lies before this point. `None` when the frame runs past the end of the
    /// source, which says nothing about where the data went wrong.
    before: Option<u64>,
    cause: std::io::Error,
}

impl std::fmt::Display for Undecodable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DECOMPRESS_ERR")
    }
}

impl std::error::Error for Undecodable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Read buffer in front of the decoder.
const READ_BUFFER: usize = 8 * 1024;

/// How far before [`Undecodable::before`] the rejected data can lie. zstd
/// decodes a block (at most 128 KiB, plus a 3-byte header) only once it holds
/// all of it, and a damaged block header almost always shows up by the next
/// block; the read buffer adds its size.
pub(super) const UNDECODABLE_REACH: u64 = 2 * (128 * 1024 + 3) + READ_BUFFER as u64;

/// The decoder's source: counts the bytes handed out and notes whether the
/// last read failed in the source itself.
struct Traced<R> {
    inner: R,
    taken: u64,
    seen: u64,
    failed: bool,
}

impl<R: BufRead> Read for Traced<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf).inspect_err(|_| self.failed = true)?;
        self.taken += n as u64;
        self.seen = self.seen.max(self.taken);
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Traced<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        match self.inner.fill_buf() {
            Ok(buf) => {
                self.seen = self.seen.max(self.taken + buf.len() as u64);
                Ok(buf)
            }
            Err(e) => {
                self.failed = true;
                Err(e)
            }
        }
    }

    fn consume(&mut self, n: usize) {
        self.taken += n as u64;
        self.inner.consume(n);
    }
}

/// zstd over a source. The source's own read errors pass through unchanged;
/// errors the decoder raises itself become [`Undecodable`], or a short body
/// when a network response declared no length (`unbounded`, its sanitized
/// URL): there the frame running past the end means the body was cut off.
struct Decoded<R: BufRead> {
    decoder: zstd::stream::read::Decoder<'static, Traced<R>>,
    unbounded: Option<String>,
}

impl<R: BufRead> Read for Decoded<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.decoder.get_mut().failed = false;
        self.decoder.read(buf).map_err(|cause| {
            let source = self.decoder.get_ref();
            if source.failed {
                return cause;
            }
            let ran_out = cause.kind() == std::io::ErrorKind::UnexpectedEof;
            match &self.unbounded {
                Some(url) if ran_out => std::io::Error::other(ClassifiedNetworkError {
                    failure: NetFailure::short_body(),
                    original_error: Box::new(cause),
                    url: url.clone(),
                }),
                _ => std::io::Error::other(Undecodable {
                    before: (!ran_out).then_some(source.seen),
                    cause,
                }),
            }
        })
    }
}

/// A source opened for reading: packed blocks are slices of the mapped
/// package, everything else is a raw (still compressed) stream.
enum RawSource {
    Bytes(&'static [u8]),
    /// `unbounded`: see [`Decoded`].
    Stream {
        stream: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        unbounded: Option<String>,
    },
}

impl RawSource {
    fn stream(stream: Box<dyn tokio::io::AsyncRead + Unpin + Send>) -> Self {
        Self::Stream {
            stream,
            unbounded: None,
        }
    }
}

/// Insights a failure of one input is recorded on: the request's, or, for a
/// sliced source, its parts'.
struct SourceInsight {
    request: InsightHandle,
    parts: Option<SlicedParts>,
}

/// The parts of a sliced source, in stream order, and the insight of the
/// request that delivered each.
struct SlicedParts {
    lens: Vec<u64>,
    insights: super::download::PartInsights,
}

impl SourceInsight {
    fn none() -> Self {
        Self {
            request: None,
            parts: None,
        }
    }

    fn fail(&self, err: anyhow::Error) -> crate::utils::error::TACommandError {
        if let Some(parts) = &self.parts {
            parts.blame(&err);
        }
        fail_with_insight(err, &self.request)
    }
}

impl SlicedParts {
    /// Records wrong data (`HASH_MISMATCH`) on the parts that can have
    /// delivered it: those within [`UNDECODABLE_REACH`] before where the
    /// decoder gave up, or every part when nothing narrows it down (a hash
    /// that does not match, a frame that runs past the end).
    fn blame(&self, err: &anyhow::Error) {
        if !matches!(extract(err), Extracted::Coded(c) if c.code == HASH_MISMATCH)
            || altered_after_write(err)
        {
            return;
        }
        let before = err
            .chain()
            .find_map(|e| e.downcast_ref::<Undecodable>())
            .and_then(|u| u.before);
        let insights = self.insights.lock().unwrap_or_else(|e| e.into_inner());
        let mut start = 0;
        for (len, insight) in self.lens.iter().zip(insights.iter()) {
            let end = start + len;
            let suspect =
                before.is_none_or(|b| start < b && b.saturating_sub(UNDECODABLE_REACH) < end);
            start = end;
            if let (true, Some(insight)) = (suspect, insight) {
                if let Ok(mut item) = insight.lock() {
                    apply_insight_error(&mut item, err);
                }
            }
        }
    }
}

async fn open_source(source: &InstallFileSource) -> TAResult<(RawSource, SourceInsight)> {
    match source {
        InstallFileSource::Sliced { parts, .. } => {
            if let [(offset, len)] = parts.as_slice() {
                let (reader, _, insight) =
                    create_resolved_stream(*offset as usize, *len as usize, None).await?;
                return Ok((
                    RawSource::stream(reader),
                    SourceInsight {
                        request: Some(insight),
                        parts: None,
                    },
                ));
            }
            let (reader, insights) = super::download::sliced(parts.clone());
            Ok((
                RawSource::stream(reader),
                SourceInsight {
                    request: None,
                    parts: Some(SlicedParts {
                        lens: parts.iter().map(|&(_, len)| len).collect(),
                        insights,
                    }),
                },
            ))
        }
        InstallFileSource::Url {
            url,
            offset,
            size,
            request_range,
            ..
        } => {
            let (stream, length, insight) =
                create_http_stream(url, *offset, *size, request_range.as_deref()).await?;
            let unbounded = (*size == 0 && length == 0)
                .then(|| crate::utils::url::sanitize_url_for_logging(url));
            Ok((
                RawSource::Stream { stream, unbounded },
                SourceInsight {
                    request: Some(insight),
                    parts: None,
                },
            ))
        }
        InstallFileSource::Local { offset, size, .. } => {
            let mmap = crate::local::mmap().await;
            let bytes = mmap
                .as_slice()
                .get(*offset..offset + size)
                .context("MMAP_ERR")?;
            Ok((RawSource::Bytes(bytes), SourceInsight::none()))
        }
    }
}

/// zstd runs in the reading thread; each block is one zstd frame.
fn decoded<R: BufRead + Send + 'static>(
    reader: R,
    decompress: bool,
    unbounded: Option<String>,
) -> Result<SyncInput> {
    if !decompress {
        return Ok(Box::new(reader));
    }
    let source = Traced {
        inner: reader,
        taken: 0,
        seen: 0,
        failed: false,
    };
    let decoder = zstd::stream::read::Decoder::with_buffer(source)?.single_frame();
    Ok(Box::new(Decoded { decoder, unbounded }))
}

/// Blocking reader over a source. Streams are bridged here, in the async
/// context the bridge needs.
fn sync_input(raw: RawSource, decompress: bool) -> Result<SyncInput> {
    match raw {
        RawSource::Bytes(bytes) => decoded(std::io::Cursor::new(bytes), decompress, None),
        RawSource::Stream { stream, unbounded } => decoded(
            std::io::BufReader::with_capacity(
                READ_BUFFER,
                tokio_util::io::SyncIoBridge::new(stream),
            ),
            decompress,
            unbounded,
        ),
    }
}

fn owned_input(bytes: Vec<u8>, decompress: bool) -> Result<SyncInput> {
    decoded(std::io::Cursor::new(bytes), decompress, None)
}

async fn open_input(source: &InstallFileSource) -> TAResult<(SyncInput, SourceInsight)> {
    let (raw, insight) = open_source(source).await?;
    let input = sync_input(raw, !source.skips_decompress()).map_err(|e| insight.fail(e))?;
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

/// Check the written content, read it back, flush it to disk and close it, all
/// on the handle it was written through. The read back checks what the file
/// system holds after the write buffer, the page cache and any filter driver.
/// The flush is there because rename is atomic only for metadata: an
/// unflushed file swapped in right before power loss would come back
/// truncated.
fn commit_output(writer: StagedWriter<'_>, target: &Path) -> Result<u64> {
    let (file, written) = writer
        .finish_verified()
        .map_err(|f| write_failure(f, target))?;
    file.sync_all()
        .context("SYNC_TARGET_ERR")
        .attach(FILE_IO_FAILED)?;
    Ok(written.bytes)
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
    Ok(commit_output(writer, target)? as usize)
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
                // network cancel or reset, or an undecodable diff, is still
                // recognised
                source_failure(e).context("PATCH_FAILED_ERR")
            }
        })?;
    commit_output(writer, target)?;
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

pub async fn ipc_install_file(
    args: InstallFileArgs,
    notify: ProgressNotify,
) -> TAResult<InstallResult> {
    let target = PathBuf::from(&args.target);
    let reporter = Reporter::new(work_total(&args), notify);
    let (bytes_transferred, insight) = match args.mode.clone() {
        InstallFileMode::Direct(source) => {
            reporter.segment();
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
            .map_err(|e| insight.fail(e))?;
            (written, insight)
        }
        InstallFileMode::Patch { source, diff_size } => {
            reporter.segment();
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
            .map_err(|e| insight.fail(e))?;
            (diff_size, insight)
        }
        InstallFileMode::HybridPatch {
            diff,
            source,
            diff_size,
            ..
        } => {
            reporter.segment();
            // the packed base is extracted next to the output; the patch then
            // reads it back through the same handle as its old file
            let mut base = target.as_os_str().to_owned();
            base.push(".hybrid-base");
            let base = TemporaryFile(PathBuf::from(base));
            let (mut base_input, base_insight) = open_input(&source).await?;
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
                let (buffered, base) = writer.finish().map_err(|f| write_failure(f, &base_path))?;
                let base_len = base.bytes;
                drop(buffered);
                base_file
                    .rewind()
                    .context("OPEN_TARGET_ERR")
                    .attach(FILE_IO_FAILED)?;
                Ok((base_file, base_len))
            })
            .await
            .map_err(|e| base_insight.fail(e))?;

            // the diff request starts only once the base is ready
            reporter.segment();
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
            .map_err(|e| insight.fail(e))?;
            drop(base);
            (diff_size, insight)
        }
    };
    let insight = snapshot_insight(&insight.request);
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
            reporter.segment();
            let written = write_direct(&mut *input, args, &target, reporter, cancelled)?;
            reporter.finish();
            Ok(written)
        }
        InstallFileMode::Patch { diff_size, .. } => {
            reporter.segment();
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
            // the stream fails a body shorter than the range, so these reads
            // end with all the bytes or with a network error
            if chunk.position > position {
                let skip = (chunk.position - position) as u64;
                tokio::io::copy(&mut (&mut reader).take(skip), &mut tokio::io::sink()).await?;
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
            Progress::File(f) => Some(f.completed),
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

    /// A staged file changed between the write and the read back fails the
    /// write as `HASH_MISMATCH`, though the bytes handed in match the
    /// expected hash.
    #[test]
    fn file_changed_before_the_read_back_is_a_hash_mismatch() {
        let data = crate::verified_write::testing::bytes(600 * 1024);
        let dir = temp_dir();
        let target = dir.join("out.bin");
        let mut input = crate::verified_write::testing::TamperAtEnd::new(data.clone(), &target);
        let never = || false;
        let err = write_direct(
            &mut input,
            &args_for(&target, &data),
            &target,
            &reporter(Arc::new(Mutex::new(Vec::new()))),
            &never,
        )
        .unwrap_err();
        assert!(matches!(extract(&err), Extracted::Coded(c) if c.code == HASH_MISMATCH));
        assert!(altered_after_write(&err));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that read back differently is retried as `HASH_MISMATCH`, and
    /// no request that delivered its bytes is blamed, sliced or not; a
    /// request that delivered wrong bytes still is.
    #[test]
    fn altered_read_back_is_not_recorded_on_the_request() {
        let insight = || {
            Arc::new(Mutex::new(InsightItem {
                url: "http://x/f".into(),
                ttfb: 0,
                time: 0,
                size: 0,
                error: None,
                range: Vec::new(),
                mode: None,
            }))
        };
        let altered = || {
            write_failure(
                Failure::Altered {
                    written: "4 bytes aa".into(),
                    read: "4 bytes bb".into(),
                },
                Path::new("x.bin"),
            )
        };
        let wrong = || {
            write_failure(
                Failure::Hash {
                    expected: "a".into(),
                    actual: "b".into(),
                },
                Path::new("x.bin"),
            )
        };
        assert!(matches!(extract(&altered()), Extracted::Coded(c) if c.code == HASH_MISMATCH));
        let failed = fail_with_insight(altered(), &Some(insight()));
        assert!(failed.insight.is_some_and(|i| i.error.is_none()));
        let failed = fail_with_insight(wrong(), &Some(insight()));
        assert_eq!(
            failed.insight.and_then(|i| i.error).as_deref(),
            Some(HASH_MISMATCH)
        );

        let parts = SlicedParts {
            lens: vec![2, 2],
            insights: Arc::new(Mutex::new(vec![Some(insight()), Some(insight())])),
        };
        let blamed = |parts: &SlicedParts| -> Vec<Option<String>> {
            let insights = parts.insights.lock().unwrap();
            insights
                .iter()
                .map(|i| i.as_ref().unwrap().lock().unwrap().error.clone())
                .collect()
        };
        parts.blame(&altered());
        assert_eq!(blamed(&parts), [None, None]);
        parts.blame(&wrong());
        assert_eq!(
            blamed(&parts),
            [
                Some(HASH_MISMATCH.to_string()),
                Some(HASH_MISMATCH.to_string())
            ]
        );
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

    /// The decoder keeps a failed read of its source apart from data it cannot
    /// decode: a network failure under it stays that failure, undecodable data
    /// is `HASH_MISMATCH`.
    #[test]
    fn decoder_tells_source_failures_from_bad_data() {
        use crate::utils::net_failure::find;
        struct Stalled;
        impl Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other(ClassifiedNetworkError {
                    failure: NetFailure::stalled(),
                    original_error: "stalled".into(),
                    url: "http://x/f".into(),
                }))
            }
        }
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let packed = zstd::encode_all(&data[..], 3).unwrap();
        let half = packed[..packed.len() / 2].to_vec();
        let dir = temp_dir();
        let target = dir.join("out.bin");
        let args = args_for(&target, &data);
        let never = || false;
        let fail = |mut input: SyncInput| {
            let log = Arc::new(Mutex::new(Vec::new()));
            write_direct(&mut *input, &args, &target, &reporter(log), &never).unwrap_err()
        };

        let garbage = packed.iter().map(|b| b ^ 0xff).collect();
        let bad = fail(owned_input(garbage, true).unwrap());
        assert!(matches!(extract(&bad), Extracted::Coded(c) if c.code == HASH_MISMATCH));
        assert!(bad.chain().any(|e| e
            .downcast_ref::<Undecodable>()
            .is_some_and(|u| u.before.is_some())));

        let cut = std::io::BufReader::new(Read::chain(std::io::Cursor::new(half), Stalled));
        let network = fail(decoded(cut, true, None).unwrap());
        assert_eq!(
            find(network.as_ref()).map(|f| f.tag),
            Some("NET_STALLED".into())
        );
        assert!(!matches!(extract(&network), Extracted::Coded(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A whole-file download whose response declares no length and ends in
    /// the middle of the frame is a short body, on the code and the insight.
    #[tokio::test]
    async fn whole_file_cut_off_without_a_length_is_a_short_body() {
        use crate::utils::net_failure::find;
        use tokio::io::AsyncWriteExt;
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let packed = zstd::encode_all(&data[..], 3).unwrap();
        let half = packed[..packed.len() / 2].to_vec();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/f", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            let chunk = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
                half.len()
            );
            socket.write_all(chunk.as_bytes()).await.unwrap();
            socket.write_all(&half).await.unwrap();
            socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        });
        let dir = temp_dir();
        let target = dir.join("out.bin");
        let args = InstallFileArgs {
            mode: InstallFileMode::Direct(InstallFileSource::Url {
                url,
                offset: 0,
                size: 0,
                skip_decompress: false,
                request_range: None,
            }),
            ..args_for(&target, &data)
        };
        let err = ipc_install_file(args, crate::ipc::progress_noop())
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(
            find(err.error.as_ref()).map(|f| f.tag),
            Some("NET_SHORT_BODY".into())
        );
        assert_eq!(
            err.insight.and_then(|i| i.error).as_deref(),
            Some("NET_SHORT_BODY")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
