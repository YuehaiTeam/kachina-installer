use anyhow::{Context, Result};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::fs::{create_http_stream, create_local_stream, create_target_file, progressed_copy};

pub async fn install_runtime(
    tag: String,
    offset: Option<usize>,
    size: Option<usize>,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<String> {
    // if tag startswith Microsoft.DotNet, install .NET runtime
    if tag.starts_with("Microsoft.DotNet") {
        return install_dotnet(tag, offset, size, notify).await;
    }
    if tag.starts_with("Microsoft.VCRedist") {
        return install_vcredist(tag, offset, size, notify).await;
    }
    // else not supported
    Err(anyhow::anyhow!("UNSUPPORTED_RUNTIME"))
}

/*
 * Install .NET runtime package
 * Supported tags:
 * Microsoft.DotNet.DesktopRuntime.*
 * Microsoft.DotNet.Runtime.*
 * * may be number '8' or '8.0.1'
 */
pub async fn install_dotnet(
    tag: String,
    offset: Option<usize>,
    size: Option<usize>,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<String> {
    let tag_without_version = tag.split('.').take(3).collect::<Vec<&str>>().join(".");
    pub struct PackageLinks {
        pub latest_link_template: &'static str,
        pub versioned_link_template: &'static str,
        pub tag: &'static str,
    }
    let runtime = match tag_without_version.as_str() {
        "Microsoft.DotNet.DesktopRuntime" => PackageLinks {
            latest_link_template: "https://aka.ms/dotnet/$/windowsdesktop-runtime-win-x64.exe",
            versioned_link_template: "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/$/windowsdesktop-runtime-$-win-x64.exe",
            tag: "Microsoft.WindowsDesktop.App",
        },
        "Microsoft.DotNet.Runtime" => PackageLinks {
            latest_link_template: "https://aka.ms/dotnet/$/dotnet-runtime-win-x64.exe",
            versioned_link_template: "https://builds.dotnet.microsoft.com/dotnet/Runtime/$/dotnet-runtime-$-win-x64.exe",
            tag: "Microsoft.NETCore.App",
        },
        _ => {
            return Err(anyhow::anyhow!("UNSUPPORTED_DOTNET_RUNTIME"));
        }
    };
    // check if runtime is installed by running dotnet --list-runtimes
    let cmd = tokio::process::Command::new("dotnet")
        .arg("--list-runtimes")
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .await;
    // if installed, continue; if check failed, return error
    if let Ok(output) = cmd {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let version_primary = tag
                .split('.')
                .nth(3)
                .ok_or_else(|| anyhow::anyhow!("INVALID_DOTNET_VERSION"))?;
            let query_name = format!("{} {}", runtime.tag, version_primary);
            if stdout.contains(&query_name) {
                return Ok("ALREADY_INSTALLED".to_string());
            }
        }
    }
    // download to tmp folder
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir
        .as_path()
        .join(format!("Kachina.RuntimePackage.{tag}.exe"));
    let mut target = create_target_file(installer_path.as_os_str().to_str().unwrap())
        .await
        .context("CREATE_TARGET_FILE_ERR")?;
    let (mut stream, len) = if offset.is_some() || size.is_some() {
        // runtime packed, just extract and run
        let stream = create_local_stream(offset.unwrap(), size.unwrap(), true)
            .await
            .context("RUNTIME_EXTRACT_ERR")?;
        tracing::info!(
            "Extracted {} installer from local stream, offset: {}, size: {}",
            tag,
            offset.unwrap(),
            size.unwrap()
        );
        (stream, size.unwrap())
    } else {
        let mut vernum = tag.split('.').skip(3).collect::<Vec<&str>>().join(".");
        let url;
        if vernum.len() == 1 {
            vernum = format!("{vernum}.0");
        }

        if vernum.len() == 2 {
            // Download the latest version
            url = runtime.latest_link_template.replace("$", &vernum);
        } else {
            // Download a specific version
            url = runtime.versioned_link_template.replace("$", &vernum);
        }
        let (stream, len, _insight) = create_http_stream(&url, 0, 0, true)
            .await
            .context("RUNTIME_DOWNLOAD_ERR")?;
        (stream, len.try_into().unwrap_or(0))
    };
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!((downloaded, len)));
    };
    progressed_copy(&mut stream, &mut target, progress_noti).await?;
    // close streams
    drop(stream);
    drop(target);
    // run installer with /passive /norestart
    let mut cmd = tokio::process::Command::new(&installer_path)
        .arg("/passive")
        .arg("/norestart")
        .spawn()
        .context("RUNTIME_INSTALL_START_ERR")?;
    let status = cmd.wait().await.context("RUNTIME_INSTALL_WAIT_ERR")?;
    if !status.success() {
        return Err(anyhow::anyhow!("RUNTIME_INSTALL_FAILED"));
    }
    // remove installer
    let _ = tokio::fs::remove_file(&installer_path).await;
    Ok("NEWLY_INSTALLED".to_string())
}

