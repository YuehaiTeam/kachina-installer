use super::operation::run_opr;
use super::operation::IpcOperation;
use super::{
    decode_frame, encode_frame, progress_notify, read_frame, IpcError, IpcResult, PipeMsg,
    ProgressNotify,
};
use crate::utils::acl::create_security_attributes;
use crate::utils::code::{Attach, ELEVATE_FAILED};
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
use windows::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows::Win32::System::Threading::{GetProcessId, WaitForSingleObject};

const PIPE_BUFFER_SIZE: usize = 64 * 1024;

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct IpcInner {
    op: IpcOperation,
    id: String,
}

/// One waiter per in-flight request, keyed by request id. The pipe reader
/// calls the request's notify for each of its progress frames, in pipe order,
/// and takes the entry out when the result arrives; progress for an id without
/// an entry is dropped. The notify runs on the reader task, so it must not
/// block: a slow notify delays every later frame, results included.
pub type Pending =
    Arc<Mutex<HashMap<String, (oneshot::Sender<Result<IpcResult, IpcError>>, ProgressNotify)>>>;

fn retired_error() -> IpcError {
    IpcError {
        message: "Elevate process retired: IPC_ERR".into(),
        code: None,
        subject: None,
        sid: None,
        cancelled: false,
        insight: None,
        network: None,
    }
}

pub fn disconnect_error() -> IpcError {
    IpcError {
        message: "Elevate process disconnected: PIPE_DISCONNECT_ERR".into(),
        code: None,
        subject: None,
        sid: None,
        cancelled: false,
        insight: None,
        network: None,
    }
}

/// The elevated side of one session. Each session starts its own helper, so
/// every elevated session asks for consent; [`ManagedElevate::close`] ends it.
pub struct ManagedElevate {
    process: tokio::sync::RwLock<Option<SendableHandle>>,
    started: AtomicBool,
    /// [`start`](Self::start) failed once; it is not retried.
    start_failed: AtomicBool,
    /// The helper holds an exit-time staging cleanup and must outlive this
    /// process.
    holds_cleanup: AtomicBool,
    mpsc_tx: tokio::sync::mpsc::Sender<IpcInner>,
    mpsc_rx: tokio::sync::RwLock<Option<tokio::sync::mpsc::Receiver<IpcInner>>>,
    pending: Pending,
    pipe_id: String,
    already_elevated: bool,
    resolve_tx: tokio::sync::mpsc::UnboundedSender<(super::download::Resolve, bool)>,
    resolve_rx:
        tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<(super::download::Resolve, bool)>>,
}

impl Default for ManagedElevate {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static::lazy_static! {
    /// Retired helpers whose pipe stays open until this process exits.
    static ref RETIRED: Mutex<Vec<ManagedElevate>> = Mutex::new(Vec::new());
}

impl ManagedElevate {
    /// End the session's helper. A helper without a pending cleanup exits as
    /// the pipe closes. One holding a cleanup refuses further operations and
    /// keeps the pipe until this process exits, because the staging root it
    /// deletes holds this process's image.
    pub async fn close(self) {
        if !self.started.load(Ordering::SeqCst) {
            return;
        }
        self.wait_idle().await;
        if !self.holds_cleanup.load(Ordering::SeqCst) {
            return;
        }
        if let Err(err) = self
            .run(IpcOperation::Retire, true, super::progress_noop())
            .await
        {
            tracing::warn!("retire elevate process failed: {err:#}");
        }
        RETIRED.lock().unwrap_or_else(|e| e.into_inner()).push(self);
    }

    /// Whether operations sent with `elevate` run in a separate helper
    /// process, under a token that may not see this session's drive mappings.
    pub fn uses_helper(&self, elevate: bool) -> bool {
        elevate && !self.already_elevated
    }

