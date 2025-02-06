use super::operation::run_opr;
use super::operation::IpcOperation;
use crate::utils::acl::create_security_attributes;
use crate::utils::uac::check_elevated;
use crate::utils::uac::run_elevated;
use crate::utils::uac::SendableHandle;
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
        let pipe_id = format!("Kachina-Elevate-{}", uuid::Uuid::new_v4());
        Self {
            process: tokio::sync::RwLock::new(None),
            broadcast_tx,
            mpsc_tx,
            mpsc_rx: tokio::sync::RwLock::new(Some(mpsc_rx)),
            pipe_id,
            already_elevated: check_elevated().unwrap_or(false),
        }
    }
    pub fn create_pipe(name: &str) -> Result<NamedPipeServer, String> {
        let mut attr = create_security_attributes();
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .pipe_mode(PipeMode::Message)
                .create_with_security_attributes_raw(name, &mut attr as *mut _ as *mut c_void)
        };
        match server {
            Ok(server) => Ok(server),
            Err(err) => Err(format!("{:?}", err)),
        }
    }
    pub async fn start(&self) -> Result<(), String> {
        let mut process = self.process.write().await;
        if process.is_none() {
            let command = run_elevated(
                std::env::current_exe().unwrap(),
                format!("headless-uac {}", self.pipe_id),
            );
            let command = match command {
                Ok(cmd) => cmd,
                Err(e) => {
                    return Err(format!("Failed to start elevate process: {:?}", e));
                }
            };
            process.replace(command);
            let name = self.pipe_id.clone();
            let name = format!(r"\\.\pipe\{}", name);
            let server = Self::create_pipe(&name);
            if server.is_err() {
                return Err("Failed to create pipe listener".to_string());
            }
            println!("Pipe listener created at {:?}", name);
            let mut server = server.unwrap();
            let tx = self.broadcast_tx.clone();
            let rx = self.mpsc_rx.write().await.take().unwrap();
            if !wait_conn(&mut server).await {
                return Err("Failed to wait for connection".to_string());
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
        let mut serverrx = tokio::io::BufReader::new(serverrx);
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
                            let _ = servertx.write(&b).await;
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
) -> Result<serde_json::Value, String> {
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
        Err("Failed to receive response from elevate process".to_string())
    }
}

pub async fn uac_ipc_main(args: crate::cli::arg::UacArgs) {
    let pipe_name = format!(r"\\.\pipe\{}", args.pipe_id);
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
            .set_description(format!("Failed to connect to pipe: {:?}", err))
            .show();
        return;
    }
    let client = client.unwrap();
    let (clientrx, mut clienttx) = tokio::io::split(client);
    let mut clientrx = tokio::io::BufReader::new(clientrx);

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    loop {
        let mut buf = String::new();
        tokio::select! {
            v = clientrx.read_line(&mut buf) => {
                if let Ok(v) = v {
                    if v == 0 {
                        println!("Client disconnected");
                        break;
                    }
                    let res = serde_json::from_str::<IpcInner>(&buf);
                    if let Ok(res) = res {
                        let tx = tx.clone();
                        let id = res.id.clone();
                        tokio::spawn(async move {
                            let tx2 = tx.clone();
                            println!("Operation started: {:?}", id);
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
                            println!("Operation done: {:?}", id);
                            let _ = tx2
                                .send(serde_json::json!({ "id": id, "data": res, "done": true }))
                                .await;
                        });
                    } else {
                        println!("Failed to parse message: {:?} {:?}", res.err(), buf);
                    }
                } else {
                    println!("Failed to read from pipe: {:?}", v.err());
                    break;
                }
            },
            v = rx.recv() =>{
                if let Some(v) = v {
                    let b = serde_json::to_vec(&v);
                    if let Ok(mut b) = b {
                        b.push(b'\n');
                        let _ = clienttx.write(&b).await;
                    } else {
                        println!("Failed to serialize message: {:?}", b.err());
                    }
                } else {
                    println!("Failed to receive message from channel");
                    break;
                }
            }
        }
    }
}
