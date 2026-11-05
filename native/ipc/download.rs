//! 下载会话的请求预算、取消和范围解析。数据与暂存文件留在执行安装的进程中。

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, LazyLock, Mutex,
    },
};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub id: String,
    pub concurrency: usize,
    pub dl_dir: String,
    pub prefetch_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub session: String,
    pub large: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolve {
    pub id: String,
    pub offset: u64,
    pub len: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: String,
    pub url: Result<String, String>,
}

pub struct Session {
    pub cancel: CancellationToken,
    pub disk: Arc<Semaphore>,
    pub dl_dir: String,
    large: Arc<Semaphore>,
    small: Arc<Semaphore>,
    first: [AtomicUsize; 2],
    ready: tokio::sync::Notify,
}
#[derive(Clone)]
pub struct Context {
    pub session: Arc<Session>,
    pub large: bool,
    pub stop: CancellationToken,
    workers: Arc<Mutex<Vec<tokio::task::JoinHandle<anyhow::Result<()>>>>>,
    first: Arc<AtomicBool>,
    pub insights: Arc<Mutex<Vec<Arc<Mutex<crate::dfs::InsightItem>>>>>,
}

type Waiters = HashMap<String, oneshot::Sender<Result<String, String>>>;
static SESSIONS: LazyLock<Mutex<HashMap<String, Arc<Session>>>> = LazyLock::new(Default::default);
static WAITERS: LazyLock<Mutex<Waiters>> = LazyLock::new(Default::default);
tokio::task_local! {
    pub static CURRENT: Context;
    pub static RESOLVER: mpsc::UnboundedSender<(Resolve, bool)>;
}

pub fn limits(n: usize) -> (usize, usize) {
    let n = n.clamp(1, 16);
    if n == 1 {
        (1, 1)
    } else {
        let large = (5 * n / 16).clamp(1, n - 1);
        (large, n - large)
    }
}

pub fn begin(config: Config) {
    let (large, small) = limits(config.concurrency);
    let large = Arc::new(Semaphore::new(large));
    let small = if config.concurrency == 1 {
        large.clone()
    } else {
        Arc::new(Semaphore::new(small))
    };
    SESSIONS.lock().unwrap().insert(
        config.id,
        Arc::new(Session {
            cancel: CancellationToken::new(),
            disk: Arc::new(Semaphore::new(config.prefetch_bytes.min(256 * 1024 * 1024))),
            dl_dir: config.dl_dir,
            large,
            small,
            first: Default::default(),
            ready: Default::default(),
        }),
    );
}
pub fn cancel(id: &str) {
    if let Some(session) = SESSIONS.lock().unwrap().get(id) {
        session.cancel.cancel();
    }
}
pub fn end(id: &str) {
    SESSIONS.lock().unwrap().remove(id);
}
pub fn context(job: &Job, network: bool) -> anyhow::Result<Context> {
    let session = SESSIONS
        .lock()
        .unwrap()
        .get(&job.session)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("download session is closed"))?;
    if network {
        session.first[job.large as usize].fetch_add(1, Ordering::Relaxed);
    }
    Ok(Context {
        stop: session.cancel.child_token(),
        session,
        large: job.large,
        workers: Default::default(),
        first: Arc::new(AtomicBool::new(network)),
        insights: Default::default(),
    })
}
pub fn reply(reply: Reply) {
    if let Some(tx) = WAITERS.lock().unwrap().remove(&reply.id) {
        let _ = tx.send(reply.url);
    }
}
pub async fn resolve(offset: u64, len: u64) -> anyhow::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    WAITERS.lock().unwrap().insert(id.clone(), tx);
    struct Remove(String);
    impl Drop for Remove {
        fn drop(&mut self) {
            WAITERS.lock().unwrap().remove(&self.0);
        }
    }
    let _remove = Remove(id.clone());
    RESOLVER.try_with(|tx| tx.send((Resolve { id, offset, len }, false)))??;
    let cancel = CURRENT.with(|c| c.stop.clone());
    tokio::select! {
        result = rx => result?.map_err(anyhow::Error::msg),
        _ = cancel.cancelled() => Err(crate::utils::code::Cancelled.into()),
    }
}