    pub fn new() -> Self {
        let (mpsc_tx, mpsc_rx) = tokio::sync::mpsc::channel(100);
        let (resolve_tx, resolve_rx) = tokio::sync::mpsc::unbounded_channel();
        let pipe_id = format!("{}", uuid::Uuid::new_v4());
        Self {
            resolve_tx,
            resolve_rx: tokio::sync::Mutex::new(resolve_rx),
            process: tokio::sync::RwLock::new(None),
            started: AtomicBool::new(false),
            start_failed: AtomicBool::new(false),
            holds_cleanup: AtomicBool::new(false),
            mpsc_tx,
            mpsc_rx: tokio::sync::RwLock::new(Some(mpsc_rx)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            pipe_id,
            already_elevated: check_elevated().unwrap_or(false),
        }
    }

    /// A manager whose "elevated side" is whatever the test drives through
    /// the returned request receiver, replying via `pending`.
    #[cfg(test)]
    fn detached() -> (Self, tokio::sync::mpsc::Receiver<IpcInner>, Pending) {
        let mut me = Self::new();
        me.already_elevated = false;
        me.started.store(true, Ordering::SeqCst);
        let rx = me.mpsc_rx.get_mut().take().unwrap();
        let pending = me.pending.clone();
        (me, rx, pending)
    }
    /// The only instance of the pipe. Its DACL admits SYSTEM and enabled
    /// Administrators only ([`create_security_attributes`]): the helper is
    /// elevated, while the launching user's other processes (and every
    /// medium or low integrity process) cannot open it.
    pub fn create_pipe(name: &str) -> anyhow::Result<NamedPipeServer> {
        let mut attr = create_security_attributes();
        Ok(unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .max_instances(1)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, &mut attr as *mut _ as *mut c_void)
        }?)
    }

