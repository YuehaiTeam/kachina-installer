pub mod commit;
pub mod staging;

use fmmap::tokio::AsyncMmapFileExt;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    dfs::InsightItem,
    ipc::{Progress, ProgressNotify},
    local::mmap,
    utils::{
        code::Cancelled,
        error::{TACommandError, TAResult},
        hash::{hash_file_with, HASH_BUFFER_SIZE},
        net_failure::{self, NetFailure, Phase},
        url::HttpContextExt,
    },
    DOWNLOAD_CLIENT,
};
use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

/// A failed read of a download body, classified where it happened. Marks the
/// io error as a network failure, so it is never taken for a local one.
#[derive(Debug)]
pub struct ClassifiedNetworkError {
    pub failure: NetFailure,
    pub original_error: Box<dyn std::error::Error + Send + Sync>,
    /// Already sanitized for logging.
    pub url: String,
}

impl std::fmt::Display for ClassifiedNetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.failure.tag, self.url, self.original_error
        )
    }
}

impl std::error::Error for ClassifiedNetworkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.original_error.as_ref())
    }
}

pub struct NetworkInsightStream<S> {
    inner: S,
    insight: Arc<Mutex<InsightItem>>,
    network_bytes: Arc<AtomicU64>,
    response_received_time: Instant,
    /// Sanitized, for [`ClassifiedNetworkError`].
    url: String,
    /// Body length the request (a range) or the response (`Content-Length`)
    /// declares. A body that ends before it or runs past it fails here: HTTP
    /// framing does not catch a chunked or close-delimited body that ends
    /// early, and the H3 middleware's body ends at the stream's FIN without
    /// checking any length.
    expected: Option<u64>,

    // Download stall detection fields
    last_stall_check: Instant,             // Last 5-second stall check time
    last_stall_check_bytes: u64,           // Bytes at last 5-second check
    slow_detection_start: Option<Instant>, // Start time for 30-second slow detection
    slow_window_start_bytes: u64,          // Bytes at start of 30-second window
}

