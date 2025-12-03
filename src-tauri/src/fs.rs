use async_compression::tokio::bufread::ZstdDecoder as TokioZstdDecoder;
use bytes::Bytes;
use fmmap::tokio::AsyncMmapFileExt;
use futures::Stream;
use futures::{StreamExt, TryStreamExt};
use serde::Serialize;
use std::{
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
    dfs::InsightItem,
    installer::uninstall::DELETE_SELF_ON_EXIT_PATH,
    local::mmap,
    utils::{
        error::{TAResult, DOWNLOAD_STALLED, DOWNLOAD_TOO_SLOW},
        hash::run_hash,
        progressed_read::ReadWithCallback,
        url::HttpContextExt,
    },
    REQUEST_CLIENT,
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
                            insight.error = Some(classified_error.context.clone());
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
                        insight.error = Some(classified_error.context.clone());
                        insight.time = self.response_received_time.elapsed().as_millis() as u32;
                        insight.size = self.network_bytes.load(Ordering::Relaxed) as u32;
                    }

                    // 返回分类后的网络错误
                    Poll::Ready(Err(classified_error.into()))
                } else {
                    // 非网络错误：更新insight，然后保持原始错误传播
                    if let Ok(mut insight) = self.insight.try_lock() {
                        insight.error = Some(e.to_string());
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
                let io_error = std::io::Error::new(std::io::ErrorKind::Other, e.to_string());
                let error_type = ClassifiedNetworkError::classify_error(&io_error);
                let is_network_error = !matches!(error_type, NetworkErrorType::Other(_));

                // 更新insight
                if let Ok(mut insight) = self.insight.try_lock() {
                    if is_network_error {
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
                        insight.error = Some(context.to_string());
                    } else {
                        insight.error = Some(io_error.to_string());
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
            url: url.clone(),
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
            url,   // 保存URL
            range, // 保存Range
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
                                let base_error = std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    DOWNLOAD_TOO_SLOW,
                                );
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
        Box<dyn AsyncRead + Unpin + Send>,
        u64,
        Arc<Mutex<InsightItem>>,
    ),
    anyhow::Error,
> {
    let request_start_time = Instant::now();
    let has_range = size > 0;

    // 构建HTTP请求
    let mut builder = REQUEST_CLIENT.get(url);
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
                url: url.to_string(),
                ttfb: request_start_time.elapsed().as_millis() as u32,
                time: 0,
                size: 0,
                error: Some(format!("{:#}", e)),
                range: if has_range {
                    vec![(offset as u32, (offset + size - 1) as u32)]
                } else {
                    vec![]
                },
                mode: None,
            }));
            return Err(crate::utils::error::TACommandError::with_insight_handle(e, insight).error);
        }
    };

    // HTTP状态码检查
    let code = res.status();
    if (!has_range && code != 200) || (has_range && code != 206) {
        let insight = Arc::new(Mutex::new(InsightItem {
            url: url.to_string(),
            ttfb: request_start_time.elapsed().as_millis() as u32,
            time: 0,
            size: 0,
            error: Some(format!("HTTP status error: {}", code)),
            range: if has_range {
                vec![(offset as u32, (offset + size - 1) as u32)]
            } else {
                vec![]
            },
            mode: None,
        }));
        let error = anyhow::Error::new(std::io::Error::other(format!(
            "URL {} returned {}",
            crate::utils::url::sanitize_url_for_logging(url),
            code
        )))
        .context(crate::utils::url::create_reqwest_context(
            "create_http_stream",
            url,
            "HTTP_STATUS_ERR",
        ));
        return Err(crate::utils::error::TACommandError::with_insight_handle(error, insight).error);
    }

    let content_length = res.content_length().unwrap_or(0);
    let stream = res.bytes_stream();
    let reader = tokio_util::io::StreamReader::new(stream.map_err(std::io::Error::other));

    // 创建NetworkInsightStream包装
    let insight_stream = NetworkInsightStream::new_with_detection(
        reader,
        url.to_string(),
        if has_range {
            vec![(offset as u32, (offset + size - 1) as u32)]
        } else {
            vec![]
        },
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
) -> TAResult<(
    Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send + Unpin>,
    u64,
    String,
    Arc<Mutex<InsightItem>>,
)> {
    let request_start_time = Instant::now();
    let range_info = parse_range_string(range);

    let res = REQUEST_CLIENT
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
                url: url.to_string(),
                ttfb: request_start_time.elapsed().as_millis() as u32,
                time: 0,
                size: 0,
                error: Some(format!("{:#}", e)),
                range: range_info,
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
            url: url.to_string(),
            ttfb: request_start_time.elapsed().as_millis() as u32,
            time: 0,
            size: 0,
            error: Some(format!("HTTP status error: {}", code)),
            range: range_info,
            mode: None,
        }));
        let error = anyhow::Error::new(std::io::Error::other(format!(
            "URL {} returned {}",
            crate::utils::url::sanitize_url_for_logging(url),
            code
        )))
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
        url.to_string(),
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
    mut source: impl AsyncRead + std::marker::Unpin,
    mut target: impl AsyncWrite + std::marker::Unpin,
    on_progress: impl Fn(usize),
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
    let target_ori = target.clone();
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
        let target_path_ori = PathBuf::from(target_ori);
        // if old file is not self
        if exe_path != target_cl && exe_path != target_path_ori {
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
            tokio::fs::rename(new_target, target_path_ori)
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
