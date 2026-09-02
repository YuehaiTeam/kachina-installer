use crate::utils::taskdialog::ProgressDialog;
use crate::utils::url::HttpContextExt;
use crate::REQUEST_CLIENT;

pub async fn install_webview2() -> anyhow::Result<()> {
    if crate::host::webview_version().is_ok() {
        return Ok(());
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
    }
    let dialog = match ProgressDialog::show(
        "安装 WebView2 运行时",
        "当前系统缺少 WebView2 运行时，正在安装...",
        "正在下载安装程序...",
        true,
    )
    .await
    {
        Ok(dialog) => dialog,
        Err(err) => {
            rfd::MessageDialog::new()
                .set_title("出错了")
                .set_description(format!("无法显示安装进度: {err}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            return Err(err);
        }
    };

    let fail = async |dialog: ProgressDialog, message: String| -> anyhow::Result<()> {
        dialog.close().await;
        rfd::MessageDialog::new()
            .set_title("出错了")
            .set_description(&message)
            .set_level(rfd::MessageLevel::Error)
            .show();
        Err(anyhow::anyhow!(message))
    };

    let wv2_url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let res = REQUEST_CLIENT
        .get(wv2_url)
        .send()
        .await
        .with_http_context("install_webview2", wv2_url);
    let res = match res {
        Ok(res) => res,
        Err(e) => return fail(dialog, format!("WebView2 运行时下载失败: {e}")).await,
    };
    let wv2_installer_blob = res
        .bytes()
        .await
        .with_http_context("install_webview2", wv2_url);
    let wv2_installer_blob = match wv2_installer_blob {
        Ok(bytes) => bytes,
        Err(e) => return fail(dialog, format!("WebView2 运行时下载失败: {e}")).await,
    };

    let installer_path = std::env::temp_dir().join("kachina.MicrosoftEdgeWebview2Setup.exe");
    if let Err(e) = tokio::fs::write(&installer_path, wv2_installer_blob).await {
        return fail(dialog, format!("WebView2 运行时安装程序写入失败: {e}")).await;
    }

    dialog.set_content("正在安装 WebView2 运行时...");
    let code = match crate::utils::process::spawn(&installer_path, &["/install"], false) {
        Ok(child) => child.wait().await,
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_file(&installer_path).await;
    match code {
        Ok(0) => {
            dialog.close().await;
            Ok(())
        }
        Ok(_) => fail(dialog, "WebView2 运行时安装失败".to_string()).await,
        Err(e) => fail(dialog, format!("WebView2 运行时安装失败: {e}")).await,
    }
}
