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
async fn open_part(
    offset: u64,
    len: u64,
) -> anyhow::Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>> {
    let url = resolve(offset, len).await?;
    match crate::fs::create_http_stream(
        &url,
        usize::try_from(offset)?,
        usize::try_from(len)?,
        true,
        None,
    )
    .await
    {
        Ok((reader, _, insight)) => {
            CURRENT.with(|ctx| ctx.insights.lock().unwrap().push(insight));
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

async fn prefetch(offset: u64, len: u64, space: OwnedSemaphorePermit) -> anyhow::Result<PartFile> {
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
        let input = open_part(offset, len).await;
        let mut reader = match input {
            Ok(reader) => reader,
            Err(err) if attempt == 0 && !CURRENT.with(|c| c.stop.is_cancelled()) => {
                tracing::debug!("range retry: {err:#}");
                continue;
            }
            Err(err) => return Err(err),
        };
        let mut received = 0;
        let mut buffer = vec![0; 64 * 1024];
        let outcome = loop {
            match reader.read(&mut buffer).await {
                Ok(0) => {
                    break if received == len {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("incomplete range body"))
                    }
                }
                Ok(n) => {
                    received += n as u64;
                    anyhow::ensure!(received <= len, "range body exceeds declared length");
                    file.write_all(&buffer[..n]).await.map_err(local_error)?;
                }
                Err(err) => break Err(err.into()),
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

pub fn sliced(parts: Vec<(u64, u64)>) -> Box<dyn tokio::io::AsyncRead + Unpin + Send> {
    let (tx, rx) = mpsc::channel::<std::io::Result<bytes::Bytes>>(2);
    let worker = spawn(async move {
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
                let mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> = if let Some(part) = &part {
                    Box::new(tokio::fs::File::open(part.file.path()).await?)
                } else {
                    match open_part(offset, len).await {
                        Ok(reader) => reader,
                        Err(_) if !ctx.stop.is_cancelled() => { retried = true; open_part(offset, len).await? },
                        Err(err) => return Err(err),
                    }
                };
                for ahead in index + 1..parts.len().min(index + 3) {
                    if pending.contains_key(&ahead) { continue; }
                    let (offset, len) = parts[ahead];
                    if let Ok(space) = ctx.session.disk.clone().try_acquire_many_owned(len.try_into()?) {
                        let waiting = queued[ahead].take();
                        pending.insert(ahead, spawn(async move {
                            let result = prefetch(offset, len, space).await;
                            drop(waiting);
                            result
                        }));
                    }
                }
                let mut received = 0;
                loop {
                    let mut buffer = vec![0; 64 * 1024];
                    let read = reader.read(&mut buffer).await;
                    if (read.is_err() || matches!(read, Ok(0))) && received == 0 && part.is_none() && !retried && !ctx.stop.is_cancelled() {
                        drop(reader);
                        retried = true;
                        reader = open_part(offset, len).await?;
                        continue;
                    }
                    let n = read?;
                    if n == 0 { break; }
                    received += n as u64;
                    anyhow::ensure!(received <= len, "range body exceeds declared length");
                    buffer.truncate(n);
                    tokio::select! {
                        _ = ctx.stop.cancelled() => return Err(crate::utils::code::Cancelled.into()),
                        sent = tx.send(Ok(buffer.into())) => { sent?; }
                    }
                }
                anyhow::ensure!(received == len, "incomplete range body");
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
    Box::new(tokio_util::io::StreamReader::new(Box::pin(stream)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        let retry_offset = parts[1].start;
        let calls = Arc::new(Mutex::new(HashMap::<u64, usize>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/package", listener.local_addr().unwrap());
        let server_calls = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let data = packed.clone();
                let calls = server_calls.clone();
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
                    let fail = {
                        let mut counts = calls.lock().unwrap();
                        let count = counts.entry(start as u64).or_default();
                        *count += 1;
                        start as u64 == retry_offset && *count == 1
                    };
                    let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len(), end - start + 1);
                    socket.write_all(header.as_bytes()).await.unwrap();
                    let end_sent = if fail { start + 31 } else { end };
                    socket.write_all(&data[start..=end_sent]).await.unwrap();
                });
            }
        });
        let id = uuid::Uuid::new_v4().to_string();
        let dir = std::env::temp_dir().join(format!("kachina-slices-{id}"));
        tokio::fs::create_dir_all(&dir).await.unwrap();
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
        let target = dir.join("output");
        let args = super::super::install_file::InstallFileArgs {
            mode: super::super::install_file::InstallFileMode::Direct(
                super::super::install_file::InstallFileSource::Sliced {
                    parts: parts.iter().map(|p| (p.start, p.len)).collect(),
                    skip_decompress: false,
                },
            ),
            target: target.to_string_lossy().into_owned(),
            output_size: data.len() as u64,
            old: None,
            md5: None,
            xxh: None,
            clear_installer_index_mark: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            RESOLVER.scope(
                tx,
                CURRENT.scope(
                    ctx.clone(),
                    super::super::network::measure(super::super::progress_noop(), |notify| {
                        super::super::install_file::ipc_install_file(args, notify)
                    }),
                ),
            ),
        )
        .await;
        finish(&ctx, result.as_ref().is_ok_and(|r| r.0.is_ok()))
            .await
            .unwrap();
        end(&id);
        server.abort();
        resolver.abort();
        let (result, stats) = result.unwrap();
        result.unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), data);
        assert_eq!(calls.lock().unwrap().get(&retry_offset), Some(&2));
        assert_eq!(stats.network.bytes, len + 32);
        assert_eq!(stats.network.active, 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        tokio::fs::remove_file(target).await.unwrap();
        tokio::fs::remove_dir(dir).await.unwrap();
    }
}
