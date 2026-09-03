pub mod commit;
pub mod staging;

use crate::utils::code::{code_for_network_type, Attach};
use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use bytes::Bytes;
use fmmap::tokio::AsyncMmapFileExt;
use futures::Stream;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};

use crate::{
    dfs::{apply_insight_io_error, InsightItem},
    ipc::{Progress, ProgressNotify},
    local::mmap,
    utils::{
        error::{TACommandError, TAResult, DOWNLOAD_STALLED, DOWNLOAD_TOO_SLOW},
        hash::run_hash,
        progressed_read::ReadWithCallback,
        url::HttpContextExt,
    },
    DOWNLOAD_CLIENT,
};
use anyhow::{Context, Result};

#[derive(Debug, Clone, Serialize)]
pub enum NetworkErrorType {
    ConnectionReset,
    ConnectionTimeout,
    StreamError,
    DnsResolutionFailed,
    TlsHandshakeError,
    HttpProtocolError,
    NetworkUnreachable,
    RequestTimeout,
    ResponseBodyError,
    DownloadStalled,
    DownloadTooSlow,
    Other(String),
}

#[derive(Debug)]
pub struct ClassifiedNetworkError {
    pub error_type: NetworkErrorType,
    pub original_error: Box<dyn std::error::Error + Send + Sync>,
    pub context: String,
    pub url: String,
    pub range: Vec<(u32, u32)>,
}

impl ClassifiedNetworkError {
    pub fn new(
        error_type: NetworkErrorType,
        original_error: Box<dyn std::error::Error + Send + Sync>,
        url: String,
        range: Vec<(u32, u32)>,
    ) -> Self {
        let context = match &error_type {
            NetworkErrorType::ConnectionReset => "ERR_CONNECTION_RESET",
            NetworkErrorType::ConnectionTimeout => "ERR_CONNECTION_TIMEOUT",
            NetworkErrorType::StreamError => "ERR_STREAM_ERROR",
            NetworkErrorType::DnsResolutionFailed => "ERR_DNS_RESOLUTION_FAILED",
            NetworkErrorType::TlsHandshakeError => "ERR_TLS_HANDSHAKE_ERROR",
            NetworkErrorType::HttpProtocolError => "ERR_HTTP_PROTOCOL_ERROR",
            NetworkErrorType::NetworkUnreachable => "ERR_NETWORK_UNREACHABLE",
            NetworkErrorType::RequestTimeout => "ERR_REQUEST_TIMEOUT",
            NetworkErrorType::ResponseBodyError => "ERR_RESPONSE_BODY_ERROR",
            NetworkErrorType::DownloadStalled => "ERR_DOWNLOAD_STALLED",
            NetworkErrorType::DownloadTooSlow => "ERR_DOWNLOAD_TOO_SLOW",
            NetworkErrorType::Other(_) => "ERR_NETWORK_OTHER",
        };

        Self {
            error_type,
            original_error,
            context: context.to_string(),
            url,
            range,
        }
    }

    /// 分析错误并分类
    pub fn classify_error(error: &dyn std::error::Error) -> NetworkErrorType {
        let error_str = error.to_string().to_lowercase();

        if error_str.contains("connection reset") || error_str.contains("connection was reset") {
            NetworkErrorType::ConnectionReset
        } else if error_str.contains("download_stalled") {
            NetworkErrorType::DownloadStalled
        } else if error_str.contains("download_too_slow") {
            NetworkErrorType::DownloadTooSlow
        } else if error_str.contains("timed out") || error_str.contains("timeout") {
            if error_str.contains("connect") || error_str.contains("connection") {
                NetworkErrorType::ConnectionTimeout
            } else {
                NetworkErrorType::RequestTimeout
            }
        } else if error_str.contains("stream error")
            || error_str.contains("unexpected internal error")
        {
            NetworkErrorType::StreamError
        } else if error_str.contains("dns") || error_str.contains("name resolution") {
            NetworkErrorType::DnsResolutionFailed
        } else if error_str.contains("tls")
            || error_str.contains("ssl")
            || error_str.contains("handshake")
        {
            NetworkErrorType::TlsHandshakeError
        } else if error_str.contains("http")
            && (error_str.contains("protocol") || error_str.contains("invalid"))
        {
            NetworkErrorType::HttpProtocolError
        } else if error_str.contains("network unreachable") || error_str.contains("no route") {
            NetworkErrorType::NetworkUnreachable
        } else if error_str.contains("error decoding response body")
            || error_str.contains("response body error")
        {
            NetworkErrorType::ResponseBodyError
        } else {
            NetworkErrorType::Other(error.to_string())
        }
    }
}

