use std::io::Read;

use anyhow::Context;

use crate::{
    fs::{create_http_stream, create_target_file, prepare_target, progressed_copy},
    installer::uninstall::DELETE_SELF_ON_EXIT_PATH,
    utils::{
        error::{return_ta_result, IntoTAResult, TAResult},
        metadata::RepoMetadata,
    },
};

pub static MIRRORC_CRED_PREFIX: &str = "KachinaInstaller_MirrorChyanCDK_";

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct MirrorcChangeset {
    pub added: Option<Vec<String>>,
    pub deleted: Option<Vec<String>>,
    pub modified: Option<Vec<String>>,
}

pub async fn run_mirrorc_install(
    zip_path: &str,
    target_path: &str,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> TAResult<(Option<RepoMetadata>, Option<MirrorcChangeset>)> {
    let zip_path = zip_path.to_string();
    let target_path = target_path.to_string();
    tokio::task::spawn_blocking(move || run_mirrorc_install_sync(&zip_path, &target_path, notify))
        .await
        .into_ta_result()?
}

pub fn run_mirrorc_install_sync(
    zip_path: &str,
    target_path: &str,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> TAResult<(Option<RepoMetadata>, Option<MirrorcChangeset>)> {
    let file = std::fs::File::open(zip_path).into_ta_result()?;
    let mut archive = zip::ZipArchive::new(file).into_ta_result()?;
    let total_len = archive.len() - 1;

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

    // .metadata.json
    let metadata: Option<RepoMetadata> = match archive.by_name(&format!("{prefix}.metadata.json")) {
        Ok(mut metadata) => {
            let mut metadata_str = String::new();
            metadata
                .read_to_string(&mut metadata_str)
                .into_ta_result()?;
            Some(serde_json::from_str(&metadata_str).into_ta_result()?)
        }
        Err(_) => None,
    };

    // if both changeset and metadata are None, return error
    if changeset.is_none() && metadata.is_none() {
        return return_ta_result(
            "Not a valid mirrorc archive: neither changes.json nor .metadata.json found"
                .to_string(),
            "MIRRORC_ARCHIVE_ERR",
        );
    }

    let current_exe = std::env::current_exe().context("GET_EXE_PATH_ERR")?;

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
        let mut out_path = std::path::PathBuf::from(target_path);
        out_path.push(file_name.clone());
        if file.is_dir() {
            continue;
        }
        if out_path == current_exe {
            // delete .instbak if exists
            let instbak = out_path.clone().with_extension("instbak");
            if instbak.exists() {
                std::fs::remove_file(&instbak)
                    .into_ta_result()
                    .context("SELF_UPDATE_ERR")?;
            }
            // mv current exe to .instbak
            std::fs::rename(&current_exe, &instbak)
                .into_ta_result()
                .context("SELF_UPDATE_ERR")?;
            DELETE_SELF_ON_EXIT_PATH
                .write()
                .unwrap()
                .replace(instbak.to_string_lossy().to_string());
        }
        let parent = out_path.parent();
        if let Some(parent) = parent {
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .into_ta_result()
                    .context("CREATE_DIR_ERR")?;
            }
        }
        let mut out_file = std::fs::File::create(&out_path)
            .into_ta_result()
            .context(format!("CREATE_FILE_ERR: {}", out_path.display()))?;
        std::io::copy(&mut file, &mut out_file)
            .into_ta_result()
            .context(format!("WRITE_FILE_ERR: {}", out_path.display()))?;
        notify(
            serde_json::json!({"type": "extract", "file": file_name, "count": i, "total": total_len}),
        );
    }

    // delete files in target_path that are not in the changeset
    if let Some(changeset) = changeset.as_ref() {
        if let Some(deletes) = changeset.deleted.as_ref() {
            for file in deletes {
                let mut out_path = std::path::PathBuf::from(target_path);
                let strip_path = file.strip_prefix(&prefix).unwrap_or(file);
                out_path.push(strip_path);
                if out_path.exists() {
                    std::fs::remove_file(out_path).into_ta_result()?;
                    notify(serde_json::json!({"type": "delete", "file": strip_path}));
                }
            }
        }
    }
    if let Some(metadata) = metadata.as_ref() {
        // delete files in target_path that are not in the metadata
        if let Some(deletes) = metadata.deletes.as_ref() {
            for file in deletes {
                let mut out_path = std::path::PathBuf::from(target_path);
                out_path.push(file.clone());
                if out_path.exists() {
                    std::fs::remove_file(out_path).into_ta_result()?;
                    notify(serde_json::json!({"type": "delete", "file": file}));
                }
            }
        }
    }
    // delete zip file
    let _ = std::fs::remove_file(zip_path);
    Ok((metadata, changeset))
}

#[tauri::command]
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
        .context("MIRRORC_HTTP_ERR")?;
    let status: serde_json::Value = resp.json().await.into_ta_result()?;
    Ok(status)
}

pub async fn run_mirrorc_download(
    zip_path: &str,
    url: &str,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> TAResult<()> {
    let (stream, len) = create_http_stream(url, 0, 0, true).await?;
    prepare_target(zip_path).await?;
    let target = create_target_file(zip_path).await?;
    progressed_copy(stream, target, |downloaded| {
        notify(serde_json::json!({"type": "download", "downloaded": downloaded, "total": len}));
    })
    .await
    .context("MIRRORC_DOWNLOAD_ERR")?;
    Ok(())
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
