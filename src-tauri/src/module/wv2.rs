use windows::Win32::Foundation::HWND;

use crate::utils::code::{coded_from_error, Attach, WEBVIEW2_FAILED};
use crate::utils::i18n;
use crate::utils::taskdialog::{show_error_coded, ProgressDialog};
use crate::utils::url::HttpContextExt;
use crate::REQUEST_CLIENT;

fn parent_hwnd(dialog: &ProgressDialog) -> HWND {
    dialog.hwnd().unwrap_or_default()
}

pub async fn install_webview2() -> anyhow::Result<()> {
    if crate::host::webview_version().is_ok() {
        return Ok(());
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
    }
    let title = i18n::t("webview2.progress_title", &[]);
    let heading = i18n::t("webview2.progress_heading", &[]);
    let downloading = i18n::t("webview2.progress_downloading", &[]);
    let dialog = match ProgressDialog::show(&title, &heading, &downloading, true).await {
        Ok(dialog) => dialog,
        Err(err) => {
            let err = err.attach(WEBVIEW2_FAILED);
            if let Some(coded) = coded_from_error(&err) {
                tokio::task::spawn_blocking(move || {
                    show_error_coded(&coded, HWND::default());
                })
                .await
                .ok();
            }
            return Err(err);
        }
    };

    // The dialog is closed before the error is shown; the coded error is both displayed
    // here (no WebView exists yet) and returned to the caller.
    let fail = async |dialog: ProgressDialog, err: anyhow::Error| -> anyhow::Result<()> {
        let parent = parent_hwnd(&dialog).0 as isize;
        dialog.close().await;
        let err = err.attach(WEBVIEW2_FAILED);
        if let Some(coded) = coded_from_error(&err) {
            tokio::task::spawn_blocking(move || {
                show_error_coded(&coded, HWND(parent as *mut _));
            })
            .await
            .ok();
        }
        Err(err)
    };

    let wv2_url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let res = REQUEST_CLIENT
        .get(wv2_url)
        .send()
        .await
        .with_http_context("install_webview2", wv2_url);
    let res = match res {
        Ok(res) => res,
        Err(e) => return fail(dialog, e).await,
    };
    let wv2_installer_blob = res
        .bytes()
        .await
        .with_http_context("install_webview2", wv2_url);
    let wv2_installer_blob = match wv2_installer_blob {
        Ok(bytes) => bytes,
        Err(e) => return fail(dialog, e).await,
    };

    let installer_path =
        crate::fs::staging::scratch_file("kachina.MicrosoftEdgeWebview2Setup.exe");
    if let Err(e) = tokio::fs::write(&installer_path, wv2_installer_blob).await {
        return fail(dialog, e.into()).await;
    }

    dialog.set_content(&i18n::t("webview2.progress_installing", &[]));
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
        Ok(status) => fail(dialog, anyhow::anyhow!("exit status {status}")).await,
        Err(e) => fail(dialog, e.into()).await,
    }
}