pub async fn request() -> anyhow::Result<Option<OwnedSemaphorePermit>> {
    let Ok(ctx) = CURRENT.try_with(Clone::clone) else {
        return Ok(None);
    };
    let sem = if ctx.large {
        &ctx.session.large
    } else {
        &ctx.session.small
    };
    let first = ctx.first.swap(false, Ordering::Relaxed);
    if !first {
        loop {
            let ready = ctx.session.ready.notified();
            if ctx.session.first[ctx.large as usize].load(Ordering::Relaxed) == 0 {
                break;
            }
            tokio::select! { _ = ready => {}, _ = ctx.stop.cancelled() => return Err(crate::utils::code::Cancelled.into()) }
        }
    }
    let result = tokio::select! {
        _ = ctx.stop.cancelled() => Err(crate::utils::code::Cancelled.into()),
        permit = sem.clone().acquire_owned() => Ok(Some(permit?)),
    };
    if first {
        ctx.session.first[ctx.large as usize].fetch_sub(1, Ordering::Relaxed);
        ctx.session.ready.notify_waiters();
    }
    result
}

/// 在连接等待和读取期间响应取消；调用者仍须等待解码、补丁与写入任务结束。
pub async fn cancellable<F: std::future::Future>(future: F) -> anyhow::Result<F::Output> {
    let Ok(cancel) = CURRENT.try_with(|c| c.stop.clone()) else {
        return Ok(future.await);
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(crate::utils::code::Cancelled.into()),
        result = future => Ok(result),
    }
}

pub fn spawn<F: std::future::Future + Send + 'static>(
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F::Output: Send + 'static,
{
    let ctx = CURRENT.with(Clone::clone);
    let resolver = RESOLVER.with(Clone::clone);
    let meter = super::network::CURRENT.with(Clone::clone);
    tokio::spawn(CURRENT.scope(
        ctx,
        RESOLVER.scope(resolver, super::network::CURRENT.scope(meter, future)),
    ))
}

pub async fn finish(ctx: &Context, success: bool) -> anyhow::Result<()> {
    if !success {
        ctx.stop.cancel();
    }
    if ctx.first.swap(false, Ordering::Relaxed) {
        ctx.session.first[ctx.large as usize].fetch_sub(1, Ordering::Relaxed);
        ctx.session.ready.notify_waiters();
    }
    let workers = std::mem::take(&mut *ctx.workers.lock().unwrap());
    let mut error = None;
    for worker in workers {
        if let Err(err) = worker
            .await
            .map_err(anyhow::Error::from)
            .and_then(|result| result)
        {
            if error.is_none() {
                error = Some(err);
            }
        }
    }
    ctx.stop.cancel();
    if let Some(error) = error {
        Err(error)
    } else {
        Ok(())
    }
}

struct PartFile {
    file: super::install_file::TemporaryFile,
    _space: OwnedSemaphorePermit,
}

#[derive(Debug, Clone)]
pub(super) struct SharedError(pub(super) Arc<anyhow::Error>);
impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}
/// Insight of the request that delivered each part of a sliced read, by part
/// index; a part read again after a failure keeps the later request's.
pub(super) type PartInsights = Arc<Mutex<Vec<Option<Arc<Mutex<crate::dfs::InsightItem>>>>>>;

type PartReader = Box<dyn tokio::io::AsyncRead + Unpin + Send>;

