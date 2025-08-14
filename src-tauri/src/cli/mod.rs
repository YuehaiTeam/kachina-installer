pub mod arg;
use arg::{Command, InstallArgs};
use clap::Parser;

use crate::{utils::url::HttpContextExt, REQUEST_CLIENT};

#[derive(Parser)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[clap(flatten)]
    pub install: InstallArgs,
}
impl Cli {
    pub fn command(&self) -> Command {
        self.command
            .clone()
            .unwrap_or(Command::Install(self.install.clone()))
    }
}

pub async fn install_webview2() {
    println!("安装程序缺少必要的运行环境");
    println!("当前系统未安装 WebView2 运行时，正在下载并安装...");
    // use reqwest to download the installer
    let wv2_url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let res = REQUEST_CLIENT
        .get(wv2_url)
        .send()
        .await
        .with_http_context("install_webview2", wv2_url)
        .expect("Failed to download WebView2 installer");
    let wv2_installer_blob = res
        .bytes()
        .await
        .with_http_context("install_webview2", wv2_url)
        .expect("Failed to read WebView2 installer data");
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir
        .as_path()
        .join("kachina.MicrosoftEdgeWebview2Setup.exe");
    tokio::fs::write(&installer_path, wv2_installer_blob)
        .await
        .expect("failed to write installer to temp dir");
    // run the installer
    let status = tokio::process::Command::new(installer_path.clone())
        .arg("/install")
        .status()
        .await
        .expect("failed to run installer");
    let _ = tokio::fs::remove_file(installer_path).await;
    if status.success() {
        println!("WebView2 运行时安装成功");
        println!("正在重新启动安装程序...");
        // exec self and detatch
        let _ = tokio::process::Command::new(std::env::current_exe().unwrap()).spawn();
        // delete the installer
    } else {
        println!("WebView2 运行时安装失败");
        println!("按任意键退出...");
        let _ = tokio::io::AsyncReadExt::read(&mut tokio::io::stdin(), &mut [0u8]).await;
        std::process::exit(0);
    }
}
