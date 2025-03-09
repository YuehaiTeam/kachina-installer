use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::fs::{create_http_stream, create_target_file, progressed_copy};

pub async fn install_runtime(
    tag: String,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<String, String> {
    // if tag startswith Microsoft.DotNet, install .NET runtime
    if tag.starts_with("Microsoft.DotNet") {
        return install_dotnet(tag, notify).await;
    }
    // else not supported
    Err(format!("Unsupported runtime tag: {}", tag))
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
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<String, String> {
    let tag_without_version = tag.split('.').take(3).collect::<Vec<&str>>().join(".");
    let runtime = match tag_without_version.as_str() {
        "Microsoft.DotNet.DesktopRuntime" => (
            "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/$/latest.version",
            "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/$/windowsdesktop-runtime-$-win-x64.exe",
            "Microsoft.WindowsDesktop.App"
        ),
        "Microsoft.DotNet.Runtime" => (
            "https://builds.dotnet.microsoft.com/dotnet/Runtime/$/latest.version",
            "https://builds.dotnet.microsoft.com/dotnet/Runtime/$/dotnet-runtime-$-win-x64.exe",
            "Microsoft.NETCore.App"
        ),
        _ => {
            return Err(format!("Unsupported tag: {}", tag));
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
            let version_primary = tag.split('.').nth(3);
            if version_primary.is_none() {
                return Err(format!("Unsupported dotnet runtime tag: {}", tag));
            }
            let version_primary = version_primary.unwrap();
            let query_name = format!("{} {}", runtime.2, version_primary);
            if stdout.contains(&query_name) {
                return Ok("ALREADY_INSTALLED".to_string());
            }
        }
    }
    let mut vernum = tag.split('.').skip(3).collect::<Vec<&str>>().join(".");
    // if vernum is release version, get real version
    if vernum.len() == 1 || vernum.len() == 2 {
        let relver = if vernum.len() == 1 {
            format!("{}.0", vernum)
        } else {
            vernum.clone()
        };
        let url = runtime.0.replace("$", &relver);
        let resp = reqwest::get(&url).await;
        if let Err(e) = resp {
            return Err(format!("Failed to get dotnet version: {:?}", e));
        }
        let resp = resp.unwrap();
        if !resp.status().is_success() {
            return Err(format!("Failed to get dotnet version: {:?}", resp.status()));
        }
        let text = resp.text().await;
        if let Err(e) = text {
            return Err(format!("Failed to get dotnet version: {:?}", e));
        }
        let text = text.unwrap();
        vernum = text.trim().to_string();
    }
    // get real download url
    let url = runtime.1.replace("$", &vernum);
    // download to tmp folder
    let temp_dir = std::env::temp_dir();
    let installer_path = temp_dir
        .as_path()
        .join(format!("Kachina.RuntimePackage.{}.exe", tag));
    let (mut stream, len) = create_http_stream(&url, 0, 0, true)
        .await
        .map_err(|e| format!("Failed to download dotnet runtime installer: {:?}", e))?;
    let mut target = create_target_file(installer_path.as_os_str().to_str().unwrap())
        .await
        .map_err(|e| format!("Failed to create dotnet runtime installer: {:?}", e))?;
    let progress_noti = move |downloaded: usize| {
        notify(serde_json::json!((downloaded, len)));
    };
    progressed_copy(&mut stream, &mut target, progress_noti).await?;
    // close streams
    drop(stream);
    drop(target);
    // run installer with /passive /norestart
    let cmd = tokio::process::Command::new(&installer_path)
        .arg("/passive")
        .arg("/norestart")
        .spawn();
    if let Err(e) = cmd {
        return Err(format!("Failed to run dotnet runtime installer: {:?}", e));
    }
    let mut cmd = cmd.unwrap();
    let status = cmd.wait().await;
    if let Err(e) = status {
        return Err(format!("Failed to wait dotnet runtime installer: {:?}", e));
    }
    let status = status.unwrap();
    if !status.success() {
        return Err(format!("Failed to install dotnet runtime: {:?}", status));
    }
    // remove installer
    let _ = tokio::fs::remove_file(&installer_path).await;
    Ok("NEWLY_INSTALLED".to_string())
}