/// Opens one part and records its request in `insights`. The stream fails a
/// body that is not `len` bytes, so a read that ends cleanly has all of them.
async fn open_part(
    offset: u64,
    len: u64,
    insights: &PartInsights,
    index: usize,
) -> anyhow::Result<PartReader> {
    match crate::fs::create_resolved_stream(usize::try_from(offset)?, usize::try_from(len)?, None)
        .await
    {
        Ok((reader, _, insight)) => {
            CURRENT.with(|ctx| ctx.insights.lock().unwrap().push(insight.clone()));
            insights.lock().unwrap()[index] = Some(insight);
            Ok(reader)
        }
        Err(err) => {
            if let Some(insight) = err.insight {
                CURRENT.with(|ctx| {
                    ctx.insights
                        .lock()
                        .unwrap()
                        .push(Arc::new(Mutex::new(insight)))
                });
            }
            Err(err.error)
        }
    }
}

fn local_error(error: std::io::Error) -> anyhow::Error {
    use crate::utils::code::Attach;
    let code = crate::utils::code::code_for_local_io(&error);
    anyhow::Error::new(error).attach(code)
}

async fn prefetch(
    (offset, len): (u64, u64),
    space: OwnedSemaphorePermit,
    insights: PartInsights,
    index: usize,
) -> anyhow::Result<PartFile> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = CURRENT.with(|c| c.session.dl_dir.clone());
    let part = PartFile {
        file: super::install_file::TemporaryFile(
            std::path::Path::new(&dir).join(format!("{}.part", uuid::Uuid::new_v4())),
        ),
        _space: space,
    };
    for attempt in 0..2 {
        let mut file = tokio::fs::File::create(part.file.path())
            .await
            .map_err(local_error)?;
        let input = open_part(offset, len, &insights, index).await;
        let mut reader = match input {
            Ok(reader) => reader,
            Err(err) if attempt == 0 && !CURRENT.with(|c| c.stop.is_cancelled()) => {
                tracing::debug!("range retry: {err:#}");
                continue;
            }
            Err(err) => return Err(err),
        };
        let mut buffer = vec![0; 64 * 1024];
        let outcome = loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break Ok(()),
                Ok(n) => file.write_all(&buffer[..n]).await.map_err(local_error)?,
                Err(err) => break Err(anyhow::Error::from(err)),
            }
        };
        drop(file);
        match outcome {
            Ok(()) => return Ok(part),
            Err(err) if attempt == 0 && !CURRENT.with(|c| c.stop.is_cancelled()) => {
                tracing::debug!("range retry: {err:#}")
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!()
}

pub fn sliced(parts: Vec<(u64, u64)>) -> (PartReader, PartInsights) {
    let (tx, rx) = mpsc::channel::<std::io::Result<bytes::Bytes>>(2);
    let insights: PartInsights = Arc::new(Mutex::new(vec![None; parts.len()]));
    let recorded = insights.clone();
    let worker = spawn(async move {
        let insights = recorded;
        use tokio::io::AsyncReadExt;
        let ctx = CURRENT.with(Clone::clone);
        // 未开始和正在重试的切片也属于网络等待，磁盘回读不计入网络活动。
        let mut queued: Vec<_> = parts
            .iter()
            .map(|_| Some(super::network::Reading::begin()))
            .collect();
        let mut pending: HashMap<usize, tokio::task::JoinHandle<anyhow::Result<PartFile>>> =
            HashMap::new();
        let result: anyhow::Result<()> = async {
            for index in 0..parts.len() {
                let (offset, len) = parts[index];
                let part = match pending.remove(&index) { Some(task) => Some(task.await??), None => None };
                let network_pending = queued[index].take();
                let mut retried = false;
                let mut reader: PartReader = if let Some(part) = &part {
                    Box::new(tokio::fs::File::open(part.file.path()).await?)
                } else {
                    match open_part(offset, len, &insights, index).await {
                        Ok(reader) => reader,
                        Err(_) if !ctx.stop.is_cancelled() => { retried = true; open_part(offset, len, &insights, index).await? },
                        Err(err) => return Err(err),
                    }
                };
                for ahead in index + 1..parts.len().min(index + 3) {
                    if pending.contains_key(&ahead) { continue; }
                    let (_, len) = parts[ahead];
                    if let Ok(space) = ctx.session.disk.clone().try_acquire_many_owned(len.try_into()?) {
                        let waiting = queued[ahead].take();
                        let (part, insights) = (parts[ahead], insights.clone());
                        pending.insert(ahead, spawn(async move {
                            let result = prefetch(part, space, insights, ahead).await;
                            drop(waiting);
                            result
                        }));
                    }
                }
                let mut received = 0;
                // 读进同一块 BytesMut 的空闲容量再切走：不逐次清零 64 KiB，零碎的小读共享一块
                // 分配，接收方释放先前的块后 reserve 可以回收原分配。
                let mut buffer = bytes::BytesMut::new();
                loop {
                    buffer.reserve(64 * 1024);
                    let read = reader.read_buf(&mut buffer).await;
                    if read.is_err() && received == 0 && part.is_none() && !retried && !ctx.stop.is_cancelled() {
                        drop(reader);
                        retried = true;
                        reader = open_part(offset, len, &insights, index).await?;
                        continue;
                    }
                    let n = read?;
                    if n == 0 { break; }
                    received += n as u64;
                    let chunk = buffer.split().freeze();
                    tokio::select! {
                        _ = ctx.stop.cancelled() => return Err(crate::utils::code::Cancelled.into()),
                        sent = tx.send(Ok(chunk)) => { sent?; }
                    }
                }
                drop(reader);
                drop(network_pending);
                drop(part);
            }
            Ok(())
        }.await;
        if result.is_err() {
            ctx.stop.cancel();
        }
        for (_, task) in pending {
            let _ = task.await;
        }
        match result {
            Ok(()) => Ok(()),
            Err(err) => {
                let error = SharedError(Arc::new(err));
                let _ = tx.send(Err(std::io::Error::other(error.clone()))).await;
                Err(error.into())
            }
        }
    });
    CURRENT.with(|c| c.workers.lock().unwrap().push(worker));
    let stream = futures::stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    });
    (
        Box::new(tokio_util::io::StreamReader::new(Box::pin(stream))),
        insights,
    )
}

