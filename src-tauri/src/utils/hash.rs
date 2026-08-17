use anyhow::{Context, Result};
use std::{io::Read, os::windows::fs::OpenOptionsExt, path::Path};

const HASH_BUFFER_SIZE: usize = 1024 * 1024;
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

pub fn hash_file(hash_algorithm: &str, path: &str) -> Result<String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(false)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(Path::new(path))
        .context("OPEN_TARGET_ERR")?;

    let mut buffer = vec![0u8; HASH_BUFFER_SIZE];
    if hash_algorithm == "md5" {
        let mut hasher = chksum_md5::MD5::new();
        loop {
            let read = file.read(&mut buffer).context("READ_FILE_ERR")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.digest().to_hex_lowercase())
    } else if hash_algorithm == "xxh" {
        use twox_hash::XxHash3_128;
        let mut hasher = XxHash3_128::new();
        loop {
            let read = file.read(&mut buffer).context("READ_FILE_ERR")?;
            if read == 0 {
                break;
            }
            hasher.write(&buffer[..read]);
        }
        Ok(format!("{:x}", hasher.finish_128()))
    } else {
        Err(anyhow::anyhow!("NO_HASH_ALGO_ERR"))
    }
}

pub async fn run_hash(hash_algorithm: &str, path: &str) -> Result<String> {
    let hash_algorithm = hash_algorithm.to_string();
    let path = path.to_string();
    tokio::task::spawn_blocking(move || hash_file(&hash_algorithm, &path))
        .await
        .context("HASH_THREAD_ERR")?
        .context("HASH_COMPLETE_ERR")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(bytes: &[u8]) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "kachina-hash-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.bin");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        (dir, path.to_string_lossy().to_string())
    }

    fn cleanup(dir: std::path::PathBuf, path: &str) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn md5_matches_known_digest() {
        let (dir, path) = temp_file(b"hello");
        let hash = hash_file("md5", &path).unwrap();
        cleanup(dir, &path);
        assert_eq!(hash, "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn hashes_read_only_file() {
        let (dir, path) = temp_file(b"readonly-hash");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();
        let hash = hash_file("md5", &path).unwrap();
        let expected = chksum_md5::hash(b"readonly-hash").to_hex_lowercase();
        cleanup(dir, &path);
        assert_eq!(hash, expected);
    }

    #[test]
    fn rejects_unknown_algorithm() {
        let (dir, path) = temp_file(b"x");
        let err = hash_file("sha1", &path).is_err();
        cleanup(dir, &path);
        assert!(err);
    }
}
