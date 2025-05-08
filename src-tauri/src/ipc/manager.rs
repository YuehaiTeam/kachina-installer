use super::operation::run_opr;
use super::operation::IpcOperation;
use crate::utils::acl::create_security_attributes;
use crate::utils::error::TAResult;
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
use tokio::net::windows::named_pipe::PipeMode;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::time;
use windows::Win32::Foundation::ERROR_PIPE_BUSY;

// 100k buffer size
static PIPE_BUFFER_SIZE: usize = 1024 * 100;
// 4k chunk size due to https://github.com/tokio-rs/mio/pull/1778
static PIPE_CHUNK_SIZE: usize = 1024 * 4;

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct IpcInner {
    op: IpcOperation,
    id: String,
}

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
                .pipe_mode(PipeMode::Message)
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
            let name = format!(r"\\.\pipe\Kachina-Elevate-{}", name);
            let mut server = Self::create_pipe(&name).context("ELEVATE_ERR")?;
            println!("Pipe listener created at {:?}", name);
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
        println!("Failed to accept pipe connection: {:?}", err);
        return false;
    }
    println!("Client connected to pipe");
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
        loop {
            let mut buf = String::new();
            tokio::select! {
                v = serverrx.read_line(&mut buf) => {
                    if v.is_ok() {
                        let res = serde_json::from_str::<serde_json::Value>(&buf);
                        if let Ok(res) = res {
                            let _ = tx.send(res);
                        }else{
                            println!("Failed to parse message: {:?} {:?}", res.err(), buf);
                            fail_times += 1;
                            if fail_times > 30 {
                                println!("Failed to parse message too many times, closing pipe");
                                let _ = tx.send(serde_json::json!({ "Err": "PIPE_DISCONNECT_ERR" }));
                                break;
                            }
                        }
                    }else{
                        println!("Failed to read from pipe: {:?}", v.err());
                        break;
                    }
                }
                v = rx.recv() => {
                    if let Some(v) = v {
                        let b = serde_json::to_vec(&v);
                        if let Ok(mut b) = b {
                            b.push(b'\n');
                            // split into 4k chunks
                            let chunks = b.chunks(PIPE_CHUNK_SIZE);
                            for b in chunks{
                                let res = servertx.write(b).await;
                                if let Err(err) = res{
                                    println!("Failed to write to pipe: {:?}", err);
                                }
                            }
                        }else{
                            println!("Failed to serialize message: {:?}", b.err());
                        }
                    }else{
                        println!("Failed to receive message from channel");
                        break;
                    }
                }
            }
        }
    });
}
#[tauri::command]
pub async fn managed_operation(
    ipc: IpcOperation,
    id: String,
    elevate: bool,
    mgr: tauri::State<'_, ManagedElevate>,
    window: tauri::WebviewWindow,
) -> TAResult<serde_json::Value> {
    if !elevate || mgr.already_elevated {
        run_opr(ipc, move |opr| {
            let _ = window.emit(&id, opr);
        })
        .await
    } else {
        if mgr.process.read().await.is_none() {
            println!("Elevate process not started, starting...");
            mgr.start().await?;
            println!("Elevate process started");
        }
        let _ = mgr
            .mpsc_tx
            .send(IpcInner {
                op: ipc,
                id: id.clone(),
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
            .set_description(format!("Client: Failed to connect to pipe: {:?}", err))
            .show();
        return;
    }
    let client = client.unwrap();
    let (clientrx, mut clienttx) = tokio::io::split(client);
    let mut clientrx = tokio::io::BufReader::with_capacity(PIPE_BUFFER_SIZE, clientrx);

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    loop {
        let mut buf = String::new();
        tokio::select! {
            v = clientrx.read_line(&mut buf) => {
                if let Ok(v) = v {
                    if v == 0 {
                        println!("Client: disconnected");
                        break;
                    }
                    let res = serde_json::from_str::<IpcInner>(&buf);
                    if let Ok(res) = res {
                        let tx = tx.clone();
                        let id = res.id.clone();
                        tokio::spawn(async move {
                            let tx2 = tx.clone();
                            println!("Client: Operation started: {:?}", id);
                            let res = run_opr(res.op, move |opr| {
                                let id = res.id.clone();
                                let tx_clone = tx.clone();
                                tokio::spawn(async move {
                                    let _ = tx_clone
                                        .send(serde_json::json!({ "id": id, "data": opr }))
                                        .await;
                                });
                            })
                            .await;
                            println!("Client: Operation done: {:?}", id);
                            let _ = tx2
                                .send(serde_json::json!({ "id": id, "data": res, "done": true }))
                                .await;
                        });
                    } else {
                        println!("Client: Failed to parse message: {:?} {:?}", res.err(), buf);
                    }
                } else {
                    println!("Client: Failed to read from pipe: {:?}", v.err());
                    break;
                }
            },
            v = rx.recv() =>{
                if let Some(v) = v {
                    let b = serde_json::to_vec(&v);
                    if let Ok(mut b) = b {
                        b.push(b'\n');
                        // split into chunks
                        let chunks = b.chunks(PIPE_CHUNK_SIZE);
                        for b in chunks {
                            let res = clienttx.write(b).await;
                            if let Err(err) = res {
                                println!("Client: Failed to write to pipe: {:?}", err);
                                break;
                            }
                        }
                    } else {
                        println!("Client: Failed to serialize message: {:?}", b.err());
                    }
                } else {
                    println!("Client: Failed to receive message from channel");
                    break;
                }
            }
        }
    }
}
