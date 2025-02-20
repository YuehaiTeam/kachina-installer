use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt, AsyncMmapFileReader};
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
static MMAP_SELF: OnceCell<AsyncMmapFile> = OnceCell::const_new();

pub async fn mmap() -> &'static AsyncMmapFile {
    MMAP_SELF
        .get_or_init(|| async {
            let exe_path = {
                #[cfg(debug_assertions)]
                {
                    // use last release build
                    let exe_path = std::env::current_exe().unwrap();
                    // ../release/${basename}
                    let exe_path = exe_path
                        .parent()
                        .ok_or("Failed to get parent dir".to_string())
                        .unwrap()
                        .parent()
                        .ok_or("Failed to get parent dir".to_string())
                        .unwrap()
                        .join("release")
                        .join("kachina-builder-bundle.exe");
                    exe_path
                }
                #[cfg(not(debug_assertions))]
                {
                    std::env::current_exe().unwrap()
                }
            };
            AsyncMmapFile::open(exe_path).await.unwrap()
        })
        .await
}

async fn search_pattern() -> Result<Vec<usize>, String> {
    let pattern: [u8; 4] = [0x4D, 0x5A, 0x90, 0x00]; // exe header
    let file = mmap().await;
    let mut reader = file.reader(0).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 4096];
    let mut offset: usize = 0;
    let mut previous_bytes = Vec::new();
    let mut founds = Vec::new();

    loop {
        let bytes_read = reader.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if bytes_read == 0 {
            break;
        }

        // Step 1: Check across previous_bytes and buffer
        if !previous_bytes.is_empty() {
            let pb_len = previous_bytes.len();
            let needed = 4 - pb_len;
            if bytes_read >= needed {
                let combined: Vec<u8> = previous_bytes
                    .iter()
                    .cloned()
                    .chain(buffer[..needed].iter().cloned())
                    .collect();
                if combined == pattern[..] {
                    founds.push(offset - (pb_len));
                }
            }
        }

        // Step 2: Check within the buffer
        for i in 0..bytes_read - 3 {
            if buffer[i..i + 4] == pattern {
                founds.push(offset + i);
            }
        }

        // Step 3: Update previous_bytes
        if bytes_read > 0 {
            let start = bytes_read - std::cmp::min(3, bytes_read);
            previous_bytes = buffer[start..bytes_read].to_vec();
        }

        offset += bytes_read as usize;
    }

    Ok(founds)
}

pub async fn get_reader_for_bundle() -> Result<AsyncMmapFileReader<'static>, String> {
    let headers = search_pattern().await?;
    if headers.len() < 3 {
        return Err("Failed to find packed exe".to_string());
    }
    let exe_offset = headers[2];
    let file = mmap().await;
    let reader = file.reader(exe_offset).map_err(|e| e.to_string())?;
    Ok(reader)
}
