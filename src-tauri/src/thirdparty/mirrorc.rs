use std::io::Read;
use std::path::Path;

use anyhow::Context;
use sha2::Digest;

use crate::{
    fs::{create_http_stream, create_staged_file, progressed_copy},
    ipc::{Progress, ProgressNotify},
    utils::{
        code::{Attach, Coded, MIRRORC_FAILED},
        error::{return_ta_result, IntoTAResult, TAResult},
        metadata::RepoMetadata,
        url::HttpContextExt,
    },
};

pub static MIRRORC_CRED_PREFIX: &str = "KachinaInstaller_MirrorChyanCDK_";

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct MirrorcChangeset {
    pub added: Option<Vec<String>>,
    pub deleted: Option<Vec<String>>,
    pub modified: Option<Vec<String>>,
}

/// What phase one produced from a Mirror酱 archive: every extracted file with
/// its md5 (relative path, `/`), the archive's delete list, and the
/// `.metadata.json` text for the session to parse.
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default)]
pub struct MirrorcExtract {
    pub metadata: Option<String>,
    pub files: Vec<(String, String)>,
    pub deletes: Vec<String>,
}

/// Verify the archive digest, then extract into the staging `new\` directory.
/// The install directory is never touched here.
pub async fn run_mirrorc_install(
    zip_path: &str,
    new_dir: &str,
    sha256: &str,
    notify: ProgressNotify,
) -> TAResult<MirrorcExtract> {
    let zip_path = zip_path.to_string();
    let new_dir = new_dir.to_string();
    let sha256 = sha256.to_string();
    tokio::task::spawn_blocking(move || {
        run_mirrorc_install_sync(&zip_path, &new_dir, &sha256, notify)
    })
    .await
    .into_ta_result()?
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path).context("OPEN_TARGET_ERR")?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).context("READ_FILE_ERR")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn run_mirrorc_install_sync(
    zip_path: &str,
    new_dir: &str,
    sha256: &str,
    notify: ProgressNotify,
) -> TAResult<MirrorcExtract> {
    if !sha256.is_empty() {
        let got = sha256_file(Path::new(zip_path))?;
        if !got.eq_ignore_ascii_case(sha256) {
            let _ = std::fs::remove_file(zip_path);
            return Err(anyhow::anyhow!("archive sha256 mismatch: expected {sha256}, got {got}")
                .attach(MIRRORC_FAILED)
                .into());
        }
    }
    let file = std::fs::File::open(zip_path).into_ta_result()?;
    let mut archive = zip::ZipArchive::new(file).into_ta_result()?;
    let total_len = archive.len();

    let file_lists = archive
        .file_names()
        .map(|s| s.to_string())
        .filter(|s| s != "changes.json" && s != ".metadata.json")
        .collect::<Vec<String>>();
    let prefix = longest_common_prefix(file_lists);
    // split last '/', get the prefix
    let mut prefix = prefix.split('/').collect::<Vec<&str>>();
    prefix.pop();
    let mut prefix = prefix.join("/");
    if !prefix.is_empty() && !prefix.ends_with('/') {
        prefix.push('/');
    }

    // changes.json
    let changeset: Option<MirrorcChangeset> = match archive.by_name("changes.json") {
        Ok(mut changeset) => {
            let mut changeset_str = String::new();
            changeset
                .read_to_string(&mut changeset_str)
                .into_ta_result()?;
            Some(serde_json::from_str(&changeset_str).into_ta_result()?)
        }
        Err(_) => None,
    };

    // .metadata.json：本地只需要 deletes，原文交回会话侧解析
    let metadata_str: Option<String> = match archive.by_name(&format!("{prefix}.metadata.json")) {
        Ok(mut metadata) => {
            let mut metadata_str = String::new();
            metadata
                .read_to_string(&mut metadata_str)
                .into_ta_result()?;
            Some(metadata_str)
        }
        Err(_) => None,
    };
    let metadata: Option<RepoMetadata> = match metadata_str.as_deref() {
        Some(text) => Some(serde_json::from_str(text).into_ta_result()?),
        None => None,
    };

    // if both changeset and metadata are None, return error
    if changeset.is_none() && metadata.is_none() {
        return Err(anyhow::Error::from(Coded::bare(MIRRORC_FAILED))
            .context("Not a valid mirrorc archive: neither changes.json nor .metadata.json found")
            .into());
    }

    let mut files = Vec::new();
    for i in 0..total_len {
        let mut file = archive.by_index(i).into_ta_result()?;
        let file_name = file
            .name()
            .strip_prefix(&prefix)
            .unwrap_or(file.name())
            .to_string();
        if file_name == "changes.json"
            || file_name == ".metadata.json"
            || file_name == format!("{prefix}.metadata.json")
        {
            continue;
        }
        if file.is_dir() {
            continue;
        }
        if !crate::fs::staging::is_safe_rel(&file_name) {
            return Err(anyhow::Error::from(Coded::bare(MIRRORC_FAILED))
                .context("unsafe archive path")
                .into());
        }
        let out_path = crate::fs::staging::join_rel(Path::new(new_dir), &file_name);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .into_ta_result()
                .context("CREATE_DIR_ERR")?;
        }
        let mut out_file = std::fs::File::create(&out_path)
            .into_ta_result()
            .context(format!("CREATE_FILE_ERR: {}", out_path.display()))?;
        let mut hasher = chksum_md5::MD5::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .into_ta_result()
                .context(format!("READ_ENTRY_ERR: {}", out_path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            std::io::Write::write_all(&mut out_file, &buf[..n])
                .into_ta_result()
                .context(format!("WRITE_FILE_ERR: {}", out_path.display()))?;
        }
        out_file.sync_all().into_ta_result()?;
        files.push((file_name.replace('\\', "/"), hasher.digest().to_hex_lowercase()));
        notify(Progress::Extract {
            file: file_name,
            done: (i + 1) as u64,
            total: total_len as u64,
        });
    }

    let mut deletes: Vec<String> = Vec::new();
    if let Some(changeset) = changeset.as_ref() {
        if let Some(deleted) = changeset.deleted.as_ref() {
            for file in deleted {
                let strip_path = file.strip_prefix(&prefix).unwrap_or(file);
                if !crate::fs::staging::is_safe_rel(strip_path) {
                    return Err(anyhow::Error::from(Coded::bare(MIRRORC_FAILED))
                        .context("unsafe delete path")
                        .into());
                }
                deletes.push(strip_path.replace('\\', "/"));
            }
        }
    }
    if let Some(metadata) = metadata.as_ref() {
        for file in &metadata.deletes {
            if !crate::fs::staging::is_safe_rel(file) {
                return Err(anyhow::Error::from(Coded::bare(MIRRORC_FAILED))
                    .context("unsafe delete path")
                    .into());
            }
            deletes.push(file.replace('\\', "/"));
        }
    }
    deletes.sort();
    deletes.dedup();
    let _ = std::fs::remove_file(zip_path);
    Ok(MirrorcExtract {
        metadata: metadata_str,
        files,
        deletes,
    })
}