// 为AsyncRead实现
impl<S: AsyncRead + Unpin> AsyncRead for NetworkInsightStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before_len = buf.filled().len();
        let capacity = buf.remaining();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);

        match result {
            Poll::Ready(Ok(())) => {
                let bytes_read = buf.filled().len() - before_len;
                if bytes_read > 0 {
                    // 原子更新网络字节数（高频操作，避免锁）
                    let total_bytes = self
                        .network_bytes
                        .fetch_add(bytes_read as u64, Ordering::Relaxed)
                        + bytes_read as u64;

                    // 更新insight（使用try_lock避免阻塞）
                    if let Ok(mut insight) = self.insight.try_lock() {
                        insight.size = total_bytes as u32;
                        insight.time = self.response_received_time.elapsed().as_millis() as u32;
                    }

                    let failure = if let Some(n) = self.expected.filter(|&n| total_bytes > n) {
                        let cause = format!("body runs past {n} bytes");
                        Some((NetFailure::long_body(), cause.into()))
                    } else {
                        self.check_download_health()
                            .map(|(failure, cause)| (failure, cause.into()))
                    };
                    if let Some((failure, cause)) = failure {
                        // a read that fails hands out no bytes
                        buf.set_filled(before_len);
                        return Poll::Ready(Err(self.fail(failure, cause)));
                    }
                } else if capacity > 0 {
                    let received = self.network_bytes.load(Ordering::Relaxed);
                    if let Some(expected) = self.expected.filter(|&n| received < n) {
                        let cause = format!("body ended at {received} of {expected} bytes");
                        return Poll::Ready(Err(self.fail(NetFailure::short_body(), cause.into())));
                    }
                }
                Poll::Ready(Ok(()))
            }
            // Everything under this stream is the response body.
            Poll::Ready(Err(e)) => {
                let failure = net_failure::classify(&e, Phase::Body);
                Poll::Ready(Err(self.fail(failure, Box::new(e))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> NetworkInsightStream<S> {
    pub fn new(
        stream: S,
        url: &str,
        range: Vec<(u32, u32)>,
        request_start_time: Instant,
        response_received_time: Instant,
        expected: Option<u64>,
    ) -> Self {
        let ttfb = request_start_time.elapsed().as_millis() as u32;
        let now = Instant::now();
        let url = crate::utils::url::sanitize_url_for_logging(url);

        let insight = Arc::new(Mutex::new(InsightItem {
            url: url.clone(),
            ttfb,
            time: 0,
            size: 0,
            error: None,
            range,
            mode: None,
        }));

        Self {
            inner: stream,
            insight,
            network_bytes: Arc::new(AtomicU64::new(0)),
            response_received_time,
            url,
            expected,
            last_stall_check: now,
            last_stall_check_bytes: 0,
            slow_detection_start: None,
            slow_window_start_bytes: 0,
        }
    }

    /// Records the failure in the insight and wraps it for the reader.
    fn fail(
        &self,
        failure: NetFailure,
        cause: Box<dyn std::error::Error + Send + Sync>,
    ) -> std::io::Error {
        if let Ok(mut insight) = self.insight.try_lock() {
            insight.error = Some(failure.tag.clone());
            insight.time = self.response_received_time.elapsed().as_millis() as u32;
            insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
        }
        std::io::Error::other(ClassifiedNetworkError {
            failure,
            original_error: cause,
            url: self.url.clone(),
        })
    }

    /// The failure and its description when the download is stalled or too slow.
    fn check_download_health(&mut self) -> Option<(NetFailure, &'static str)> {
        let current_bytes = self.network_bytes.load(Ordering::Relaxed);
        let now = Instant::now();

        // 1. DOWNLOAD_STALLED detection (almost no progress in 5 seconds)
        if now.duration_since(self.last_stall_check) >= Duration::from_secs(5) {
            let progress = current_bytes - self.last_stall_check_bytes;
            if progress < 5 * 1024 {
                return Some((NetFailure::stalled(), "less than 5 KiB in 5 s"));
            }
            self.last_stall_check = now;
            self.last_stall_check_bytes = current_bytes;
        }

        // 2. DOWNLOAD_TOO_SLOW detection (large file slow download)
        if let Some(total_size) = self.expected {
            if total_size > 10 * 1024 * 1024 {
                // >10MB
                let progress_ratio = current_bytes as f64 / total_size as f64;

                if progress_ratio < 0.5 {
                    // Progress < 50%
                    if self.slow_detection_start.is_none() {
                        // Start slow detection
                        self.slow_detection_start = Some(now);
                        self.slow_window_start_bytes = current_bytes;
                    } else if let Some(start_time) = self.slow_detection_start {
                        if now.duration_since(start_time) >= Duration::from_secs(30) {
                            let window_progress = current_bytes - self.slow_window_start_bytes;
                            let avg_speed = window_progress / 30; // bytes per second

                            if avg_speed < 100 * 1024 {
                                return Some((
                                    NetFailure::too_slow(),
                                    "under 100 KiB/s for 30 s before half done",
                                ));
                            }

                            // Reset 30-second window
                            self.slow_detection_start = Some(now);
                            self.slow_window_start_bytes = current_bytes;
                        }
                    }
                } else {
                    // Progress > 50%, stop slow detection
                    self.slow_detection_start = None;
                }
            }
        }

        None
    }

    /// 获取insight的共享引用，外部可以通过这个引用访问最新数据
    /// 🔑 关键方法：解决解压缩包装问题
    pub fn get_insight_handle(&self) -> Arc<Mutex<InsightItem>> {
        self.insight.clone()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Metadata {
    pub file_name: String,
    pub hash: String,
    pub size: u64,
    pub unwritable: bool,
}

/// One enumeration pass over the install directory: stat + hash of every
/// managed file, plus which directories are not wholly ours.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LocalScan {
    pub files: Vec<Metadata>,
    /// Existing directories (relative, `/`, lowercase, `""` = root) that hold
    /// something the metadata does not manage — an unmanaged file, an
    /// unmanaged or empty subdirectory, a reparse point, or a subtree whose
    /// managed files are all hash-skipped (user data). Such a directory cannot
    /// be swapped as one unit.
    pub dirty_dirs: Vec<String>,
    /// Subdirectories that are reparse points (junction / symlink); managed
    /// files under them are committed by copy, not rename.
    pub reparse_dirs: Vec<String>,
}

fn norm_rel(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase()
}

struct ScanWalk<'a> {
    root: &'a Path,
    /// lowercase `/` rel → rel as listed in the metadata
    managed: &'a HashMap<String, String>,
    managed_dirs: &'a HashSet<String>,
    all_skip_dirs: &'a HashSet<String>,
    stat: Vec<(String, u64)>,
    dirty: Vec<String>,
    reparse: Vec<String>,
    stop: &'a CancellationToken,
}

fn join_lower(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_lowercase()
    } else {
        format!("{dir}/{}", name.to_lowercase())
    }
}

impl ScanWalk<'_> {
    /// Stat the managed files under `rel` one by one (subtree not entered).
    fn stat_individually(&mut self, rel: &str) {
        let prefix = format!("{rel}/");
        let under: Vec<String> = self
            .managed
            .iter()
            .filter(|(m, _)| m.starts_with(&prefix))
            .map(|(_, orig)| orig.clone())
            .collect();
        for orig in under {
            let Some(path) = staging::try_join_rel(self.root, &orig) else {
                continue;
            };
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() {
                    self.stat.push((orig, meta.len()));
                }
            }
        }
    }

    fn walk(&mut self, path: &Path, rel: &str) -> std::io::Result<bool> {
        let mut clean = true;
        for entry in std::fs::read_dir(path)? {
            if self.stop.is_cancelled() {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let sub_rel = join_lower(rel, &name);
            let meta = entry.metadata()?;
            let is_link = meta.file_type().is_symlink() || commit::is_reparse(&meta);
            if is_link || meta.is_dir() {
                if is_link {
                    clean = false;
                    self.reparse.push(sub_rel.clone());
                    self.stat_individually(&sub_rel);
                    continue;
                }
                if !self.managed_dirs.contains(&sub_rel) {
                    clean = false;
                    continue;
                }
                if self.all_skip_dirs.contains(&sub_rel) {
                    clean = false;
                    self.stat_individually(&sub_rel);
                    continue;
                }
                if !self.walk(&entry.path(), &sub_rel)? {
                    clean = false;
                }
            } else if let Some(orig) = self.managed.get(&sub_rel) {
                self.stat.push((orig.clone(), meta.len()));
            } else {
                clean = false;
            }
        }
        if !clean {
            self.dirty.push(rel.to_string());
        }
        Ok(clean)
    }
}

fn parent_dirs(rel: &str) -> impl Iterator<Item = String> + '_ {
    let mut acc = String::new();
    let parts: Vec<&str> = rel.split('/').collect();
    let dirs = parts[..parts.len().saturating_sub(1)].to_vec();
    std::iter::once(String::new()).chain(dirs.into_iter().map(move |p| {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(p);
        acc.clone()
    }))
}

type Enumerated = (Vec<(String, u64)>, Vec<String>, Vec<String>);

fn enumerate(
    source: &Path,
    managed: &HashMap<String, String>,
    skip_hash: &HashSet<String>,
    stop: &CancellationToken,
) -> Result<Enumerated> {
    let mut managed_dirs: HashSet<String> = HashSet::new();
    let mut dir_files: HashMap<String, (usize, usize)> = HashMap::new();
    for m in managed.keys() {
        for d in parent_dirs(m) {
            managed_dirs.insert(d.clone());
            let e = dir_files.entry(d).or_insert((0, 0));
            e.0 += 1;
            if skip_hash.contains(m) {
                e.1 += 1;
            }
        }
    }
    let all_skip_dirs: HashSet<String> = dir_files
        .iter()
        .filter(|(d, (total, skipped))| !d.is_empty() && total == skipped)
        .map(|(d, _)| d.clone())
        .collect();
    let mut walk = ScanWalk {
        root: source,
        managed,
        managed_dirs: &managed_dirs,
        all_skip_dirs: &all_skip_dirs,
        stat: Vec::new(),
        dirty: Vec::new(),
        reparse: Vec::new(),
        stop,
    };
    walk.walk(source, "").context("GET_METADATA_ERR")?;
    Ok((walk.stat, walk.dirty, walk.reparse))
}

/// Stat and hash the managed files (`file_list`) under `source`, and report
/// which directories are not wholly managed.
///
/// `delete_list` holds the planned deletes. Those that exist as files are
/// hashed too, so the journal records the hash a delete expects to remove;
/// they stay unmanaged for the directory check, because a directory swapped
/// as one unit must contain only the files the journal lists.
///
/// Once `stop` is cancelled the scan returns `Cancelled`: the directory walk
/// stops at its next entry, each hash thread at its next 1 MiB block.
pub async fn check_local_files(
    source: String,
    hash_algorithm: String,
    file_list: Vec<String>,
    skip_hash: Vec<String>,
    delete_list: Vec<String>,
    notify: ProgressNotify,
    stop: CancellationToken,
) -> Result<LocalScan> {
    let source_path = PathBuf::from(&source);
    if !source_path.exists() {
        return Ok(LocalScan::default());
    }
    let skip_hash: HashSet<String> = skip_hash.iter().map(|n| norm_rel(n)).collect();
    let managed: HashMap<String, String> = file_list
        .iter()
        .map(|n| {
            (
                norm_rel(n),
                n.replace('\\', "/").trim_start_matches('/').to_string(),
            )
        })
        .filter(|(n, _)| !n.is_empty())
        .collect();

    let (stat, dirty_dirs, reparse_dirs) = {
        let source_path = source_path.clone();
        let managed = managed.clone();
        let skip_hash = skip_hash.clone();
        let walk_stop = stop.clone();
        let enumerated = tokio::task::spawn_blocking(move || {
            enumerate(&source_path, &managed, &skip_hash, &walk_stop)
        })
        .await
        .context("SCAN_THREAD_ERR")?;
        // the walk stops with an io error once `stop` is cancelled
        if stop.is_cancelled() {
            return Err(Cancelled.into());
        }
        enumerated?
    };

    let mut files = Vec::new();
    let mut stated = Vec::new();
    for (rel, size) in stat {
        let Some(abs) = staging::try_join_rel(&source_path, &rel) else {
            continue;
        };
        let item = Metadata {
            file_name: abs.to_string_lossy().to_string(),
            hash: String::new(),
            size,
            unwritable: false,
        };
        if skip_hash.contains(&norm_rel(&rel)) {
            stated.push(item);
        } else {
            files.push(item);
        }
    }
    let mut seen: HashSet<String> = managed.keys().cloned().collect();
    for rel in delete_list {
        if !seen.insert(norm_rel(&rel)) {
            continue;
        }
        let Some(abs) = staging::try_join_rel(&source_path, &rel) else {
            continue;
        };
        if let Ok(meta) = std::fs::metadata(&abs) {
            if meta.is_file() {
                files.push(Metadata {
                    file_name: abs.to_string_lossy().to_string(),
                    hash: String::new(),
                    size: meta.len(),
                    unwritable: false,
                });
            }
        }
    }

    // 大文件按大小降序排在最前，避免最后剩一个大文件拖尾；小文件保持目录枚举顺序，
    // 同一目录的文件连续读取。sort_by_key 是稳定排序。
    const LARGE_FILE: u64 = 1024 * 1024;
    files.sort_by_key(|file| {
        std::cmp::Reverse(if file.size >= LARGE_FILE {
            file.size
        } else {
            0
        })
    });
    let len = files.len() + stated.len();
    notify(Progress::CountOf {
        done: 0,
        total: len as u64,
    });
    let scan = LocalScan {
        files: Vec::new(),
        dirty_dirs,
        reparse_dirs,
    };
    if len == 0 {
        return Ok(scan);
    }
    if files.is_empty() {
        notify(Progress::CountOf {
            done: len as u64,
            total: len as u64,
        });
        return Ok(LocalScan {
            files: stated,
            ..scan
        });
    }

    // 每个 CPU 一个阻塞线程，按顺序领取文件，整个扫描期间复用各自的读缓冲。
    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(files.len());
    let files = Arc::new(files);
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Metadata>>();
    for _ in 0..workers {
        let (files, next, tx) = (files.clone(), next.clone(), tx.clone());
        let hash_algorithm = hash_algorithm.clone();
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || {
            let mut buffer = vec![0u8; HASH_BUFFER_SIZE];
            let stopped = || stop.is_cancelled();
            loop {
                if stopped() {
                    break;
                }
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(file) = files.get(i) else { break };
                let res = hash_file_with(&hash_algorithm, &file.file_name, &mut buffer, &stopped)
                    .map(|hash| Metadata {
                        hash,
                        unwritable: false,
                        ..file.clone()
                    });
                let failed = res.is_err();
                // 接收端已因错误返回时停止
                if tx.send(res).is_err() || failed {
                    break;
                }
            }
        });
    }
    drop(tx);

    let mut finished = stated.len();
    let mut finished_hashes = Vec::with_capacity(len);
    finished_hashes.extend(stated);
    let mut last_notify = Instant::now();
    const PROGRESS_FRAME: Duration = Duration::from_millis(50);

    loop {
        // a thread that saw `stop` sends an error or nothing; `stop` wins
        let res = tokio::select! {
            biased;
            _ = stop.cancelled() => return Err(Cancelled.into()),
            res = rx.recv() => res,
        };
        let Some(res) = res else { break };
        let res = res.context("HASH_COMPLETE_ERR")?;
        finished += 1;
        finished_hashes.push(res);
        if finished == len || last_notify.elapsed() >= PROGRESS_FRAME {
            notify(Progress::CountOf {
                done: finished as u64,
                total: len as u64,
            });
            last_notify = Instant::now();
        }
    }
    // 工作线程 panic 时通道提前关闭
    if finished != len {
        return Err(anyhow::anyhow!("hash worker exited early")).context("HASH_THREAD_ERR");
    }

    Ok(LocalScan {
        files: finished_hashes,
        ..scan
    })
}