impl std::fmt::Display for ClassifiedNetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.context,
            crate::utils::url::sanitize_url_for_logging(&self.url),
            self.original_error
        )
    }
}

impl std::error::Error for ClassifiedNetworkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.original_error.as_ref())
    }
}

// 为了与现有的anyhow错误系统兼容，实现到io::Error的转换
impl From<ClassifiedNetworkError> for std::io::Error {
    fn from(err: ClassifiedNetworkError) -> Self {
        let error_kind = match err.error_type {
            NetworkErrorType::ConnectionReset => std::io::ErrorKind::ConnectionReset,
            NetworkErrorType::ConnectionTimeout => std::io::ErrorKind::TimedOut,
            NetworkErrorType::RequestTimeout => std::io::ErrorKind::TimedOut,
            NetworkErrorType::DownloadStalled => std::io::ErrorKind::TimedOut,
            NetworkErrorType::DownloadTooSlow => std::io::ErrorKind::TimedOut,
            NetworkErrorType::NetworkUnreachable => std::io::ErrorKind::NetworkUnreachable,
            _ => std::io::ErrorKind::Other,
        };

        std::io::Error::new(error_kind, err)
    }
}

pub struct NetworkInsightStream<S> {
    inner: S,
    insight: Arc<Mutex<InsightItem>>,
    network_bytes: Arc<AtomicU64>,
    response_received_time: Instant,
    url: String,            // 新增：保存URL用于错误处理
    range: Vec<(u32, u32)>, // 新增：保存Range用于错误处理

    // Download stall detection fields
    content_length: Option<u64>,           // Total file size
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

                    // Check download health
                    if let Err(classified_error) = self.check_download_health() {
                        // Update insight with classified error
                        if let Ok(mut insight) = self.insight.try_lock() {
                            insight.error = Some(
                                code_for_network_type(&classified_error.error_type).to_string(),
                            );
                            insight.time = self.response_received_time.elapsed().as_millis() as u32;
                            insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
                        }
                        return Poll::Ready(Err(classified_error.into()));
                    }
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
                // 检查是否为网络错误并创建分类错误
                let error_type = ClassifiedNetworkError::classify_error(&e);
                let is_network_error = !matches!(error_type, NetworkErrorType::Other(_));

