use super::operation::run_opr;
use super::operation::IpcOperation;
use super::{
    decode_frame, encode_frame, progress_notify, read_frame, IpcError, IpcResult, PipeMsg,
    Progress, ProgressNotify,
};
use crate::utils::acl::create_security_attributes;
use crate::utils::error::TAResult;
use crate::utils::uac::check_elevated;
use crate::utils::uac::run_elevated;
use crate::utils::uac::SendableHandle;
use anyhow::Context;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::oneshot;
use tokio::time;
use windows::Win32::Foundation::ERROR_PIPE_BUSY;

const PIPE_BUFFER_SIZE: usize = 64 * 1024;

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct IpcInner {
    op: IpcOperation,
    id: String,
}

/// One waiter per in-flight request, keyed by request id. Results are
/// delivered here; progress goes over a broadcast that may lag without
/// consequence.
pub type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<IpcResult, IpcError>>>>>;

pub fn disconnect_error() -> IpcError {
    IpcError {
        message: "Elevate process disconnected: PIPE_DISCONNECT_ERR".into(),
        code: None,
        subject: None,
        sid: None,
        cancelled: false,
        insight: None,
    }
}

#[derive(Debug)]
pub struct ManagedElevate {
    process: tokio::sync::RwLock<Option<SendableHandle>>,
    started: AtomicBool,
    mpsc_tx: tokio::sync::mpsc::Sender<IpcInner>,
    mpsc_rx: tokio::sync::RwLock<Option<tokio::sync::mpsc::Receiver<IpcInner>>>,
    progress_tx: tokio::sync::broadcast::Sender<(String, Progress)>,
    pending: Pending,
    pipe_id: String,
    already_elevated: bool,
}

impl Default for ManagedElevate {
    fn default() -> Self {
        Self::new()
    }
}