    /// Start the helper once per session. A failed start (UAC declined, the
    /// helper never connected) is final for this manager: later elevated
    /// operations fail with the same code instead of asking again.
    pub async fn start(&self) -> anyhow::Result<()> {
        let mut process = self.process.write().await;
        // checked first: a failed start may still have left a helper handle
        if self.start_failed.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("elevated helper unavailable").attach(ELEVATE_FAILED));
        }
        if process.is_some() {
            return Ok(());
        }
        let result = self.start_helper(&mut process).await;
        if result.is_err() {
            self.start_failed.store(true, Ordering::SeqCst);
        }
        result.map_err(|err| err.attach(ELEVATE_FAILED))
    }

    async fn start_helper(&self, process: &mut Option<SendableHandle>) -> anyhow::Result<()> {
        let name = format!(r"\\.\pipe\Kachina-Elevate-{}", self.pipe_id);
        let mut server = Self::create_pipe(&name).context("create elevation pipe")?;
        tracing::info!("Pipe server created at {:?}", name);

        // the helper is started only after the pipe exists
        let mut helper_args = format!(
            "headless-uac {} \"{}\" {}",
            self.pipe_id,
            crate::utils::log::path().display(),
            std::process::id()
        );
        if SILENT_SESSION.load(Ordering::SeqCst) {
            helper_args.push_str(" --silent");
        }
        let helper = run_elevated(
            std::env::current_exe().context("CURRENT_EXE_ERR")?,
            helper_args,
        )
        .context("start elevated helper")?;
        let helper_raw = helper.0 .0 as isize;
        process.replace(helper);
        tracing::info!("UAC process started, waiting for pipe connection...");

        accept_helper(&mut server, helper_raw).await?;
        let rx = self
            .mpsc_rx
            .write()
            .await
            .take()
            .context("elevation pipe already used")?;
        handle_pipe(server, self.pending.clone(), rx, self.resolve_tx.clone()).await;
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub async fn run(
        &self,
        ipc: IpcOperation,
        elevate: bool,
        on_progress: ProgressNotify,
    ) -> TAResult<IpcResult> {
        if !self.uses_helper(elevate) {
            let (result, snapshot) = super::download::RESOLVER
                .scope(
                    self.resolve_tx.clone(),
                    super::network::measure(on_progress.clone(), |notify| run_opr(ipc, notify)),
                )
                .await;
            snapshot.publish(&on_progress);
            return result;
        }
        if !self.started.load(Ordering::SeqCst) {
            tracing::info!("Elevate process not started, starting...");
            self.start().await?;
            tracing::info!("Elevate process started");
        }
        if matches!(ipc, IpcOperation::ScheduleCleanup(_)) {
            self.holds_cleanup.store(true, Ordering::SeqCst);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, result_rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), (tx, on_progress));
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
        match result_rx.await {
            Ok(Ok(data)) => Ok(data),
            Ok(Err(error)) => Err(error.into_ta()),
            Err(_) => Err(
                anyhow::anyhow!("Failed to receive response from elevate process")
                    .context("IPC_ERR")
                    .into(),
            ),
        }
    }

    pub async fn next_resolution(&self) -> Option<(super::download::Resolve, bool)> {
        self.resolve_rx.lock().await.recv().await
    }

    pub async fn resolve_reply(&self, reply: super::download::Reply, remote: bool) {
        if remote {
            let _ = self
                .mpsc_tx
                .send(IpcInner {
                    op: IpcOperation::ResolveDownload(reply),
                    id: String::new(),
                })
                .await;
        } else {
            super::download::reply(reply);
        }
    }

    /// Block until every in-flight elevate request has a result. Pending
    /// entries stay until the elevate side replies, even if the local `run`
    /// future was dropped (phase-one cancel).
    pub async fn wait_idle(&self) {
        let started = std::time::Instant::now();
        let mut waited = false;
        loop {
            let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner()).len();
            if pending == 0 {
                if waited {
                    tracing::info!("elevated requests drained after {:?}", started.elapsed());
                }
                return;
            }
            if !waited {
                tracing::info!("waiting for {pending} in-flight elevated requests");
                waited = true;
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
    for (_, (tx, _)) in waiters {
        let _ = tx.send(Err(disconnect_error()));
    }
}

/// Set by a silent launch (`-S`): the helper is started with `--silent` and
/// shows no dialogs either.
pub static SILENT_SESSION: AtomicBool = AtomicBool::new(false);

/// Accept the helper's connection and check that the client is the helper
/// process this session started. Fails once the helper exits without
/// connecting (it could not open the pipe, or was refused).
async fn accept_helper(server: &mut NamedPipeServer, helper: isize) -> anyhow::Result<()> {
    // `HANDLE` is not `Send`; it is rebuilt at each use instead of held across awaits
    let helper_handle = || HANDLE(helper as *mut c_void);
    let exited = async {
        loop {
            if unsafe { WaitForSingleObject(helper_handle(), 0) } == WAIT_OBJECT_0 {
                return;
            }
            time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::select! {
        conn = server.connect() => conn.context("accept elevated helper connection")?,
        _ = exited => anyhow::bail!("elevated helper exited before connecting"),
    }
    let mut client_pid = 0u32;
    let pipe = HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(server));
    unsafe { GetNamedPipeClientProcessId(pipe, &mut client_pid) }
        .context("query elevation pipe client")?;
    // 0 when the handle from the elevation lacks query rights; the pipe's
    // DACL already limits clients to elevated processes, and the helper
    // checks this side's pid in turn
    let helper_pid = unsafe { GetProcessId(helper_handle()) };
    if helper_pid == 0 {
        tracing::warn!("helper pid unavailable, client {client_pid} not matched");
    } else if client_pid != helper_pid {
        anyhow::bail!("elevation pipe client {client_pid} is not the helper {helper_pid}");
    }
    tracing::info!("Client connected to pipe");
    Ok(())
}
/// 读写各一个任务：`read_frame` 不可取消，不能放进与写端共用的 `select!`。
/// 任一端结束（对端关闭、帧错位、读写错误、`ManagedElevate` 被丢弃）就停掉另一端，
/// 让两个半边一起释放、管道真正关闭，并让所有等待中的 `run` 收到断连错误。
pub async fn handle_pipe(
    server: NamedPipeServer,
    pending: Pending,
    mut rx: tokio::sync::mpsc::Receiver<IpcInner>,
    resolve_tx: tokio::sync::mpsc::UnboundedSender<(super::download::Resolve, bool)>,
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
                Ok(PipeMsg::Breadcrumb(crumb)) => {
                    crate::utils::sentry::add_breadcrumb_json(&crumb);
                }
                Ok(PipeMsg::Progress(id, p)) => {
                    // the guard is dropped at the end of this statement: the
                    // notify runs with `pending` unlocked
                    let notify = reader_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&id)
                        .map(|(_, notify)| notify.clone());
                    if let Some(notify) = notify {
                        notify(p);
                    }
                }
                Ok(PipeMsg::Ok(id, data, snapshot)) => {
                    if let Some((tx, notify)) = reader_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                    {
                        snapshot.publish(&notify);
                        let _ = tx.send(Ok(data));
                    }
                }
                Ok(PipeMsg::Err(id, err, snapshot)) => {
                    if let Some((tx, notify)) = reader_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                    {
                        snapshot.publish(&notify);
                        let _ = tx.send(Err(err));
                    }
                }
                Ok(PipeMsg::Resolve(query)) => {
                    let _ = resolve_tx.send((query, true));
                }
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

    let client = match client {
        Ok(client) => client,
        Err(err) => {
            tracing::error!("elevated helper cannot open the pipe: {err:?}");
            // the launcher sees this process exit and fails the session itself
            if !args.silent {
                let detail = format!("{err:?}");
                crate::utils::taskdialog::show_error(
                    crate::utils::taskdialog::ErrorDialog {
                        detail: Some(&detail),
                        ..crate::utils::taskdialog::ErrorDialog::code(ELEVATE_FAILED)
                    },
                    windows::Win32::Foundation::HWND::default(),
                );
            }
            return;
        }
    };
    // the pipe's server must be the process that started this helper
    let mut server_pid = 0u32;
    let handle = HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&client));
    if let Err(err) = unsafe { GetNamedPipeServerProcessId(handle, &mut server_pid) } {
        tracing::error!("elevation pipe server unknown: {err}");
        return;
    }
    if server_pid != args.launcher_pid {
        tracing::error!(
            "elevation pipe server {server_pid} is not the launcher {}",
            args.launcher_pid
        );
        return;
    }
    crate::fs::staging::set_lock_owner(server_pid);
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
            let mut retired = false;
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
                if retired {
                    let _ = tx
                        .send(PipeMsg::Err(res.id, retired_error(), Default::default()))
                        .await;
                    continue;
                }
                match res.op {
                    IpcOperation::ResolveDownload(reply) => {
                        super::download::reply(reply);
                        continue;
                    }
                    IpcOperation::Retire => {
                        tracing::info!("IPC operation: Retire");
                        retired = true;
                        let _ = tx
                            .send(PipeMsg::Ok(res.id, IpcResult::Ping, Default::default()))
                            .await;
                        continue;
                    }
                    _ => {}
                }
                let tx = tx.clone();
                let id = res.id.clone();
                tokio::spawn(async move {
                    let (resolve_tx, mut resolve_rx) = tokio::sync::mpsc::unbounded_channel();
                    let progress_tx = tx.clone();
                    let progress_id = id.clone();
                    let notify = progress_notify(move |p| {
                        // 进度允许丢失；结果与解析请求通过可靠发送路径传递。
                        let _ = progress_tx.try_send(PipeMsg::Progress(progress_id.clone(), p));
                    });
                    let mut operation = Box::pin(super::download::RESOLVER.scope(
                        resolve_tx,
                        super::network::measure(notify.clone(), |notify| run_opr(res.op, notify)),
                    ));
                    let (result, snapshot) = loop {
                        tokio::select! {
                            result = &mut operation => break result,
                            Some((query, _)) = resolve_rx.recv() => { let _ = tx.send(PipeMsg::Resolve(query)).await; }
                        }
                    };
                    let msg = match result {
                        Ok(data) => PipeMsg::Ok(id, data, snapshot),
                        Err(err) => PipeMsg::Err(id, IpcError::from_ta(&err), snapshot),
                    };
                    let _ = tx.send(msg).await;
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
                    // written here directly: this task is the only reader of
                    // `rx`, so waiting for room in it could never finish
                    Some(msg) = sentry_rx.recv() => {
                        match encode_frame(&msg) {
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
        pending
            .lock()
            .unwrap()
            .insert(id.to_string(), (tx, super::super::progress_noop()));
        rx
    }

    /// 真实命名管道上的往返：服务端 `handle_pipe` 对接客户端 `uac_ipc_main`，
    /// 覆盖 Ok / Err 两种回包与丢弃 `ManagedElevate` 后的断连收尾。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pipe_roundtrip_and_disconnect() {
        let pipe_id = uuid::Uuid::new_v4().to_string();
        // default security: the test process is not elevated, and the real
        // pipe admits elevated clients only
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(format!(r"\\.\pipe\Kachina-Elevate-{pipe_id}"))
            .unwrap();
        let client = tokio::spawn(uac_ipc_main(UacArgs {
            pipe_id,
            log_path: std::env::temp_dir().join("kachina-test.log"),
            launcher_pid: std::process::id(),
            silent: true,
        }));
        server.connect().await.unwrap();

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (mpsc_tx, mpsc_rx) = tokio::sync::mpsc::channel(16);
        handle_pipe(
            server,
            pending.clone(),
            mpsc_rx,
            tokio::sync::mpsc::unbounded_channel().0,
        )
        .await;

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

        for (id, op) in [
            ("retire", IpcOperation::Retire),
            ("late", IpcOperation::Ping),
        ] {
            let reply = register(&pending, id);
            mpsc_tx.send(IpcInner { op, id: id.into() }).await.unwrap();
            let got = wait(reply).await;
            if id == "retire" {
                assert!(matches!(got, Ok(Ok(IpcResult::Ping))));
            } else {
                let Ok(Err(err)) = got else {
                    panic!("a retired helper must refuse later operations");
                };
                assert!(err.message.contains("retired"), "{}", err.message);
            }
        }

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

    fn test_pipe() -> (String, NamedPipeServer) {
        let name = format!(r"\\.\pipe\Kachina-Elevate-{}", uuid::Uuid::new_v4());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        (name, server)
    }

    fn raw(child: &std::process::Child) -> isize {
        std::os::windows::io::AsRawHandle::as_raw_handle(child) as isize
    }

    /// 子进程没连上就退出时，启动进程报错结束等待，而不是一直等连接。
    #[tokio::test]
    async fn helper_exit_before_connecting_fails_the_wait() {
        let (_name, mut server) = test_pipe();
        let mut helper = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "0"])
            .spawn()
            .unwrap();
        let err = time::timeout(
            Duration::from_secs(5),
            accept_helper(&mut server, raw(&helper)),
        )
        .await
        .expect("accept_helper kept waiting after the helper exited")
        .unwrap_err();
        assert!(err.to_string().contains("exited"), "{err:#}");
        let _ = helper.wait();
    }

    /// 连上管道的不是本次拉起的子进程时拒绝。
    #[tokio::test]
    async fn connection_from_another_process_is_refused() {
        let (name, mut server) = test_pipe();
        let mut helper = std::process::Command::new("cmd.exe")
            .args(["/c", "ping", "-n", "10", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let _intruder = ClientOptions::new().open(&name).unwrap();
        let err = accept_helper(&mut server, raw(&helper)).await.unwrap_err();
        assert!(err.to_string().contains("is not the helper"), "{err:#}");
        let _ = helper.kill();
        let _ = helper.wait();
    }

    /// 提权侧写来的进度帧按 id 交给该请求的回调，且先于结果的最终快照；
    /// 未登记 id 的进度丢弃，不影响之后的帧。
    #[tokio::test]
    async fn progress_frames_reach_only_their_request() {
        use super::super::Progress;

        let (name, server) = test_pipe();
        let mut helper = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        // held so the writer half, and with it the pipe task, stays up
        let (_requests, mpsc_rx) = tokio::sync::mpsc::channel(1);
        handle_pipe(
            server,
            pending.clone(),
            mpsc_rx,
            tokio::sync::mpsc::unbounded_channel().0,
        )
        .await;

        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let (tx, reply) = oneshot::channel();
        pending.lock().unwrap().insert(
            "op".into(),
            (tx, progress_notify(move |p| log.lock().unwrap().push(p))),
        );
        let mine = Progress::BytesOf { done: 1, total: 2 };
        for msg in [
            PipeMsg::Progress("op".into(), mine.clone()),
            PipeMsg::Progress("unknown".into(), Progress::CountOf { done: 3, total: 4 }),
            PipeMsg::Ok("op".into(), IpcResult::Ping, Default::default()),
        ] {
            helper
                .write_all(&encode_frame(&msg).unwrap())
                .await
                .unwrap();
        }

        let got = time::timeout(Duration::from_secs(5), reply)
            .await
            .expect("pipe reply timeout");
        assert!(matches!(got, Ok(Ok(IpcResult::Ping))));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![mine, Progress::NetworkFinal(Default::default())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_idle_waits_for_dropped_request_result() {
        let (mgr, _requests, pending) = ManagedElevate::detached();
        let receiver = register(&pending, "pending");
        drop(receiver);
        let mut wait = tokio::spawn(async move { mgr.wait_idle().await });

        assert!(time::timeout(Duration::from_millis(50), &mut wait)
            .await
            .is_err());
        let tx = pending.lock().unwrap().remove("pending").unwrap();
        let _ = tx.0.send(Ok(IpcResult::Ping));
        time::timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap();
    }
}
