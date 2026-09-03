use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::fs::{create_http_stream, create_local_stream, create_staged_file, progressed_copy};
use crate::ipc::{Progress, ProgressNotify};
use crate::utils::process;

/// 32 位注册表视图下 .NET 安装器写入的键。本二进制是 x64 进程，直接走 WOW6432Node。
const DOTNET_INSTALLED_VERSIONS: &str = r"SOFTWARE\WOW6432Node\dotnet\Setup\InstalledVersions\x64";

/// apphost 查找 .NET 根目录的顺序：`DOTNET_ROOT`、注册表 `InstallLocation`、`%ProgramFiles%\dotnet`。
/// apphost 只取第一个存在的；这里全部收集，任一处装有合适版本即视为已安装。
fn dotnet_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("DOTNET_ROOT") {
        roots.push(PathBuf::from(root));
    }
    if let Ok(key) = windows_registry::LOCAL_MACHINE
        .options()
        .read()
        .open(DOTNET_INSTALLED_VERSIONS)
    {
        if let Ok(location) = key.get_string("InstallLocation") {
            roots.push(PathBuf::from(location));
        }
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        roots.push(PathBuf::from(pf).join("dotnet"));
    }
    roots
}

/// `framework` 为 `Microsoft.WindowsDesktop.App` 等共享框架名，`major` 为主版本号。
/// 目录枚举 `<root>\shared\<framework>\<version>` 与注册表 `sharedfx\<framework>` 的值名任一命中即通过。
pub fn dotnet_runtime_installed(framework: &str, major: &str) -> bool {
    let prefix = format!("{major}.");
    let matches = |name: &str| name.starts_with(&prefix);
    for root in dotnet_roots() {
        let Ok(entries) = std::fs::read_dir(root.join("shared").join(framework)) else {
            continue;
        };
        let found = entries.flatten().any(|e| {
            e.file_type().is_ok_and(|t| t.is_dir()) && e.file_name().to_str().is_some_and(matches)
        });
        if found {
            return true;
        }
    }
    if let Ok(key) = windows_registry::LOCAL_MACHINE
        .options()
        .read()
        .open(format!(r"{DOTNET_INSTALLED_VERSIONS}\sharedfx\{framework}"))
    {
        if let Ok(mut values) = key.values() {
            if values.any(|(name, _)| matches(&name)) {
                return true;
            }
        }
    }
    false
}

