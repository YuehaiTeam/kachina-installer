use std::ffi::c_void;
use std::mem::size_of;
use std::mem::zeroed;

use std::ptr::null_mut;
use std::time::Duration;

use crate::ipc::run_opr;
use crate::ipc::IpcOperation;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::net::windows::named_pipe::PipeMode;
use tokio::time;
use windows::Win32::Foundation::ERROR_PIPE_BUSY;
use windows::Win32::Foundation::HANDLE;

use std::ffi::OsStr;
use tauri::Emitter;
use windows::Win32::Foundation;
use windows::Win32::Security::GetTokenInformation;
use windows::Win32::Security::TokenElevation;
use windows::Win32::Security::TOKEN_ELEVATION;
use windows::Win32::Security::TOKEN_QUERY;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Threading::OpenProcessToken;

use tokio::net::windows::named_pipe::ServerOptions;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};

#[cfg(target_os = "windows")]
pub fn check_elevated() -> windows::core::Result<bool> {
    unsafe {
        let h_process = GetCurrentProcess();
        let mut h_token = Foundation::HANDLE(null_mut());
        let open_result = OpenProcessToken(h_process, TOKEN_QUERY, &mut h_token);
        let mut ret_len: u32 = 0;
        let mut token_info: TOKEN_ELEVATION = zeroed();

        if let Err(e) = open_result {
            println!("OpenProcessToken {:?}", e);
            return Err(e);
        }

        if let Err(e) = GetTokenInformation(
            h_token,
            TokenElevation,
            Some(std::ptr::addr_of_mut!(token_info).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        ) {
            println!("GetTokenInformation {:?}", e);

            return Err(e);
        }

        Ok(token_info.TokenIsElevated != 0)
    }
}

pub fn run_elevated<S: AsRef<OsStr>, T: AsRef<OsStr>>(
    program_path: S,
    args: T,
) -> std::io::Result<SendableHandle> {
    let file = HSTRING::from(program_path.as_ref());
    let par = HSTRING::from(args.as_ref());

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(par.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut sei)?;
        let process = { sei.hProcess };
        if process.is_invalid() {
            return Err(std::io::Error::last_os_error());
        };
        return Ok(SendableHandle(process));
    };
}

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
}

pub struct SendableHandle(pub HANDLE);
unsafe impl Send for SendableHandle {}
unsafe impl Sync for SendableHandle {}

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
        }
    }
}

pub async fn start_elevated(mgr: &ManagedElevate) -> bool {
    let mut process = mgr.process.write().await;
    if process.is_none() {
        let command = run_elevated(
            std::env::current_exe().unwrap(),
            format!("headless-uac {}", mgr.pipe_id),
        );
        let command = match command {
            Ok(cmd) => cmd,
            Err(e) => {
                println!("Failed to start elevate process: {:?}", e);
                return false;
            }
        };
        process.replace(command);
        let name = mgr.pipe_id.clone();
        let name = format!(r"\\.\pipe\{}", name);
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .pipe_mode(PipeMode::Message)
            .create(name.clone());
        if server.is_err() {
            println!("Failed to create pipe listener: {:?}", server.err());
            return false;
        }
        println!("Pipe listener created at {:?}", name);
        let mut server = server.unwrap();
        let tx = mgr.broadcast_tx.clone();
        let rx = mgr.mpsc_rx.write().await.take().unwrap();
        if !wait_conn(&mut server).await {
            return false;
        }
        handle_pipe(server, tx, rx).await;
    }
    return true;
}
pub async fn wait_conn(server: &mut NamedPipeServer) -> bool {
    if let Err(err) = server.connect().await {
        println!("Failed to accept pipe connection: {:?}", err);
        return false;
    }
    println!("Client connected to pipe");
    return true;
}
pub async fn handle_pipe(
    mut server: NamedPipeServer,
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    mut rx: tokio::sync::mpsc::Receiver<IpcInner>,
) {
    // let mut rx = mgr.mpsc_rx.write().await.take().unwrap();
    tokio::spawn(async move {
        // 256k buffer
        let mut buf = [0u8; 256 * 1024];
        loop {
            tokio::select! {
                v = server.read(&mut buf) => {
                    if let Ok(v) = v {
                        let res = serde_json::from_slice::<serde_json::Value>(&buf[..v]);
                        if let Ok(res) = res {
                            let _ = tx.send(res);
                        }else{
                            println!("Failed to parse message: {:?}", res.err());
                        }
                    }else{
                        println!("Failed to read from pipe: {:?}", v.err());
                        break;
                    }
                }
                v = rx.recv() => {
                    if let Some(v) = v {
                        let b = serde_json::to_vec(&v);
                        if let Ok(b) = b {
                            let _ = server.write(&b).await;
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
    if !elevate {
        return run_opr(ipc, move |opr| {
            let _ = window.emit(&id, opr);
        })
        .await;
    } else {
        if mgr.process.read().await.is_none() {
            println!("Elevate process not started, starting...");
            if !start_elevated(&mgr).await {
                return Err("Failed to start elevate process".to_string());
            }
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
        return Err("Failed to receive response from elevate process".to_string());
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
            .set_description(&format!("Failed to connect to pipe: {:?}", err))
            .show();
        return;
    }
    let client = client.unwrap();
    let (mut clientrx, mut clienttx) = tokio::io::split(client);

    let mut buf = vec![0u8; 256 * 1024];

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    tokio::spawn(async move {
        loop {
            let v = clientrx.read(&mut buf).await;
            if let Ok(v) = v {
                if v == 0 {
                    println!("Client disconnected");
                    break;
                }
                let res = serde_json::from_slice::<IpcInner>(&buf[..v]);
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
                    println!("Failed to parse message: {:?}", res.err());
                }
            } else {
                println!("Failed to read from pipe: {:?}", v.err());
                break;
            }
        }
    });
    loop {
        let v = rx.recv().await;
        if let Some(v) = v {
            let b = serde_json::to_vec(&v);
            if let Ok(b) = b {
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
