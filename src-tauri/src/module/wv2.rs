use windows::Win32::Foundation::HWND;

use crate::utils::code::{Coded, WEBVIEW2_FAILED};
use crate::utils::i18n;
use crate::utils::taskdialog::{show_error, ProgressDialog};
use crate::utils::url::HttpContextExt;
use crate::REQUEST_CLIENT;

fn parent_hwnd(dialog: &ProgressDialog) -> HWND {
    dialog.hwnd().unwrap_or_default()
}

fn show_wv2_error(detail: Option<&str>, parent: HWND) {
    show_error(WEBVIEW2_FAILED, detail, None, parent);
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
            let detail = format!("{err:#}");
            tokio::task::spawn_blocking(move || {
                show_wv2_error(Some(&detail), HWND::default());
            })
            .await
            .ok();
            return Err(err);
        }
    };

    let fail = async |dialog: ProgressDialog, detail: String| -> anyhow::Result<()> {
        let parent = parent_hwnd(&dialog).0 as isize;
        dialog.close().await;
        let shown = detail.clone();
        tokio::task::spawn_blocking(move || {
            show_wv2_error(Some(&shown), HWND(parent as *mut _));
        })
        .await
        .ok();
        let mut coded = Coded::bare(WEBVIEW2_FAILED);
        coded.detail = Some(detail);
        Err(anyhow::Error::from(coded))
    };

    let wv2_url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let res = REQUEST_CLIENT
        .get(wv2_url)
        .send()
        .await
        .with_http_context("install_webview2", wv2_url);
    let res = match res {
        Ok(res) => res,
        Err(e) => return fail(dialog, format!("{e:#}")).await,
    };
    let wv2_installer_blob = res
        .bytes()
        .await
        .with_http_context("install_webview2", wv2_url);
    let wv2_installer_blob = match wv2_installer_blob {
        Ok(bytes) => bytes,
        Err(e) => return fail(dialog, format!("{e:#}")).await,
    };

    let installer_path = std::env::temp_dir().join("kachina.MicrosoftEdgeWebview2Setup.exe");
    if let Err(e) = tokio::fs::write(&installer_path, wv2_installer_blob).await {
        return fail(dialog, format!("{e}")).await;
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
        Ok(status) => fail(dialog, format!("{status}")).await,
        Err(e) => fail(dialog, format!("{e}")).await,
    }
}