#[cfg(test)]
mod tests {
    use super::super::install_file::{
        ipc_install_file, InstallFileArgs, InstallFileMode, InstallFileSource, InstallResult,
    };
    use super::*;
    use crate::utils::error::TAResult;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type Requests = Arc<Mutex<HashMap<u64, usize>>>;

    /// Serves `data` by range on 127.0.0.1 and counts the requests per range
    /// start. `last(start, end, nth)` is the last byte sent for the `nth`
    /// request (from 1) of a range; the headers always announce the whole range.
    async fn range_server(
        data: Arc<Vec<u8>>,
        last: impl Fn(usize, usize, usize) -> usize + Send + Sync + 'static,
    ) -> (String, Requests, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/package", listener.local_addr().unwrap());
        let requests = Requests::default();
        let (counts, last) = (requests.clone(), Arc::new(last));
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (data, counts, last) = (data.clone(), counts.clone(), last.clone());
                tokio::spawn(async move {
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        header.push(socket.read_u8().await.unwrap());
                    }
                    let header = String::from_utf8(header).unwrap().to_lowercase();
                    let range = header
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .unwrap();
                    let (start, end) = range.split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end: usize = end.parse().unwrap();
                    let nth = {
                        let mut counts = counts.lock().unwrap();
                        let count = counts.entry(start as u64).or_default();
                        *count += 1;
                        *count
                    };
                    let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len(), end - start + 1);
                    // the client hangs up early once a download failed
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&data[start..=last(start, end, nth)]).await;
                });
            }
        });
        (url, requests, server)
    }

    fn sliced_args(parts: &[(u64, u64)], target: &std::path::Path, size: usize) -> InstallFileArgs {
        InstallFileArgs {
            mode: InstallFileMode::Direct(InstallFileSource::Sliced {
                parts: parts.to_vec(),
                skip_decompress: false,
            }),
            target: target.to_string_lossy().into_owned(),
            output_size: size as u64,
            old: None,
            md5: None,
            xxh: None,
            clear_installer_index_mark: None,
        }
    }

    struct JobRun {
        result: TAResult<InstallResult>,
        stats: super::super::network::Stats,
        finished: anyhow::Result<()>,
        /// Every request the job made.
        insights: Vec<crate::dfs::InsightItem>,
    }

    /// Runs `args` as the only job of a fresh download session whose URLs all
    /// resolve to `url`.
    async fn in_job(url: String, dir: &std::path::Path, args: InstallFileArgs) -> JobRun {
        let id = uuid::Uuid::new_v4().to_string();
        begin(Config {
            id: id.clone(),
            concurrency: 16,
            dl_dir: dir.to_string_lossy().into_owned(),
            prefetch_bytes: 256 * 1024 * 1024,
        });
        let ctx = context(
            &Job {
                session: id.clone(),
                large: true,
            },
            true,
        )
        .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<(Resolve, bool)>();
        let resolver = tokio::spawn(async move {
            while let Some((query, _)) = rx.recv().await {
                reply(Reply {
                    id: query.id,
                    url: Ok(url.clone()),
                });
            }
        });
        let run = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            RESOLVER.scope(
                tx,
                CURRENT.scope(
                    ctx.clone(),
                    super::super::network::measure(super::super::progress_noop(), |notify| {
                        ipc_install_file(args, notify)
                    }),
                ),
            ),
        )
        .await;
        let finished = finish(&ctx, run.as_ref().is_ok_and(|r| r.0.is_ok())).await;
        end(&id);
        resolver.abort();
        let (result, stats) = run.expect("job timed out");
        let insights = ctx
            .insights
            .lock()
            .unwrap()
            .iter()
            .map(|i| i.lock().unwrap().clone())
            .collect();
        JobRun {
            result,
            stats,
            finished,
            insights,
        }
    }

    // 合成不可压缩数据使分片跨越 Zstd 帧内部；中间片断流一次，覆盖落盘重试。
    #[tokio::test]
    async fn sliced_zstd_retries_range_and_cleans_prefetch() {
        let mut seed = 13u64;
        let data: Vec<u8> = (0..300_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        let packed = Arc::new(zstd::encode_all(data.as_slice(), 1).unwrap());
        let len = packed.len() as u64;
        let parts = crate::session::download_plan::Policy {
            upper: len / 4,
            maximum: len / 3,
            ..Default::default()
        }
        .split(crate::session::download_plan::Range { start: 0, len });
        let retry_offset = parts[1].start as usize;
        let parts: Vec<_> = parts.iter().map(|p| (p.start, p.len)).collect();
        let (url, requests, server) = range_server(packed, move |start, end, nth| {
            if start == retry_offset && nth == 1 {
                start + 31
            } else {
                end
            }
        })
        .await;
        let dir = std::env::temp_dir().join(format!("kachina-slices-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let target = dir.join("output");
        let run = in_job(url, &dir, sliced_args(&parts, &target, data.len())).await;
        server.abort();
        run.finished.unwrap();
        run.result.unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), data);
        assert_eq!(
            requests.lock().unwrap().get(&(retry_offset as u64)),
            Some(&2)
        );
        assert_eq!(run.stats.network.bytes, len + 32);
        assert_eq!(run.stats.network.active, 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        tokio::fs::remove_file(target).await.unwrap();
        tokio::fs::remove_dir(dir).await.unwrap();
    }

    /// Offsets of the block headers in a zstd frame without a dictionary.
    fn block_headers(frame: &[u8]) -> Vec<usize> {
        let descriptor = frame[4];
        let single_segment = descriptor & 0x20 != 0;
        let content_size = match descriptor >> 6 {
            0 => usize::from(single_segment),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let mut at = 5 + usize::from(!single_segment) + content_size;
        let mut headers = Vec::new();
        loop {
            let header = u32::from_le_bytes([frame[at], frame[at + 1], frame[at + 2], 0]);
            headers.push(at);
            let rle = (header >> 1) & 3 == 1;
            at += 3 + if rle { 1 } else { (header >> 3) as usize };
            if header & 1 == 1 {
                return headers;
            }
        }
    }

    /// Data the decoder rejects is recorded on the part that delivered it; a
    /// hash that does not match, on every part.
    #[tokio::test]
    async fn bad_sliced_data_is_recorded_on_the_part_that_sent_it() {
        use crate::utils::code::{extract, Extracted, HASH_MISMATCH};
        let mut seed = 29u64;
        let data: Vec<u8> = (0..6 << 20)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed & 15) as u8
            })
            .collect();
        let packed = zstd::encode_all(data.as_slice(), 1).unwrap();
        let len = packed.len() as u64;
        let third = len / 3;
        let parts = [(0, third), (third, third), (2 * third, len - third * 2)];
        let reach = super::super::install_file::UNDECODABLE_REACH;
        // a block header in the middle part, far enough from both of its ends
        let header = block_headers(&packed)
            .into_iter()
            .find(|&at| at as u64 > third + reach && (at as u64) < 2 * third - reach)
            .unwrap();
        let mut broken = packed.clone();
        broken[header] |= 0b110; // reserved block type
        let dir = std::env::temp_dir().join(format!("kachina-blame-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let target = dir.join("output");
        let errors = |run: &JobRun| -> Vec<Option<String>> {
            parts
                .iter()
                .map(|&(start, _)| {
                    run.insights
                        .iter()
                        .find(|i| i.range.first().is_some_and(|r| u64::from(r.0) == start))
                        .and_then(|i| i.error.clone())
                })
                .collect()
        };
        let hash_mismatch = |run: &JobRun| {
            matches!(
                extract(&run.result.as_ref().unwrap_err().error),
                Extracted::Coded(c) if c.code == HASH_MISMATCH
            )
        };

        let (url, _, server) = range_server(Arc::new(broken), |_, end, _| end).await;
        let run = in_job(url, &dir, sliced_args(&parts, &target, data.len())).await;
        server.abort();
        assert!(hash_mismatch(&run));
        let blamed = errors(&run);
        assert_eq!(blamed[0], None);
        assert_eq!(blamed[1].as_deref(), Some(HASH_MISMATCH));
        assert_ne!(blamed[2].as_deref(), Some(HASH_MISMATCH));

        let (url, _, server) = range_server(Arc::new(packed), |_, end, _| end).await;
        let args = InstallFileArgs {
            xxh: Some("0".into()),
            ..sliced_args(&parts, &target, data.len())
        };
        let run = in_job(url, &dir, args).await;
        server.abort();
        assert!(hash_mismatch(&run));
        assert!(errors(&run)
            .iter()
            .all(|e| e.as_deref() == Some(HASH_MISMATCH)));
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn cancel_while_waiting_for_slot_requests_no_url() {
        let id = uuid::Uuid::new_v4().to_string();
        begin(Config {
            id: id.clone(),
            concurrency: 1,
            dl_dir: String::new(),
            prefetch_bytes: 0,
        });
        let ctx = context(
            &Job {
                session: id.clone(),
                large: true,
            },
            true,
        )
        .unwrap();
        let held = ctx.session.large.clone().acquire_owned().await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<(Resolve, bool)>();
        let open = tokio::spawn(RESOLVER.scope(
            tx,
            CURRENT.scope(ctx.clone(), async {
                crate::fs::create_resolved_stream(0, 16, None)
                    .await
                    .map(|_| ())
                    .map_err(|err| err.error)
            }),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel(&id);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), open)
            .await
            .unwrap()
            .unwrap();
        drop(held);
        end(&id);
        assert!(result
            .unwrap_err()
            .downcast_ref::<crate::utils::code::Cancelled>()
            .is_some());
        assert!(rx.try_recv().is_err());
    }
}
