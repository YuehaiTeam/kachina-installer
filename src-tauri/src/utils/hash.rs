use std::{io::Read, path::Path};

pub async fn run_hash(hash_algorithm: &str, path: &str) -> Result<String, String> {
    if hash_algorithm == "md5" {
        let md5 = chksum_md5::async_chksum(Path::new(path)).await;
        if md5.is_err() {
            return Err(format!("Failed to calculate md5: {:?}", md5.err()));
        }
        let md5 = md5.unwrap();
        return Ok(md5.to_hex_lowercase());
    } else if hash_algorithm == "xxh" {
        let path = path.to_string();
        let res = tokio::task::spawn_blocking(move || {
            use twox_hash::XxHash3_128;
            let mut hasher = XxHash3_128::new();
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(false)
                .open(path);
            if file.is_err() {
                return Err(format!("Failed to open file: {:?}", file.err()));
            }
            let mut file = file.unwrap();
            let mut buffer = [0u8; 1024];
            loop {
                let read = file.read(&mut buffer);
                if read.is_err() {
                    return Err(format!("Failed to read file: {:?}", read.err()));
                }
                let read = read.unwrap();
                if read == 0 {
                    break;
                }
                hasher.write(&buffer[..read]);
            }
            let hash = hasher.finish_128();
            Ok(format!("{:x}", hash))
        })
        .await;
        if res.is_err() {
            return Err(format!("Failed to calculate xxhash: {:?}", res.err()));
        }
        let res = res.unwrap();
        if res.is_err() {
            return Err(format!("Failed to calculate xxhash: {:?}", res.err()));
        }
        let res = res.unwrap();
        return Ok(res);
    }
    // unknown hash algorithm
    Err("Unknown hash algorithm".to_string())
}