pub async fn get_mirrorc_status(
    resource_id: &str,
    current_version: &str,
    cdk: &str,
    channel: &str,
    arch: Option<&str>,
    os: Option<&str>,
) -> TAResult<serde_json::Value> {
    if resource_id.is_empty() || channel.is_empty() {
        return return_ta_result(
            "Invalid parameters for get_mirrorc_status: rid or channel is empty".to_string(),
            "MIRRORC_INVALID_PARAMS",
        );
    }
    let mut opts = String::new();
    if let Some(arch) = arch {
        opts.push_str(&format!("&arch={arch}"));
    }
    if let Some(os) = os {
        opts.push_str(&format!("&os={os}"));
    }
    let mirrorc_url = format!("https://mirrorchyan.com/api/resources/{resource_id}/latest?current_version={current_version}&cdk={cdk}&channel={channel}{opts}&user_agent=KachinaInstaller");
    let resp = crate::REQUEST_CLIENT
        .get(&mirrorc_url)
        .send()
        .await
        .with_http_context("get_mirrorc_status", &mirrorc_url)?;

    let body_text = resp
        .text()
        .await
        .with_http_context("get_mirrorc_status", &mirrorc_url)?;
    let status: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON ({}): {}", e, body_text))?;
    Ok(status)
}