                if is_network_error {
                    // 创建分类后的网络错误，保留原始错误链
                    let classified_error = ClassifiedNetworkError::new(
                        error_type,
                        Box::new(e), // 保存完整的原始错误
                        self.url.clone(),
                        self.range.clone(),
                    );

                    // 更新insight
                    if let Ok(mut insight) = self.insight.try_lock() {
                        insight.error =
                            Some(code_for_network_type(&classified_error.error_type).to_string());
                        insight.time = self.response_received_time.elapsed().as_millis() as u32;
                        insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
                    }

                    // 返回分类后的网络错误
                    Poll::Ready(Err(classified_error.into()))
                } else {
                    // 非网络错误：更新insight，然后保持原始错误传播
                    if let Ok(mut insight) = self.insight.try_lock() {
                        apply_insight_io_error(&mut insight, &e);
                        insight.time = self.response_received_time.elapsed().as_millis() as u32;
                        insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
                    }
                    Poll::Ready(Err(e))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// 为Stream实现
impl<S, E> Stream for NetworkInsightStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let result = Pin::new(&mut self.inner).poll_next(cx);

        match &result {
            Poll::Ready(Some(Ok(bytes))) => {
                // 原子更新网络字节数
                let total_bytes = self
                    .network_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed)
                    + bytes.len() as u64;

                // 更新insight
                if let Ok(mut insight) = self.insight.try_lock() {
                    insight.size = total_bytes as u32;
                    insight.time = self.response_received_time.elapsed().as_millis() as u32;
                }

                // Note: Download health check is mainly handled in AsyncRead implementation
                // For streams, the check will happen when data is actually read
            }
            Poll::Ready(Some(Err(e))) => {
                // Stream 实现中只更新 insight，因为泛型 E 的限制
                // 实际的错误处理会在转换为 AsyncRead 时进行
                let io_error = std::io::Error::other(e.to_string());
                let error_type = ClassifiedNetworkError::classify_error(&io_error);
                let is_network_error = !matches!(error_type, NetworkErrorType::Other(_));

                // 更新insight
                if let Ok(mut insight) = self.insight.try_lock() {
                    if is_network_error {
                        insight.error = Some(code_for_network_type(&error_type).to_string());
                    } else {
                        apply_insight_io_error(&mut insight, &io_error);
                    }
                    insight.time = self.response_received_time.elapsed().as_millis() as u32;
                    insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
                }
                // 错误继续向上传播，在被转换为 AsyncRead 时会得到正确处理
            }
            Poll::Ready(None) => {
                // 流结束，最终更新时间
                if let Ok(mut insight) = self.insight.try_lock() {
                    insight.time = self.response_received_time.elapsed().as_millis() as u32;
                    insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
                }
            }
            _ => {}
        }
        result
    }
}

impl<S> NetworkInsightStream<S> {
    pub fn new(
        stream: S,
        url: String,
        range: Vec<(u32, u32)>,
        request_start_time: Instant,
        response_received_time: Instant,
    ) -> Self {
        Self::new_with_detection(
            stream,
            url,
            range,
            request_start_time,
            response_received_time,
            None,
        )
    }

    pub fn new_with_detection(
        stream: S,
        url: String,
        range: Vec<(u32, u32)>,
        request_start_time: Instant,
        response_received_time: Instant,
        content_length: Option<u64>,
    ) -> Self {
        let ttfb = request_start_time.elapsed().as_millis() as u32;
        let now = Instant::now();

        let insight = Arc::new(Mutex::new(InsightItem {
            url: crate::utils::url::sanitize_url_for_logging(&url),
            ttfb,
            time: 0,
            size: 0,
            error: None,
            range: range.clone(),
            mode: None,
        }));

        Self {
            inner: stream,
            insight,
            network_bytes: Arc::new(AtomicU64::new(0)),
            response_received_time,
            url: crate::utils::url::sanitize_url_for_logging(&url), // 保存URL
            range,                                                  // 保存Range
            content_length,
            last_stall_check: now,
            last_stall_check_bytes: 0,
            slow_detection_start: None,
            slow_window_start_bytes: 0,
        }
    }

