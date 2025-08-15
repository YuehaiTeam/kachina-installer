use super::operation::run_opr;
use super::operation::IpcOperation;
use crate::utils::acl::create_security_attributes;
use crate::utils::error::TAResult;
use crate::utils::sentry::forward_envelope;
use crate::utils::sentry::AUTO_TRANSPORT;
use crate::utils::uac::check_elevated;
use crate::utils::uac::run_elevated;
use crate::utils::uac::SendableHandle;
use anyhow::Context;
use std::ffi::c_void;
use std::time::Duration;
use tauri::Emitter;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::time;
use windows::Win32::Foundation::ERROR_PIPE_BUSY;

// 1m buffer size
static PIPE_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct IpcInner {
    op: IpcOperation,
    id: String,
    context: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct ManagedElevate {
    process: tokio::sync::RwLock<Option<SendableHandle>>,
    mpsc_tx: tokio::sync::mpsc::Sender<IpcInner>,
    mpsc_rx: tokio::sync::RwLock<Option<tokio::sync::mpsc::Receiver<IpcInner>>>,
    broadcast_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
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
        let (broadcast_tx, _broadcast_rx) = tokio::sync::broadcast::channel(100);
        let (mpsc_tx, mpsc_rx) = tokio::sync::mpsc::channel(100);
        let pipe_id = format!("{}", uuid::Uuid::new_v4());
        Self {
            process: tokio::sync::RwLock::new(None),
            broadcast_tx,
            mpsc_tx,
            mpsc_rx: tokio::sync::RwLock::new(Some(mpsc_rx)),
            pipe_id,
            already_elevated: check_elevated().unwrap_or(false),
        }
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
            let command = run_elevated(
                std::env::current_exe().unwrap(),
                format!("headless-uac {}", self.pipe_id),
            )
            .context("ELEVATE_ERR")?;
            process.replace(command);
            let name = self.pipe_id.clone();
            let name = format!(r"\\.\pipe\Kachina-Elevate-{name}");
            let mut server = Self::create_pipe(&name).context("ELEVATE_ERR")?;
            tracing::info!("Pipe listener created at {:?}", name);
            let tx = self.broadcast_tx.clone();
            let rx = self.mpsc_rx.write().await.take().unwrap();
            if !wait_conn(&mut server).await {
                return Err(anyhow::anyhow!("Failed to wait for connection").context("ELEVATE_ERR"));
            }
            handle_pipe(server, tx, rx).await;
        }
        Ok(())
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
pub async fn handle_pipe(
    server: NamedPipeServer,
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    mut rx: tokio::sync::mpsc::Receiver<IpcInner>,
) {
    // let mut rx = mgr.mpsc_rx.write().await.take().unwrap();
    tokio::spawn(async move {
        let (serverrx, mut servertx) = tokio::io::split(server);
        let mut serverrx = tokio::io::BufReader::with_capacity(PIPE_BUFFER_SIZE, serverrx);
        let mut fail_times = 0;
        let mut buf = String::new();
        loop {
            tokio::select! {
                v = serverrx.read_line(&mut buf) => {
                    if v.is_ok() {
                        if buf.trim().is_empty() {
                            buf.clear();
                            continue;
                        }
                        let res = serde_json::from_str::<serde_json::Value>(&buf);
                        if let Ok(res) = res {
                            // sentry envelope
                            if let Some(envelope) = res["envelope"].as_str() {
                                let envelope = sentry::Envelope::from_slice(envelope.as_bytes());
                                match envelope {
                                    Ok(envelope) => {
                                        forward_envelope(envelope);
                                    }
                                    Err(err) => {
                                        tracing::warn!("Failed to parse envelope: {:?}", err);
                                    }
                                }
                            }
                            // sentry breadcrumb
                            if res["breadcrumb"].is_object() {
                                let breadcrumb = res["breadcrumb"].clone();
                                if let Ok(breadcrumb) = serde_json::from_value::<sentry::Breadcrumb>(breadcrumb) {
                                    sentry::add_breadcrumb(breadcrumb);
                                }
                            }
                            let _ = tx.send(res);
                        }else{
                            fail_times += 1;
                            if fail_times > 30 {
                                tracing::error!("Failed to parse message too many times, closing pipe");
                                let _ = tx.send(serde_json::json!({ "PipeErr": "PIPE_DISCONNECT_ERR" }));
                                break;
                            }
                        }
                        buf.clear();
                    }else{
                        tracing::error!("Failed to read from pipe: {:?}", v.err());
                        break;
                    }
                }
                v = rx.recv() => {
                    if let Some(v) = v {
                        let b = serde_json::to_vec(&v);
                        if let Ok(mut b) = b {
                            b.extend_from_slice(b"\n \n \n");
                            let res = servertx.write_all(&b).await;
                            if let Err(err) = res {
                                tracing::warn!("Failed to write to pipe: {:?}", err);
                            }
                        }else{
                            tracing::warn!("Failed to serialize message: {:?}", b.err());
                        }
                    }else{
                        tracing::error!("Failed to receive message from channel");
                        break;
                    }
                }
            }
        }
    });
}
#[tracing::instrument(skip(ipc, mgr, window))]
#[tauri::command]
pub async fn managed_operation(
    ipc: IpcOperation,
    id: String,
    elevate: bool,
    mgr: tauri::State<'_, ManagedElevate>,
    window: tauri::WebviewWindow,
) -> TAResult<serde_json::Value> {
    if !elevate || mgr.already_elevated {
        run_opr(
            ipc,
            move |opr| {
                let _ = window.emit(&id, opr);
            },
            vec![],
        )
        .await
    } else {
        if mgr.process.read().await.is_none() {
            tracing::info!("Elevate process not started, starting...");
            mgr.start().await?;
            tracing::info!("Elevate process started");
        }
        let mut context = vec![];
        if let Some(span) = sentry::configure_scope(|scope| scope.get_span()) {
            for (k, v) in span.iter_headers() {
                context.push((k.to_string(), v.to_string()));
            }
        }
        let _ = mgr
            .mpsc_tx
            .send(IpcInner {
                op: ipc,
                id: id.clone(),
                context,
            })
            .await;
        let mut rx = mgr.broadcast_tx.subscribe();
        while let Ok(v) = rx.recv().await {
            let msgid = v["id"].as_str();
            if let Some(msgid) = msgid {
                if msgid == id {
                    if let Some(done) = v["done"].as_bool() {
                        if done {
                            return Ok(v["data"].clone());
                        }
                    }
                    let _ = window.emit(&id, v["data"].clone());
                }
            }
            let pipeerr = v["PipeErr"].as_str();
            if let Some(pipeerr) = pipeerr {
                return Err(anyhow::anyhow!("Elevate process disconnected: {}", pipeerr)
                    .context("IPC_ERR")
                    .into());
            }
        }
        Err(
            anyhow::anyhow!("Failed to receive response from elevate process")
                .context("IPC_ERR")
                .into(),
        )
    }
}

pub async fn uac_ipc_main(args: crate::cli::arg::UacArgs) {
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
        rfd::MessageDialog::new()
            .set_title("Elevate Fail")
            .set_description(format!("Client: Failed to connect to pipe: {err:?}"))
            .show();
        return;
    }
    let client = client.unwrap();
    let (clientrx, mut clienttx) = tokio::io::split(client);
    let mut clientrx = tokio::io::BufReader::with_capacity(PIPE_BUFFER_SIZE, clientrx);

    let (tx, mut rx) = tokio::sync::mpsc::channel(500);
    let mut sentry_rx = AUTO_TRANSPORT.mpsc_rx.write().await;
    let mut buf = String::new();

    // 创建一个取消通知器
    let (cancel_tx, cancel_rx) = tokio::sync::broadcast::channel(1);

    // 第一个线程：处理客户端读取
    let read_handle = {
        let tx = tx.clone();
        let cancel_tx = cancel_tx.clone();
        let mut cancel_rx = cancel_rx.resubscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_rx.recv() => {
                        tracing::info!("Read thread cancelled");
                        break;
                    }
                    v = clientrx.read_line(&mut buf) => {
                        if let Ok(v) = v {
                            if v == 0 {
                                tracing::warn!("Client: disconnected");
                                let _ = cancel_tx.send(());
                                break;
                            }
                            if buf.trim().is_empty() {
                                buf.clear();
                                continue;
                            }
                            let res = serde_json::from_str::<IpcInner>(&buf);
                            if let Ok(res) = res {
                                let tx = tx.clone();
                                let id = res.id.clone();
                                tokio::spawn(async move {
                                    let tx2 = tx.clone();
                                    let res = run_opr(res.op, move |opr| {
                                        let id = res.id.clone();
                                        let tx_clone = tx.clone();
                                        tokio::spawn(async move {
                                            let _ = tx_clone
                                                .send(serde_json::json!({ "id": id, "data": opr }))
                                                .await;
                                        });
                                    },res.context)
                                    .await;
                                    if let Err(err) = res.as_ref() {
                                        tracing::error!("Client: Operation failed: {:?}", err);
                                    }
                                    let _ = tx2
                                        .send(serde_json::json!({ "id": id, "data": res, "done": true }))
                                        .await;
                                });
                            } else {
                                tracing::warn!("Client: Failed to parse message: {:?} {:?}", res.err(), buf);
                            }
                            buf.clear();
                        } else {
                            tracing::warn!("Client: Failed to read from pipe: {:?}", v.err());
                            let _ = cancel_tx.send(());
                            break;
                        }
                    }
                }
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
                            let b = serde_json::to_vec(&v);
                            if let Ok(mut b) = b {
                                b.extend_from_slice(b"\n \n \n");
                                let res = clienttx.write_all(&b).await;
                                if let Err(err) = res {
                                    tracing::warn!("Client: Failed to write to pipe: {:?}", err);
                                    let _ = cancel_tx.send(());
                                    break;
                                }
                            } else {
                                tracing::warn!("Client: Failed to serialize message: {:?}", b.err());
                            }
                        } else {
                            tracing::warn!("Client: Failed to receive message from channel");
                            let _ = cancel_tx.send(());
                            break;
                        }
                    }
                    v = sentry_rx.recv() => {
                        if let Some(v) = v {
                            match v {
                                crate::utils::sentry::SentryData::Breadcrumb(b) => {
                                    let _ = tx.send(serde_json::json!({ "breadcrumb": b })).await;
                                },
                                crate::utils::sentry::SentryData::Envelope(v) => {
                                    let mut vec = Vec::new();
                                    v.to_writer(&mut vec).unwrap();
                                    let str = String::from_utf8_lossy(&vec).to_string();
                                    let _ = tx.send(serde_json::json!({ "envelope": str })).await;
                                }
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