pub async fn probe_writable(file_list: Vec<String>) -> Vec<String> {
    tokio::task::spawn_blocking(move || {
        file_list
            .into_iter()
            .filter(|path| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .is_err()
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

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
    let exe_path = path.join(exe_name.clone());
    if !exe_name.is_empty() && exe_path.exists() {
        return (false, true);
    }
    let mut entries = entries.unwrap();
    if let Ok(Some(_entry)) = entries.next_entry().await {
        return (false, false);
    }
    (true, false)
}

pub async fn ensure_dir(path: String) -> Result<(), anyhow::Error> {
    let path = Path::new(&path);
    tokio::fs::create_dir_all(path)
        .await
        .context("CREATE_DIR_ERR")?;
    Ok(())
}

type HttpStream = (
    Box<dyn AsyncRead + Unpin + Send>,
    u64,
    Arc<Mutex<InsightItem>>,
);

/// Raw bytes of `url` (or of `offset..offset+size` when `size > 0`); callers
/// decompress themselves.
pub async fn create_http_stream(
    url: &str,
    offset: usize,
    size: usize,
    request_range: Option<&str>,
) -> TAResult<HttpStream> {
    let reading = crate::ipc::network::Reading::begin();
    let permit = crate::ipc::download::request().await?;
    open_http_stream(url, offset, size, request_range, reading, permit).await
}

/// Opens a package range whose URL the session resolves. The request slot is
/// taken before the URL is requested: the DFS2 server counts a URL request as
/// the range being downloaded next. Must run inside a download job context.
pub async fn create_resolved_stream(
    offset: usize,
    size: usize,
    request_range: Option<&str>,
) -> TAResult<HttpStream> {
    let reading = crate::ipc::network::Reading::begin();
    let permit = crate::ipc::download::request().await?;
    let url = crate::ipc::download::resolve(offset as u64, size as u64).await?;
    open_http_stream(&url, offset, size, request_range, reading, permit).await
}

async fn open_http_stream(
    url: &str,
    offset: usize,
    size: usize,
    request_range: Option<&str>,
    reading: crate::ipc::network::Reading,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> TAResult<HttpStream> {
    let request_start_time = Instant::now();
    let has_range = size > 0;
    let insight_range = insight_range_vec(request_range, offset, size);

    // 构建HTTP请求
    let mut builder = DOWNLOAD_CLIENT
        .get(url)
        .header("Accept-Encoding", "identity");
    if has_range {
        builder = builder.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
    }

    // 发送请求
    let res = crate::ipc::download::cancellable(builder.send())
        .await?
        .with_http_context("create_http_stream", url);
    let response_received_time = Instant::now();

    // A request that fails before the body records its insight here.
    let failed = |error: anyhow::Error| {
        let insight = Arc::new(Mutex::new(InsightItem {
            url: crate::utils::url::sanitize_url_for_logging(url),
            ttfb: request_start_time.elapsed().as_millis() as u32,
            time: 0,
            size: 0,
            error: Some(crate::utils::code::insight_code(&error)),
            range: insight_range.clone(),
            mode: None,
        }));
        TACommandError::with_insight_handle(error, insight)
    };

    let res = res.map_err(&failed)?;

    // HTTP状态码检查
    let code = res.status();
    if (!has_range && code != 200) || (has_range && code != 206) {
        let error = anyhow::Error::new(crate::dfs::HttpStatus::new(code.as_u16(), "")).context(
            crate::utils::url::create_reqwest_context("create_http_stream", url, "HTTP_STATUS_ERR"),
        );
        return Err(failed(error));
    }

    // The header, not `content_length()`: that is the body's size hint, unknown
    // for a middleware's wrapped stream even when the header is there.
    let declared = res
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    if has_range {
        if let Err(cause) = validate_range_response(&res, offset as u64, size as u64, declared) {
            let error = anyhow::Error::new(ClassifiedNetworkError {
                failure: NetFailure::bad_range(),
                original_error: cause.into(),
                url: crate::utils::url::sanitize_url_for_logging(url),
            });
            return Err(failed(error));
        }
    }
    let expected = if has_range {
        Some(size as u64)
    } else {
        declared
    };
    let stream = res.bytes_stream();
    let reader = tokio_util::io::StreamReader::new(stream.map_err(std::io::Error::other));

    // 创建NetworkInsightStream包装
    let insight_stream = NetworkInsightStream::new(
        reader,
        url,
        insight_range,
        request_start_time,
        response_received_time,
        expected,
    );

    let insight_handle = insight_stream.get_insight_handle();
    let insight_stream =
        crate::ipc::network::Reader::new(insight_stream, reading, permit, expected);
    Ok((
        Box::new(insight_stream),
        expected.unwrap_or(0),
        insight_handle,
    ))
}

/// 包内范围必须保持原始字节位置；拒绝服务器返回其他范围或额外内容编码。
fn validate_range_response(
    response: &reqwest::Response,
    start: u64,
    len: u64,
    declared: Option<u64>,
) -> anyhow::Result<()> {
    let range = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok());
    let expected = format!("bytes {}-{}/", start, start + len - 1);
    anyhow::ensure!(
        range.is_some_and(|v| v.starts_with(&expected)),
        "unexpected Content-Range: {range:?}"
    );
    if let Some(n) = declared {
        anyhow::ensure!(n == len, "range of {n} bytes, requested {len}");
    }
    anyhow::ensure!(
        response
            .headers()
            .get("content-encoding")
            .is_none_or(|v| v == "identity"),
        "encoded range response"
    );
    Ok(())
}

fn parse_range_string(range: &str) -> Vec<(u32, u32)> {
    range
        .split(',')
        .filter_map(|part| {
            let (start, end) = part.trim().split_once('-')?;
            let start = start.parse::<u32>().ok()?;
            if end.is_empty() {
                Some((start, u32::MAX))
            } else {
                Some((start, end.parse::<u32>().ok()?))
            }
        })
        .collect()
}

fn insight_range_vec(request_range: Option<&str>, offset: usize, size: usize) -> Vec<(u32, u32)> {
    if size == 0 {
        return vec![];
    }
    if let Some(range) = request_range {
        let parsed = parse_range_string(range);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    vec![(offset as u32, (offset + size - 1) as u32)]
}

/// Raw bytes of a block inside the running package.
pub async fn create_local_stream(
    offset: usize,
    size: usize,
) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + std::marker::Send>, anyhow::Error> {
    let mmap_file = mmap().await;
    let reader = mmap_file.range_reader(offset, size).context("MMAP_ERR")?;
    Ok(Box::new(reader))
}

/// [`create_staged_file`] for blocking writers; the handle is unbuffered.
pub fn create_staged_file_blocking(path: &Path) -> Result<std::fs::File, anyhow::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("CREATE_PARENT_DIR_ERR")?;
    }
    std::fs::File::create(path).context("CREATE_TARGET_FILE_ERR")
}

/// Create a file under the staging directory (parents included). This is the
/// only producer of files in the install pipeline; nothing writes into the
/// install directory directly.
pub async fn create_staged_file(
    path: &Path,
) -> Result<tokio::io::BufWriter<tokio::fs::File>, anyhow::Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("CREATE_PARENT_DIR_ERR")?;
    }
    let file = tokio::fs::File::create(path)
        .await
        .context("CREATE_TARGET_FILE_ERR")?;
    Ok(tokio::io::BufWriter::new(file))
}