impl ManagedElevate {
    pub fn new() -> Self {
        let (progress_tx, _progress_rx) = tokio::sync::broadcast::channel(256);
        let (mpsc_tx, mpsc_rx) = tokio::sync::mpsc::channel(100);
        let pipe_id = format!("{}", uuid::Uuid::new_v4());
        Self {
            process: tokio::sync::RwLock::new(None),
            started: AtomicBool::new(false),
            progress_tx,
            mpsc_tx,
            mpsc_rx: tokio::sync::RwLock::new(Some(mpsc_rx)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            pipe_id,
            already_elevated: check_elevated().unwrap_or(false),
        }
    }

    /// A manager whose "elevated side" is whatever the test drives through
    /// the returned request receiver, replying via `pending` / `progress_tx`.
    #[cfg(test)]
    fn detached() -> (
        Self,
        tokio::sync::mpsc::Receiver<IpcInner>,
        Pending,
        tokio::sync::broadcast::Sender<(String, Progress)>,
    ) {
        let mut me = Self::new();
        me.already_elevated = false;
        me.started.store(true, Ordering::SeqCst);
        let rx = me.mpsc_rx.get_mut().take().unwrap();
        let pending = me.pending.clone();
        let progress_tx = me.progress_tx.clone();
        (me, rx, pending, progress_tx)
    }
    pub fn create_pipe(name: &str) -> anyhow::Result<NamedPipeServer> {
        let mut attr = create_security_attributes();
        Ok(unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, &mut attr as *mut _ as *mut c_void)
        }?)
    }
    pub async fn start(&self) -> anyhow::Result<()> {
        let mut process = self.process.write().await;
        if process.is_none() {
            let name = self.pipe_id.clone();
            let name = format!(r"\\.\pipe\Kachina-Elevate-{name}");

            // 先创建pipe服务器
            let mut server = Self::create_pipe(&name).context("ELEVATE_ERR")?;
            tracing::info!("Pipe server created at {:?}", name);

            // pipe服务器创建成功后再启动UAC进程
            let command = run_elevated(
                std::env::current_exe().unwrap(),
                format!("headless-uac {}", self.pipe_id),
            )
            .context("ELEVATE_ERR")?;
            process.replace(command);
            tracing::info!("UAC process started, waiting for pipe connection...");

            let rx = self.mpsc_rx.write().await.take().unwrap();
            if !wait_conn(&mut server).await {
                return Err(anyhow::anyhow!("Failed to wait for connection").context("ELEVATE_ERR"));
            }
            handle_pipe(server, self.progress_tx.clone(), self.pending.clone(), rx).await;
            self.started.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    pub async fn run(
        &self,
        ipc: IpcOperation,
        elevate: bool,
        on_progress: ProgressNotify,
    ) -> TAResult<IpcResult> {
        if !elevate || self.already_elevated {
            return run_opr(ipc, on_progress).await;
        }
        if !self.started.load(Ordering::SeqCst) {
            tracing::info!("Elevate process not started, starting...");
            self.start().await?;
            tracing::info!("Elevate process started");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, mut result_rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        let mut progress_rx = self.progress_tx.subscribe();
        // 管道任务已退出时接收端不存在，此处不返回就会永远等不到回包
        if self
            .mpsc_tx
            .send(IpcInner {
                op: ipc,
                id: id.clone(),
            })
            .await
            .is_err()
        {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(anyhow::anyhow!("Elevate process pipe is closed")
                .context("IPC_ERR")
                .into());
        }
        loop {
            tokio::select! {
                res = &mut result_rx => {
                    return match res {
                        Ok(Ok(data)) => Ok(data),
                        Ok(Err(error)) => Err(error.into_ta()),
                        Err(_) => Err(anyhow::anyhow!("Failed to receive response from elevate process")
                            .context("IPC_ERR")
                            .into()),
                    };
                }
                p = progress_rx.recv() => match p {
                    Ok((msgid, data)) if msgid == id => on_progress(data),
                    Ok(_) => {}
                    // 进度洪水只丢进度，不丢结果
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!("progress broadcast lagged by {n}");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // keep waiting for the result alone
                        let res = (&mut result_rx).await;
                        return match res {
                            Ok(Ok(data)) => Ok(data),
                            Ok(Err(error)) => Err(error.into_ta()),
                            Err(_) => Err(anyhow::anyhow!("Failed to receive response from elevate process")
                                .context("IPC_ERR")
                                .into()),
                        };
                    }
                },
            }
        }
    }

    /// Block until every in-flight elevate request has a result. Pending
    /// entries stay until the elevate side replies, even if the local `run`
    /// future was dropped (phase-one cancel).
    pub async fn wait_idle(&self) {
        loop {
            let empty = self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty();
            if empty {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn fail_all_pending(pending: &Pending) {
    let waiters: Vec<_> = pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .collect();
    for (_, tx) in waiters {
        let _ = tx.send(Err(disconnect_error()));
    }
}

pub async fn wait_conn(server: &mut NamedPipeServer) -> bool {
    if let Err(err) = server.connect().await {
        tracing::warn!("Failed to accept pipe connection: {:?}", err);
        return false;
    }
    tracing::info!("Client connected to pipe");
    true
}
/// 读写各一个任务：`read_frame` 不可取消，不能放进与写端共用的 `select!`。
/// 任一端结束（对端关闭、帧错位、读写错误、`ManagedElevate` 被丢弃）就停掉另一端，
/// 让两个半边一起释放、管道真正关闭，并让所有等待中的 `run` 收到断连错误。
pub async fn handle_pipe(
    server: NamedPipeServer,
    progress_tx: tokio::sync::broadcast::Sender<(String, Progress)>,
    pending: Pending,
    mut rx: tokio::sync::mpsc::Receiver<IpcInner>,
) {
    let (serverrx, mut servertx) = tokio::io::split(server);
    let mut writer = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            match encode_frame(&v) {
                Ok(frame) => {
                    if let Err(err) = servertx.write_all(&frame).await {
                        tracing::warn!("Failed to write to pipe: {:?}", err);
                        break;
                    }
                }
                Err(err) => tracing::warn!("Failed to serialize message: {:?}", err),
            }
        }
    });
    let reader_pending = pending.clone();
    let mut reader = tokio::spawn(async move {
        let mut serverrx = tokio::io::BufReader::with_capacity(PIPE_BUFFER_SIZE, serverrx);
        loop {
            let msg = match read_frame(&mut serverrx).await {
                Ok(Some(bytes)) => decode_frame::<PipeMsg>(&bytes),
                Ok(None) => {
                    tracing::warn!("Elevate process closed the pipe");
                    break;
                }
                Err(err) => Err(err),
            };
            match msg {
                Ok(PipeMsg::Envelope(envelope)) => {
                    crate::utils::sentry::forward_raw_envelope(envelope);
                }
                Ok(PipeMsg::Breadcrumb(crumb)) => {
                    crate::utils::sentry::add_breadcrumb_json(&crumb);
                }
                Ok(PipeMsg::Progress(id, p)) => {
                    let _ = progress_tx.send((id, p));
                }
                Ok(PipeMsg::Ok(id, data)) => {
                    if let Some(tx) = reader_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                    {
                        let _ = tx.send(Ok(data));
                    }
                }
                Ok(PipeMsg::Err(id, err)) => {
                    if let Some(tx) = reader_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                    {
                        let _ = tx.send(Err(err));
                    }
                }
                Ok(PipeMsg::Disconnect(_)) => {}
                Err(err) => {
                    tracing::error!("Failed to read from pipe: {:?}", err);
                    break;
                }
            }
        }
    });
    tokio::spawn(async move {
        tokio::select! {
            _ = &mut reader => writer.abort(),
            _ = &mut writer => reader.abort(),
        }
        fail_all_pending(&pending);
    });
}

pub async fn uac_ipc_main(args: crate::cli::arg::UacArgs) {
    // the elevated side renames and removes install directories too; its cwd
    // must not pin one (cwd is not inherited reliably across the UAC launch)
    let _ = crate::fs::staging::enter_neutral_cwd();
    let pipe_name = format!(r"\\.\pipe\Kachina-Elevate-{}", args.pipe_id);
    let mut try_times = 0;
    let client = loop {
        let pipe = ClientOptions::new().open(pipe_name.clone());
        if let Ok(pipe) = pipe {
            break Ok(pipe);
        }
        let err = pipe.err().unwrap();
        if err.raw_os_error() != Some(ERROR_PIPE_BUSY.0 as i32) {
            break Err(err);
        }
        time::sleep(Duration::from_millis(50)).await;
        try_times += 1;
        if try_times > 10 {
            break Err(std::io::Error::from_raw_os_error(ERROR_PIPE_BUSY.0 as i32));
        }
    };

    if let Err(err) = client {
        let detail = format!("{err:?}");
        crate::utils::taskdialog::show_error(
            crate::utils::taskdialog::ErrorDialog {
                detail: Some(&detail),
                ..crate::utils::taskdialog::ErrorDialog::code(crate::utils::code::ELEVATE_FAILED)
            },
            windows::Win32::Foundation::HWND::default(),
        );
        return;
    }
    let client = client.unwrap();
    let (clientrx, mut clienttx) = tokio::io::split(client);
    let mut clientrx = tokio::io::BufReader::with_capacity(PIPE_BUFFER_SIZE, clientrx);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<PipeMsg>(500);
    let mut sentry_rx = crate::utils::sentry::PIPE_OUTBOX.rx.write().await;

    // 创建一个取消通知器
    let (cancel_tx, cancel_rx) = tokio::sync::broadcast::channel(1);

    // 第一个线程：处理客户端读取。取消只在帧边界之外触发，帧内被打断时本来就要退出。
    let read_handle = {
        let tx = tx.clone();
        let cancel_tx = cancel_tx.clone();
        let mut cancel_rx = cancel_rx.resubscribe();
        tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    _ = cancel_rx.recv() => {
                        tracing::info!("Read thread cancelled");
                        break;
                    }
                    v = read_frame(&mut clientrx) => v,
                };
                let bytes = match frame {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => {
                        tracing::warn!("Client: disconnected");
                        let _ = cancel_tx.send(());
                        break;
                    }
                    Err(err) => {
                        tracing::warn!("Client: Failed to read from pipe: {:?}", err);
                        let _ = cancel_tx.send(());
                        break;
                    }
                };
                let res = match decode_frame::<IpcInner>(&bytes) {
                    Ok(res) => res,
                    Err(err) => {
                        tracing::error!("Client: Failed to decode frame, closing: {:?}", err);
                        let _ = cancel_tx.send(());
                        break;
                    }
                };
                let tx = tx.clone();
                let id = res.id.clone();
                tokio::spawn(async move {
                    let tx2 = tx.clone();
                    let res = run_opr(
                        res.op,
                        progress_notify(move |opr| {
                            let id = res.id.clone();
                            let tx_clone = tx.clone();
                            tokio::spawn(async move {
                                let _ = tx_clone.send(PipeMsg::Progress(id, opr)).await;
                            });
                        }),
                    )
                    .await;
                    if let Err(err) = res.as_ref() {
                        tracing::error!("Client: Operation failed: {:?}", err);
                    }
                    let msg = match res {
                        Ok(data) => PipeMsg::Ok(id, data),
                        Err(err) => PipeMsg::Err(id, IpcError::from_ta(&err)),
                    };
                    let _ = tx2.send(msg).await;
                });
            }
        })
    };

    // 第二个线程：处理发送和sentry消息
    let write_handle = {
        let cancel_tx = cancel_tx.clone();
        let mut cancel_rx = cancel_rx.resubscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_rx.recv() => {
                        tracing::info!("Write thread cancelled");
                        break;
                    }
                    v = rx.recv() => {
                        if let Some(v) = v {
                            match encode_frame(&v) {
                                Ok(frame) => {
                                    if let Err(err) = clienttx.write_all(&frame).await {
                                        tracing::warn!("Client: Failed to write to pipe: {:?}", err);
                                        let _ = cancel_tx.send(());
                                        break;
                                    }
                                }
                                Err(err) => {
                                    tracing::warn!("Client: Failed to serialize message: {:?}", err);
                                }
                            }
                        } else {
                            tracing::warn!("Client: Failed to receive message from channel");
                            let _ = cancel_tx.send(());
                            break;
                        }
                    }
                    v = sentry_rx.recv() => {
                        if let Some(msg) = v {
                            let _ = tx.send(msg).await;
                        }
                    }
                }
            }
        })
    };

    // 等待任一线程结束
    tokio::select! {
        _ = read_handle => {
            tracing::info!("Read thread finished");
        }
        _ = write_handle => {
            tracing::info!("Write thread finished");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::arg::UacArgs;

    fn register(pending: &Pending, id: &str) -> oneshot::Receiver<Result<IpcResult, IpcError>> {
        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(id.to_string(), tx);
        rx
    }

    /// 真实命名管道上的往返：服务端 `handle_pipe` 对接客户端 `uac_ipc_main`，
    /// 覆盖 Ok / Err 两种回包与丢弃 `ManagedElevate` 后的断连收尾。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pipe_roundtrip_and_disconnect() {
        let pipe_id = uuid::Uuid::new_v4().to_string();
        let mut server =
            ManagedElevate::create_pipe(&format!(r"\\.\pipe\Kachina-Elevate-{pipe_id}")).unwrap();
        let client = tokio::spawn(uac_ipc_main(UacArgs { pipe_id }));
        server.connect().await.unwrap();

        let (progress_tx, _progress_rx) = tokio::sync::broadcast::channel(16);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (mpsc_tx, mpsc_rx) = tokio::sync::mpsc::channel(16);
        handle_pipe(server, progress_tx, pending.clone(), mpsc_rx).await;

        async fn wait(
            rx: oneshot::Receiver<Result<IpcResult, IpcError>>,
        ) -> Result<Result<IpcResult, IpcError>, oneshot::error::RecvError> {
            time::timeout(Duration::from_secs(5), rx)
                .await
                .expect("pipe reply timeout")
        }

        let ping = register(&pending, "ping");
        mpsc_tx
            .send(IpcInner {
                op: IpcOperation::Ping,
                id: "ping".into(),
            })
            .await
            .unwrap();
        assert!(matches!(wait(ping).await, Ok(Ok(IpcResult::Ping))));

        let kill = register(&pending, "kill");
        mpsc_tx
            .send(IpcInner {
                op: IpcOperation::KillProcess(u32::MAX),
                id: "kill".into(),
            })
            .await
            .unwrap();
        let Ok(Err(err)) = wait(kill).await else {
            panic!("expected Err");
        };
        assert!(err.message.contains("OPEN_PROCESS_ERR"), "{}", err.message);

        let orphan = register(&pending, "orphan");
        drop(mpsc_tx);
        let Ok(Err(err)) = wait(orphan).await else {
            panic!("orphan waiter should get the disconnect error");
        };
        assert!(err.message.contains("disconnected"), "{}", err.message);
        assert!(pending.lock().unwrap().is_empty());
        time::timeout(Duration::from_secs(5), client)
            .await
            .expect("client did not exit after server closed")
            .unwrap();
    }

    /// 1000 条进度先于结果到达、超出广播容量：`run` 仍拿到 `Ok`，只丢进度。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn progress_flood_does_not_lose_result() {
        let (mgr, mut requests, pending, progress_tx) = ManagedElevate::detached();
        let responder = tokio::spawn(async move {
            let req = requests.recv().await.unwrap();
            for i in 0..1000u64 {
                let _ = progress_tx.send((req.id.clone(), Progress::Bytes(i)));
            }
            let tx = pending.lock().unwrap().remove(&req.id).unwrap();
            tx.send(Ok(IpcResult::Ping)).unwrap();
        });
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen2 = seen.clone();
        let res = time::timeout(
            Duration::from_secs(5),
            mgr.run(
                IpcOperation::Ping,
                true,
                progress_notify(move |_| {
                    seen2.fetch_add(1, Ordering::Relaxed);
                }),
            ),
        )
        .await
        .expect("run timed out")
        .unwrap();
        assert!(matches!(res, IpcResult::Ping));
        responder.await.unwrap();
        // progress is best effort: some of the 1000 arrive, never all of them are required
        assert!(seen.load(Ordering::Relaxed) <= 1000);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_idle_waits_for_dropped_request_result() {
        let (mgr, _requests, pending, _progress_tx) = ManagedElevate::detached();
        let receiver = register(&pending, "pending");
        drop(receiver);
        let mut wait = tokio::spawn(async move { mgr.wait_idle().await });

        assert!(time::timeout(Duration::from_millis(50), &mut wait)
            .await
            .is_err());
        let tx = pending.lock().unwrap().remove("pending").unwrap();
        let _ = tx.send(Ok(IpcResult::Ping));
        time::timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap();
    }
}