    /// Check for download health issues
    /// Returns ClassifiedNetworkError if download is stalled or too slow
    fn check_download_health(&mut self) -> Result<(), ClassifiedNetworkError> {
        let current_bytes = self.network_bytes.load(Ordering::Relaxed);
        let now = Instant::now();

        // 1. DOWNLOAD_STALLED detection (almost no progress in 5 seconds)
        if now.duration_since(self.last_stall_check) >= Duration::from_secs(5) {
            let progress = current_bytes - self.last_stall_check_bytes;
            if progress < 5 * 1024 {
                // <5KB in 5 seconds
                let base_error =
                    std::io::Error::new(std::io::ErrorKind::TimedOut, DOWNLOAD_STALLED);
                return Err(ClassifiedNetworkError::new(
                    NetworkErrorType::DownloadStalled,
                    Box::new(base_error),
                    self.url.clone(),
                    self.range.clone(),
                ));
            }
            self.last_stall_check = now;
            self.last_stall_check_bytes = current_bytes;
        }

        // 2. DOWNLOAD_TOO_SLOW detection (large file slow download)
        if let Some(total_size) = self.content_length {
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
                                // <100KB/s
                                let base_error = std::io::Error::other(DOWNLOAD_TOO_SLOW);
                                return Err(ClassifiedNetworkError::new(
                                    NetworkErrorType::DownloadTooSlow,
                                    Box::new(base_error),
                                    self.url.clone(),
                                    self.range.clone(),
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

        Ok(())
    }

    /// 获取insight的共享引用，外部可以通过这个引用访问最新数据
    /// 🔑 关键方法：解决解压缩包装问题
    pub fn get_insight_handle(&self) -> Arc<Mutex<InsightItem>> {
        self.insight.clone()
    }

    /// 获取当前insight的快照
    pub fn get_insight_snapshot(&self) -> InsightItem {
        if let Ok(insight) = self.insight.lock() {
            insight.clone()
        } else {
            // fallback
            InsightItem {
                url: "unknown".to_string(),
                ttfb: 0,
                time: 0,
                size: self.network_bytes.load(Ordering::Relaxed) as u32,
                error: Some("Failed to lock insight".to_string()),
                range: vec![],
                mode: None,
            }
        }
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

fn enumerate(
    source: &Path,
    managed: &HashMap<String, String>,
    skip_hash: &HashSet<String>,
) -> Result<(Vec<(String, u64)>, Vec<String>, Vec<String>)> {
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
    };
    walk.walk(source, "").context("GET_METADATA_ERR")?;
    Ok((walk.stat, walk.dirty, walk.reparse))
}

pub async fn check_local_files(
    source: String,
    hash_algorithm: String,
    file_list: Vec<String>,
    skip_hash: Vec<String>,
    notify: ProgressNotify,
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
        tokio::task::spawn_blocking(move || enumerate(&source_path, &managed, &skip_hash))
            .await
            .context("SCAN_THREAD_ERR")??
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

    files.sort_by(|a, b| b.size.cmp(&a.size));
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

    let hash_concurrency = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(hash_concurrency));
    let mut joinset = tokio::task::JoinSet::new();

    for file in files.iter() {
        let hash_algorithm = hash_algorithm.clone();
        let mut file = file.clone();
        let semaphore = semaphore.clone();
        joinset.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .context("HASH_SEMAPHORE_ERR")?;
            file.hash = run_hash(&hash_algorithm, &file.file_name).await?;
            file.unwritable = false;
            Ok::<Metadata, anyhow::Error>(file)
        });
    }

    let mut finished = stated.len();
    let mut finished_hashes = Vec::with_capacity(len);
    finished_hashes.extend(stated);
    let mut last_notify = Instant::now();
    const PROGRESS_FRAME: Duration = Duration::from_millis(50);

    while let Some(res) = joinset.join_next().await {
        let res = res.context("HASH_THREAD_ERR")?;
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

    Ok(LocalScan {
        files: finished_hashes,
        ..scan
    })
}

pub async fn probe_writable(file_list: Vec<String>) -> Vec<String> {
    let mut unwritable = Vec::new();
    for path in file_list {
        let writable = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await
            .is_ok();
        if !writable {
            unwritable.push(path);
        }
    }
    unwritable
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

pub async fn create_http_stream(
    url: &str,
    offset: usize,
    size: usize,
    skip_decompress: bool,
    request_range: Option<&str>,
) -> TAResult<(
    Box<dyn AsyncRead + Unpin + Send>,
    u64,
    Arc<Mutex<InsightItem>>,
)> {
    let request_start_time = Instant::now();
    let has_range = size > 0;
    let insight_range = insight_range_vec(request_range, offset, size);

    // 构建HTTP请求
    let mut builder = DOWNLOAD_CLIENT.get(url);
    if has_range {
        builder = builder.header("Range", format!("bytes={}-{}", offset, offset + size - 1));
    }

    // 发送请求
    let res = builder
        .send()
        .await
        .with_http_context("create_http_stream", url);
    let response_received_time = Instant::now();

    let res = match res {
        Ok(r) => r,
        Err(e) => {
            // 创建错误insight并立即返回
            let insight = Arc::new(Mutex::new(InsightItem {
                url: crate::utils::url::sanitize_url_for_logging(url),
                ttfb: request_start_time.elapsed().as_millis() as u32,
                time: 0,
                size: 0,
                error: Some(crate::utils::code::insight_code(&e).to_string()),
                range: insight_range.clone(),
                mode: None,
            }));
            return Err(TACommandError::with_insight_handle(e, insight));
        }
    };

    // HTTP状态码检查
    let code = res.status();
    if (!has_range && code != 200) || (has_range && code != 206) {
        let insight = Arc::new(Mutex::new(InsightItem {
            url: crate::utils::url::sanitize_url_for_logging(url),
            ttfb: request_start_time.elapsed().as_millis() as u32,
            time: 0,
            size: 0,
            error: Some(crate::utils::code::SERVER_HTTP_ERROR.to_string()),
            range: insight_range.clone(),
            mode: None,
        }));
        let error = anyhow::Error::new(crate::dfs::HttpStatus::new(code.as_u16(), ""))
            .context(crate::utils::url::create_reqwest_context(
                "create_http_stream",
                url,
                "HTTP_STATUS_ERR",
            ));
        return Err(TACommandError::with_insight_handle(error, insight));
    }

    let content_length = res.content_length().unwrap_or(0);
    let stream = res.bytes_stream();
    let reader = tokio_util::io::StreamReader::new(stream.map_err(std::io::Error::other));

    // 创建NetworkInsightStream包装
    let insight_stream = NetworkInsightStream::new_with_detection(
        reader,
        crate::utils::url::sanitize_url_for_logging(url),
        insight_range,
        request_start_time,
        response_received_time,
        Some(content_length),
    );

    let insight_handle = insight_stream.get_insight_handle();

    if skip_decompress {
        Ok((Box::new(insight_stream), content_length, insight_handle))
    } else {
        // 在NetworkInsightStream外层套一个BufReader，然后再解压缩
        let buf_reader = BufReader::new(insight_stream);
        let decompressed = TokioZstdDecoder::new(buf_reader);
        // ✅ 关键：即使被解压缩包装，insight_handle仍然可用！
        Ok((Box::new(decompressed), content_length, insight_handle))
    }
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

pub async fn create_multi_http_stream(
    url: &str,
    range: &str,
) -> TAResult<(
    Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send + Unpin>,
    u64,
    String,
    Arc<Mutex<InsightItem>>,
)> {
    let request_start_time = Instant::now();
    let range_info = parse_range_string(range);

    let res = DOWNLOAD_CLIENT
        .get(url)
        .header("Range", format!("bytes={range}"))
        .send()
        .await
        .with_http_context("create_multi_http_stream", url);
    let response_received_time = Instant::now();

    let res = match res {
        Ok(r) => r,
        Err(e) => {
            let insight = Arc::new(Mutex::new(InsightItem {
                url: crate::utils::url::sanitize_url_for_logging(url),
                ttfb: request_start_time.elapsed().as_millis() as u32,
                time: 0,
                size: 0,
                error: Some(crate::utils::code::insight_code(&e).to_string()),
                range: range_info.clone(),
                mode: None,
            }));
            return Err(crate::utils::error::TACommandError::with_insight_handle(
                e, insight,
            ));
        }
    };

    // HTTP状态码检查
    let code = res.status();
    if code != 206 {
        let insight = Arc::new(Mutex::new(InsightItem {
            url: crate::utils::url::sanitize_url_for_logging(url),
            ttfb: request_start_time.elapsed().as_millis() as u32,
            time: 0,
            size: 0,
            error: Some(crate::utils::code::SERVER_HTTP_ERROR.to_string()),
            range: range_info,
            mode: None,
        }));
        let error = anyhow::Error::new(crate::dfs::HttpStatus::new(code.as_u16(), ""))
        .context(crate::utils::url::create_reqwest_context(
            "create_multi_http_stream",
            url,
            "HTTP_STATUS_ERR",
        ));
        return Err(crate::utils::error::TACommandError::with_insight_handle(
            error, insight,
        ));
    }

    let content_length = res.content_length().unwrap_or(0);
    let content_type = res
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    // 创建NetworkInsightStream包装HTTP响应流
    let insight_stream = NetworkInsightStream::new_with_detection(
        res.bytes_stream(),
        crate::utils::url::sanitize_url_for_logging(url),
        range_info,
        request_start_time,
        response_received_time,
        Some(content_length),
    );

    let insight_handle = insight_stream.get_insight_handle();

    Ok((
        Box::new(Box::pin(insight_stream)),
        content_length,
        content_type,
        insight_handle,
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
        let read = source.read(buffer).await.map_err(|e| {
            let anyhow_err = anyhow::Error::new(e);

            // 使用 Debug 格式获取完整错误链信息
            let full_error_debug = format!("{:?}", anyhow_err);

            // 检查完整错误链中是否包含我们的网络错误码
            if full_error_debug.contains("ERR_CONNECTION_")
                || full_error_debug.contains("ERR_STREAM_")
                || full_error_debug.contains("ERR_NETWORK_")
                || full_error_debug.contains("ERR_RESPONSE_BODY_")
                || full_error_debug.contains("ERR_DNS_")
                || full_error_debug.contains("ERR_TLS_")
                || full_error_debug.contains("ERR_REQUEST_")
                || full_error_debug.contains("ERR_DOWNLOAD_")
            {
                // 找到我们的网络错误标记，直接传播
                anyhow_err
            } else {
                // 没有找到网络错误标记，说明是真正的解压错误
                anyhow_err.context("DECOMPRESS_ERR")
            }
        })?;
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

/// Apply an hpatch diff stream to `old_path`, writing the result to
/// `out_path` (under the staging directory). No renames: the caller decides
/// what happens to the output.
pub async fn progressed_hpatch(
    old_path: &Path,
    diff: Box<dyn AsyncRead + Unpin + Send>,
    diff_size: usize,
    out_path: &Path,
    on_progress: Box<dyn Fn(usize) + Send>,
) -> Result<usize, anyhow::Error> {
    let mut downloaded = 0;
    let decoder = ReadWithCallback {
        reader: diff,
        callback: move |chunk| {
            downloaded += chunk;
            on_progress(downloaded);
        },
    };
    if let Some(parent) = out_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("CREATE_PARENT_DIR_ERR")?;
    }
    let old_size = old_path.metadata().context("GET_TARGET_SIZE_ERR")?;
    let out_file = std::fs::File::create(out_path).context("CREATE_NEW_TARGET_ERR")?;
    let old_file = std::fs::File::open(old_path).context("OPEN_TARGET_ERR")?;
    let diff_file = tokio_util::io::SyncIoBridge::new(decoder);
    let res = tokio::task::spawn_blocking(move || {
        hpatch_sys::safe_patch_single_stream(
            out_file,
            diff_file,
            diff_size,
            old_file,
            old_size.file_size() as usize,
        )
    })
    .await
    .context("RUN_HPATCH_ERR")?;
    if res != 1 {
        let _ = tokio::fs::remove_file(out_path).await;
        return Err(anyhow::Error::new(std::io::Error::other(format!(
            "Patch failed with code {res}"
        ))))
        .context("PATCH_FAILED_ERR");
    }
    Ok(diff_size)
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
        .attach(crate::utils::code::HASH_MISMATCH));
    }
    Ok(())
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
            progress_notify(move |v| reports_cb.lock().unwrap().push(v)),
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
            progress_notify(|_| {}),
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
            progress_notify(|_| {}),
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
            &["app.exe", "lib/a.dll", "lib/b.dll", "plugins/x/p.dll", "User/s.json"],
            &["User/s.json"],
        )
        .await;
        assert!(s.files.iter().any(|f| f.file_name.ends_with("s.json") && f.hash.is_empty()));
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
            .args(["/C", "mklink", "/J", &link.to_string_lossy(), &real.to_string_lossy()])
            .output()
            .unwrap();
        assert!(out.status.success());
        let s = scan(&dir, &["app.exe", "link/f.dll"], &[]).await;
        assert_eq!(s.reparse_dirs, vec!["link".to_string()]);
        assert_eq!(s.dirty_dirs, vec![String::new()]);
        assert!(s.files.iter().any(|f| f.file_name.ends_with("f.dll") && !f.hash.is_empty()));
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