pub fn check_vcredist(reg: &str) -> bool {
    let key = windows_registry::LOCAL_MACHINE.options().read().open(reg);
    if let Ok(key) = key {
        let installed = key.get_u32("Installed");
        if let Ok(installed) = installed {
            if installed == 1 {
                return true;
            }
        }
    }
    false
}

pub async fn install_vcredist(
    tag: String,
    offset: Option<usize>,
    size: Option<usize>,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<String> {
    let x64_prefix = "SOFTWARE\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\";
    let x86_prefix = "SOFTWARE\\Wow6432Node\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\";
    let (url, reg) = match tag.as_str() {
        "Microsoft.VCRedist.2015+.x64" => (
            "https://aka.ms/vc14/vc_redist.x64.exe",
            format!("{}{}", x64_prefix, "x64"),
        ),
        "Microsoft.VCRedist.2015+.x86" => (
            "https://aka.ms/vc14/vc_redist.x86.exe",
            format!("{}{}", x86_prefix, "x86"),
        ),
        _ => {
            return Err(anyhow::anyhow!("UNSUPPORTED_TAG"));
        }
    };
    // check registry for already installed
    if check_vcredist(&reg) {
        return Ok("ALREADY_INSTALLED".to_string());
    }
    // download to tmp folder
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir
        .as_path()
        .join(format!("Kachina.RuntimePackage.{tag}.exe"));
    let (mut stream, len) = if offset.is_some() || size.is_some() {
        // runtime packed, just extract and run
        let stream = create_local_stream(offset.unwrap(), size.unwrap(), true)
            .await
            .context("RUNTIME_EXTRACT_ERR")?;
        tracing::info!(
            "Extracted {} installer from local stream, offset: {}, size: {}",
            tag,
            offset.unwrap(),
            size.unwrap()
        );
        (stream, size.unwrap())
    } else {
        let (stream, len, _insight) = create_http_stream(url, 0, 0, true)
            .await
            .context("RUNTIME_DOWNLOAD_ERR")?;
        (stream, len.try_into().unwrap_or(0))
    };
    let mut target = create_target_file(installer_path.as_os_str().to_str().unwrap())
        .await
        .context("CREATE_TARGET_FILE_ERR")?;
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!((downloaded, len)));
    };
    progressed_copy(&mut stream, &mut target, progress_noti).await?;
    // close streams
    drop(stream);
    drop(target);
    let mut cmd = tokio::process::Command::new(&installer_path)
        .arg("/install")
        .arg("/quiet")
        .arg("/norestart")
        .spawn()
        .context("RUNTIME_INSTALL_START_ERR")?;
    let status = cmd.wait().await.context("RUNTIME_INSTALL_WAIT_ERR")?;
    if !status.success() {
        return Err(anyhow::anyhow!("RUNTIME_INSTALL_FAILED"));
    }
    let _ = tokio::fs::remove_file(installer_path).await;
    Ok("NEWLY_INSTALLED".to_string())
}