/// Download the archive to `zip_path` (under the staging `dl\` directory).
pub async fn run_mirrorc_download(
    zip_path: &str,
    url: &str,
    notify: ProgressNotify,
) -> TAResult<()> {
    let (mut stream, len, _insight) = create_http_stream(url, 0, 0, true, None).await?;
    let mut target = create_staged_file(Path::new(zip_path)).await?;
    let on_progress = |downloaded| {
        notify(Progress::BytesOf {
            done: downloaded as u64,
            total: len as u64,
        });
    };
    progressed_copy(stream.as_mut(), &mut target, &on_progress)
        .await
        .context("MIRRORC_DOWNLOAD_ERR")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::progress_notify;
    use std::io::Write;

    fn tmp() -> std::path::PathBuf {
        let dir = crate::fs::staging::scratch_file(&format!("kachina-mirrorc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn extracts_into_new_dir_and_reports_deletes() {
        let dir = tmp();
        let zip_path = dir.join("pkg.zip");
        let exe_name = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let exe_entry = format!("root/{exe_name}");
        make_zip(
            &zip_path,
            &[
                ("changes.json", br#"{"deleted":["root/old.dll"]}"#),
                ("root/app.exe", b"app"),
                ("root/sub/lib.dll", b"lib"),
                (exe_entry.as_str(), b"self"),
            ],
        );
        let sha = sha256_file(&zip_path).unwrap();
        let new_dir = dir.join("new");
        let install = dir.join("install");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("app.exe"), b"before").unwrap();
        let out = run_mirrorc_install_sync(
            &zip_path.to_string_lossy(),
            &new_dir.to_string_lossy(),
            &sha,
            progress_notify(|_| {}),
        )
        .unwrap();
        assert_eq!(std::fs::read(new_dir.join("app.exe")).unwrap(), b"app");
        assert_eq!(std::fs::read(new_dir.join("sub").join("lib.dll")).unwrap(), b"lib");
        assert_eq!(std::fs::read(new_dir.join(&exe_name)).unwrap(), b"self");
        assert_eq!(std::fs::read(install.join("app.exe")).unwrap(), b"before");
        assert_eq!(out.deletes, vec!["old.dll".to_string()]);
        assert!(out.metadata.is_none());
        let app = out.files.iter().find(|(r, _)| r == "app.exe").unwrap();
        assert_eq!(app.1, chksum_md5::hash(b"app").to_hex_lowercase());
        assert!(!zip_path.exists(), "archive removed after extraction");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_variant_and_invalid_archives() {
        let dir = tmp();
        let zip_path = dir.join("pkg.zip");
        make_zip(
            &zip_path,
            &[
                (".metadata.json", br#"{"tag_name":"v2","hashed":[],"deletes":["x.dll"]}"#),
                ("a.txt", b"a"),
            ],
        );
        let sha = sha256_file(&zip_path).unwrap();
        let new_dir = dir.join("new");
        let out = run_mirrorc_install_sync(
            &zip_path.to_string_lossy(),
            &new_dir.to_string_lossy(),
            &sha,
            progress_notify(|_| {}),
        )
        .unwrap();
        assert_eq!(out.deletes, vec!["x.dll".to_string()]);
        assert!(out.metadata.as_deref().unwrap().contains("v2"));

        // digest mismatch: nothing extracted
        make_zip(&zip_path, &[("a.txt", b"a")]);
        let new2 = dir.join("new2");
        let err = run_mirrorc_install_sync(
            &zip_path.to_string_lossy(),
            &new2.to_string_lossy(),
            "00",
            progress_notify(|_| {}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("sha256"));
        assert!(!new2.exists() || std::fs::read_dir(&new2).unwrap().next().is_none());

        // neither changes.json nor .metadata.json
        make_zip(&zip_path, &[("a.txt", b"a")]);
        let sha = sha256_file(&zip_path).unwrap();
        assert!(run_mirrorc_install_sync(
            &zip_path.to_string_lossy(),
            &new2.to_string_lossy(),
            &sha,
            progress_notify(|_| {}),
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub fn longest_common_prefix(strs: Vec<String>) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let mut prefix = strs[0].clone();
    for s in strs.iter() {
        while !s.starts_with(&prefix) {
            if prefix.is_empty() {
                return String::new();
            }
            prefix.pop();
        }
    }
    prefix
}
