use anyhow::{Context, Result};
use std::{io::Read, path::Path};

pub async fn run_hash(hash_algorithm: &str, path: &str) -> Result<String> {
    if hash_algorithm == "md5" {
        let md5 = chksum_md5::async_chksum(Path::new(path))
            .await
            .context("HASH_COMPLETE_ERR")?;
        Ok(md5.to_hex_lowercase())
    } else if hash_algorithm == "xxh" {
        let path = path.to_string();
        let res = tokio::task::spawn_blocking(move || {
            use twox_hash::XxHash3_128;
            let mut hasher = XxHash3_128::new();
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(false)
                .open(&path)
                .context("OPEN_TARGET_ERR")?;

            let mut buffer = [0u8; 1024];
            loop {
                let read = file.read(&mut buffer).context("READ_FILE_ERR")?;
                if read == 0 {
                    break;
                }
                hasher.write(&buffer[..read]);
            }
            let hash = hasher.finish_128();
            Ok::<String, anyhow::Error>(format!("{:x}", hash))
        })
        .await
        .context("HASH_THREAD_ERR")?
        .context("HASH_COMPLETE_ERR")?;
        Ok(res)
    } else {
        Err(anyhow::anyhow!("NO_HASH_ALGO_ERR"))
    }
}