/// `dl_dir` is the staging `dl\` directory the installer package is downloaded
/// into; it goes away with the staging directory.
pub async fn install_runtime(
    tag: String,
    offset: Option<usize>,
    size: Option<usize>,
    dl_dir: String,
    notify: ProgressNotify,
) -> Result<String> {
    let dl_dir = PathBuf::from(dl_dir);
    // if tag startswith Microsoft.DotNet, install .NET runtime
    if tag.starts_with("Microsoft.DotNet") {
        return install_dotnet(tag, offset, size, &dl_dir, notify).await;
    }
    if tag.starts_with("Microsoft.VCRedist") {
        return install_vcredist(tag, offset, size, &dl_dir, notify).await;
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
    dl_dir: &std::path::Path,
    notify: ProgressNotify,
) -> Result<String> {
    let tag_without_version = tag.split('.').take(3).collect::<Vec<&str>>().join(".");
    let runtime = match tag_without_version.as_str() {
        "Microsoft.DotNet.DesktopRuntime" => (
            "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/$/latest.version",
            "https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/$/windowsdesktop-runtime-$-win-x64.exe",
            "Microsoft.WindowsDesktop.App",
        ),
        "Microsoft.DotNet.Runtime" => (
            "https://builds.dotnet.microsoft.com/dotnet/Runtime/$/latest.version",
            "https://builds.dotnet.microsoft.com/dotnet/Runtime/$/dotnet-runtime-$-win-x64.exe",
            "Microsoft.NETCore.App",
        ),
        _ => {
            return Err(anyhow::anyhow!("UNSUPPORTED_DOTNET_RUNTIME"));
        }
    };
    let version_primary = tag
        .split('.')
        .nth(3)
        .ok_or_else(|| anyhow::anyhow!("INVALID_DOTNET_VERSION"))?;
    if dotnet_runtime_installed(runtime.2, version_primary) {
        return Ok("ALREADY_INSTALLED".to_string());
    }
    let installer_path = dl_dir.join(format!("Kachina.RuntimePackage.{tag}.exe"));
    let mut target = create_staged_file(&installer_path)
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
        // if vernum is release version, get real version
        if vernum.len() == 1 || vernum.len() == 2 {
            let relver = if vernum.len() == 1 {
                format!("{vernum}.0")
            } else {
                vernum.clone()
            };
            let url = runtime.0.replace("$", &relver);
            let resp = reqwest::get(&url)
                .await
                .context("RUNTIME_VERSION_FETCH_ERR")?;
            if !resp.status().is_success() {
                return Err(anyhow::anyhow!("RUNTIME_VERSION_API_ERR"));
            }
            let text = resp.text().await.context("RUNTIME_VERSION_READ_ERR")?;
            vernum = text.trim().to_string();
        }
        // get real download url
        let url = runtime.1.replace("$", &vernum);
        let (stream, len, _insight) = create_http_stream(&url, 0, 0, true, None)
            .await
            .map_err(|e| e.error)
            .context("RUNTIME_DOWNLOAD_ERR")?;
        (stream, len.try_into().unwrap_or(0))
    };
    let progress_noti = move |downloaded: usize| {
        notify(Progress::BytesOf {
            done: downloaded as u64,
            total: len as u64,
        });
    };
    progressed_copy(stream.as_mut(), &mut target, &progress_noti).await?;
    // close streams
    drop(stream);
    drop(target);
    let child = process::spawn(&installer_path, &["/passive", "/norestart"], false)
        .context("RUNTIME_INSTALL_START_ERR")?;
    let code = child.wait().await.context("RUNTIME_INSTALL_WAIT_ERR")?;
    if code != 0 {
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
    dl_dir: &std::path::Path,
    notify: ProgressNotify,
) -> Result<String> {
    let x64_prefix = "SOFTWARE\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\";
    let x86_prefix = "SOFTWARE\\Wow6432Node\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\";
    let (url, reg) = match tag.as_str() {
        "Microsoft.VCRedist.2015+.x64" => (
            "https://aka.ms/vs/17/release/vc_redist.x64.exe",
            format!("{}{}", x64_prefix, "x64"),
        ),
        "Microsoft.VCRedist.2015+.x86" => (
            "https://aka.ms/vs/17/release/vc_redist.x86.exe",
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
    let installer_path = dl_dir.join(format!("Kachina.RuntimePackage.{tag}.exe"));
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
        let (stream, len, _insight) = create_http_stream(url, 0, 0, true, None)
            .await
            .map_err(|e| e.error)
            .context("RUNTIME_DOWNLOAD_ERR")?;
        (stream, len.try_into().unwrap_or(0))
    };
    let mut target = create_staged_file(&installer_path)
        .await
        .context("CREATE_TARGET_FILE_ERR")?;
    let progress_noti = move |downloaded: usize| {
        notify(Progress::BytesOf {
            done: downloaded as u64,
            total: len as u64,
        });
    };
    progressed_copy(stream.as_mut(), &mut target, &progress_noti).await?;
    // close streams
    drop(stream);
    drop(target);
    let child = process::spawn(
        &installer_path,
        &["/install", "/quiet", "/norestart"],
        false,
    )
    .context("RUNTIME_INSTALL_START_ERR")?;
    let code = child.wait().await.context("RUNTIME_INSTALL_WAIT_ERR")?;
    if code != 0 {
        return Err(anyhow::anyhow!("RUNTIME_INSTALL_FAILED"));
    }
    let _ = tokio::fs::remove_file(installer_path).await;
    Ok("NEWLY_INSTALLED".to_string())
}
