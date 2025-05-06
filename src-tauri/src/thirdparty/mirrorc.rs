use std::io::Read;

use crate::utils::metadata::RepoMetadata;

#[derive(serde::Deserialize, Debug)]
pub struct MirrorcChangeset {
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    pub modified: Vec<String>,
}
pub fn run_mirrorc_install(
    zip_path: &str,
    target_path: &str,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<(Option<RepoMetadata>, Option<MirrorcChangeset>), anyhow::Error> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let total_len = archive.len() - 1;

    // changes.json
    let changeset: Option<MirrorcChangeset> = match archive.by_name("changes.json") {
        Ok(mut changeset) => {
            let mut changeset_str = String::new();
            changeset.read_to_string(&mut changeset_str)?;
            Some(serde_json::from_str(&changeset_str)?)
        }
        Err(_) => None,
    };

    // .metadata.json
    let metadata: Option<RepoMetadata> = match archive.by_name(".metadata.json") {
        Ok(mut metadata) => {
            let mut metadata_str = String::new();
            metadata.read_to_string(&mut metadata_str)?;
            Some(serde_json::from_str(&metadata_str)?)
        }
        Err(_) => None,
    };

    // if both changeset and metadata are None, return error
    if changeset.is_none() && metadata.is_none() {
        return Err(anyhow::anyhow!(
            "Not a valid mirrorc archive: neither changes.json nor .metadata.json found"
        ));
    }

    let len = total_len - 1;
    for i in 0..len {
        let mut file = archive.by_index(i)?;
        let file_name = file.name().to_string();
        if file_name == "changes.json" {
            continue;
        }
        let mut out_path = std::path::PathBuf::from(target_path);
        out_path.push(file_name.clone());
        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut file, &mut out_file)?;
        notify(serde_json::json!({"type": "extract", "file": file_name, "count": i, "total": len}));
    }

    // delete files in target_path that are not in the changeset
    if let Some(changeset) = changeset.as_ref() {
        for file in &changeset.deleted {
            let mut out_path = std::path::PathBuf::from(target_path);
            out_path.push(file.clone());
            if out_path.exists() {
                std::fs::remove_file(out_path)?;
                notify(serde_json::json!({"type": "delete", "file": file}));
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
                    std::fs::remove_file(out_path)?;
                    notify(serde_json::json!({"type": "delete", "file": file}));
                }
            }
        }
    }
    Ok((metadata, changeset))
}

pub async fn run_mirrorc_download() {}