/// `FlushFileBuffers` on a finished staged file: rename is atomic only for
/// metadata, so an unflushed file swapped in right before power loss would
/// come back truncated.
pub async fn sync_staged_file(path: &Path) -> Result<(), anyhow::Error> {
    // Windows requires a writable handle for FlushFileBuffers.
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await
        .context("OPEN_TARGET_ERR")?;
    file.sync_all().await.context("SYNC_TARGET_ERR")?;
    Ok(())
}

pub async fn progressed_copy(
    source: &mut (dyn AsyncRead + Unpin + Send),
    target: &mut (dyn AsyncWrite + Unpin + Send),
    on_progress: &(dyn Fn(usize) + Send + Sync),
) -> Result<usize, anyhow::Error> {
    let mut downloaded = 0;
    let mut boxed = Box::new([0u8; 256 * 1024]);
    let buffer = &mut *boxed;
    let mut now = std::time::Instant::now();

    loop {
        let read = source.read(buffer).await?;
        if read == 0 {
            break;
        }
        downloaded += read;

        if now.elapsed().as_millis() >= 20 {
            now = std::time::Instant::now();
            on_progress(downloaded);
        }
        target
            .write_all(&buffer[..read])
            .await
            .context("WRITE_TARGET_ERR")?;
    }

    target.flush().await.context("FLUSH_TARGET_ERR")?;
    on_progress(downloaded);

    Ok(downloaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::progress_notify;
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::{Arc, Mutex};

    fn temp_dir() -> std::path::PathBuf {
        // 不能用时间戳命名：Windows 时钟粗化到 15.6ms 节拍，并行测试会取到相同值而共用目录
        let dir = std::env::temp_dir().join(format!("kachina-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        path
    }

    #[tokio::test]
    async fn check_local_files_is_read_only_and_unwritable_false() {
        let dir = temp_dir();
        write_file(&dir, "app.exe", b"hello");
        let path = dir.join("app.exe");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let reports = Arc::new(Mutex::new(Vec::new()));
        let reports_cb = reports.clone();
        let scanned = check_local_files(
            dir.to_string_lossy().to_string(),
            "md5".to_string(),
            vec!["app.exe".to_string()],
            vec![],
            vec![],
            progress_notify(move |v| reports_cb.lock().unwrap().push(v)),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(scanned.dirty_dirs.is_empty(), "{:?}", scanned.dirty_dirs);
        let scanned = scanned.files;

        assert_eq!(scanned.len(), 1);
        assert!(!scanned[0].unwritable);
        assert_eq!(scanned[0].hash, "5d41402abc4b2a76b9719d911017c592");
        let reports = reports.lock().unwrap();
        assert_eq!(
            reports.first(),
            Some(&Progress::CountOf { done: 0, total: 1 })
        );
        assert_eq!(
            reports.last(),
            Some(&Progress::CountOf { done: 1, total: 1 })
        );

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        // Windows: clears FILE_ATTRIBUTE_READONLY so the test file can be deleted.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(&path, perms);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn check_local_files_skip_hash_only_stats() {
        let dir = temp_dir();
        write_file(&dir, "User/settings.json", b"user-modified");
        write_file(&dir, "app.exe", b"hello");
        let scanned = check_local_files(
            dir.to_string_lossy().to_string(),
            "md5".to_string(),
            vec!["app.exe".to_string(), "User/settings.json".to_string()],
            vec!["User/settings.json".to_string()],
            vec![],
            progress_notify(|_| {}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        // the user-data subtree is not entered and makes the root dirty
        assert_eq!(scanned.dirty_dirs, vec![String::new()]);
        let scanned = scanned.files;
        let user = scanned
            .iter()
            .find(|f| {
                f.file_name
                    .replace('\\', "/")
                    .ends_with("User/settings.json")
            })
            .unwrap();
        let app = scanned
            .iter()
            .find(|f| f.file_name.replace('\\', "/").ends_with("app.exe"))
            .unwrap();
        assert!(user.hash.is_empty());
        assert_eq!(app.hash, "5d41402abc4b2a76b9719d911017c592");
        assert!(scanned.iter().all(|f| !f.unwritable));
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn scan(dir: &Path, managed: &[&str], skip: &[&str]) -> LocalScan {
        check_local_files(
            dir.to_string_lossy().to_string(),
            "md5".to_string(),
            managed.iter().map(|s| s.to_string()).collect(),
            skip.iter().map(|s| s.to_string()).collect(),
            vec![],
            progress_notify(|_| {}),
            CancellationToken::new(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn dirty_dirs_follow_unmanaged_content() {
        let dir = temp_dir();
        write_file(&dir, "app.exe", b"a");
        write_file(&dir, "lib/a.dll", b"a");
        write_file(&dir, "lib/b.dll", b"b");
        write_file(&dir, "plugins/x/p.dll", b"p");
        write_file(&dir, "notes.txt", b"user file at root");
        let managed = ["app.exe", "lib/a.dll", "lib/b.dll", "plugins/x/p.dll"];

        // an unmanaged file at the root dirties only the root
        let s = scan(&dir, &managed, &[]).await;
        assert_eq!(s.dirty_dirs, vec![String::new()]);
        assert_eq!(s.files.len(), 4);

        // an unmanaged file inside lib dirties lib and, through it, the root
        write_file(&dir, "lib/user.cfg", b"mine");
        let mut s = scan(&dir, &managed, &[]).await;
        s.dirty_dirs.sort();
        assert_eq!(s.dirty_dirs, vec![String::new(), "lib".to_string()]);
        std::fs::remove_file(dir.join("lib/user.cfg")).unwrap();

        // an empty unmanaged subdirectory dirties its parent
        std::fs::create_dir_all(dir.join("plugins/empty")).unwrap();
        let mut s = scan(&dir, &managed, &[]).await;
        s.dirty_dirs.sort();
        assert_eq!(s.dirty_dirs, vec![String::new(), "plugins".to_string()]);
        assert!(!s.dirty_dirs.contains(&"plugins/x".to_string()));
        std::fs::remove_dir(dir.join("plugins/empty")).unwrap();

        // a fully hash-skipped subtree (user data) is dirty and not entered,
        // but its files are still stat'd
        write_file(&dir, "User/s.json", b"u");
        let s = scan(
            &dir,
            &[
                "app.exe",
                "lib/a.dll",
                "lib/b.dll",
                "plugins/x/p.dll",
                "User/s.json",
            ],
            &["User/s.json"],
        )
        .await;
        assert!(s
            .files
            .iter()
            .any(|f| f.file_name.ends_with("s.json") && f.hash.is_empty()));
        assert!(s.dirty_dirs.contains(&String::new()));
        assert!(!s.dirty_dirs.contains(&"lib".to_string()));

        // a directory that is only managed files is clean; a missing install dir is clean
        std::fs::remove_file(dir.join("notes.txt")).unwrap();
        std::fs::remove_dir_all(dir.join("User")).unwrap();
        let s = scan(&dir, &managed, &[]).await;
        assert!(s.dirty_dirs.is_empty(), "{:?}", s.dirty_dirs);
        let s = scan(&dir.join("nope"), &managed, &[]).await;
        assert!(s.dirty_dirs.is_empty() && s.files.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn junction_subdir_is_reparse_and_dirty() {
        let dir = temp_dir();
        let real = temp_dir();
        write_file(&real, "f.dll", b"f");
        write_file(&dir, "app.exe", b"a");
        let link = dir.join("link");
        let out = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &real.to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        let s = scan(&dir, &["app.exe", "link/f.dll"], &[]).await;
        assert_eq!(s.reparse_dirs, vec!["link".to_string()]);
        assert_eq!(s.dirty_dirs, vec![String::new()]);
        assert!(s
            .files
            .iter()
            .any(|f| f.file_name.ends_with("f.dll") && !f.hash.is_empty()));
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&real);
    }

    #[tokio::test]
    async fn probe_writable_marks_locked_file() {
        let dir = temp_dir();
        let free = write_file(&dir, "free.dll", b"ok");
        let locked = write_file(&dir, "locked.dll", b"busy");
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&locked)
            .unwrap();
        let unwritable = probe_writable(vec![
            free.to_string_lossy().to_string(),
            locked.to_string_lossy().to_string(),
        ])
        .await;
        assert!(!unwritable.iter().any(|p| p.ends_with("free.dll")));
        assert!(unwritable.iter().any(|p| p.ends_with("locked.dll")));
        drop(_hold);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cancel that lands before the hash threads take a file ends the scan
    /// with `Cancelled`, not with the thread error a short count would give.
    #[tokio::test]
    async fn cancelled_scan_returns_cancelled() {
        let dir = temp_dir();
        let names: Vec<String> = (0..20).map(|i| format!("f{i}.bin")).collect();
        for name in &names {
            write_file(&dir, name, name.as_bytes());
        }
        let stop = CancellationToken::new();
        let on_first = stop.clone();
        let err = check_local_files(
            dir.to_string_lossy().to_string(),
            "md5".to_string(),
            names,
            vec![],
            vec![],
            // the first report comes before any file is hashed
            progress_notify(move |_| on_first.cancel()),
            stop,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                crate::utils::code::extract(&err),
                crate::utils::code::Extracted::Cancelled
            ),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn check_local_files_hashes_every_file_once() {
        let dir = temp_dir();
        let mut names = Vec::new();
        for i in 0..40 {
            let name = format!("d{}/f{i}.bin", i % 3);
            write_file(&dir, &name, format!("content-{i}").as_bytes());
            names.push(name);
        }
        write_file(&dir, "big.bin", &vec![7u8; 2 * 1024 * 1024]);
        names.push("big.bin".into());
        let scanned = check_local_files(
            dir.to_string_lossy().to_string(),
            "md5".to_string(),
            names.clone(),
            vec![],
            vec![],
            progress_notify(|_| {}),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .files;
        assert_eq!(scanned.len(), names.len());
        for name in &names {
            let path = dir.join(name);
            let file = scanned
                .iter()
                .find(|f| Path::new(&f.file_name) == path)
                .unwrap_or_else(|| panic!("{name} missing"));
            let expected = chksum_md5::hash(std::fs::read(&path).unwrap()).to_hex_lowercase();
            assert_eq!(file.hash, expected, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn planned_deletes_are_hashed_but_stay_unmanaged() {
        let dir = temp_dir();
        write_file(&dir, "app.exe", b"a");
        write_file(&dir, "lib/new.dll", b"n");
        write_file(&dir, "lib/old.dll", b"retired");
        let mut scanned = check_local_files(
            dir.to_string_lossy().to_string(),
            "md5".to_string(),
            vec!["app.exe".into(), "lib/new.dll".into()],
            vec![],
            vec![
                "lib/old.dll".into(),
                "lib/missing.dll".into(),
                "app.exe".into(),
            ],
            progress_notify(|_| {}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let old = scanned
            .files
            .iter()
            .find(|f| f.file_name.ends_with("old.dll"))
            .expect("existing delete is hashed");
        assert_eq!(old.hash, chksum_md5::hash(b"retired").to_hex_lowercase());
        // app.exe 同时在两张表里也只算一次；不存在的待删文件不出现
        assert_eq!(scanned.files.len(), 3);
        // 待删文件仍按未托管内容算：lib 是脏目录，不会被整目录换入
        scanned.dirty_dirs.sort();
        assert_eq!(scanned.dirty_dirs, vec![String::new(), "lib".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_range_keeps_open_end() {
        assert_eq!(parse_range_string("0-"), vec![(0, u32::MAX)]);
        assert_eq!(parse_range_string("100-200"), vec![(100, 200)]);
        assert_eq!(insight_range_vec(Some("10-14"), 0, 5), vec![(10, 14)]);
        assert_eq!(insight_range_vec(None, 10, 5), vec![(10, 14)]);
        assert_eq!(insight_range_vec(Some("0-"), 0, 0), vec![]);
        assert_eq!(insight_range_vec(None, 0, 0), vec![]);
    }
}
