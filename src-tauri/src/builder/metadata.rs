use std::path::{Path, PathBuf};

use futures::StreamExt;

use crate::utils::{hash::run_hash, metadata::FileMeta};

/// 校验源路径存在且是目录，区分三种情况：不存在、不是目录必须报错，
/// 只有合法的空目录才允许返回空列表——否则写错的源路径会静默生成
/// hashed=[] 的 metadata，旧文件还会被错列进 deletes。
async fn ensure_source_dir(source: &Path) -> Result<(), String> {
    let meta = tokio::fs::metadata(source).await.map_err(|e| {
        format!(
            "Source directory does not exist or is unreadable: {}: {e}",
            source.display()
        )
    })?;
    if !meta.is_dir() {
        return Err(format!(
            "Source path is not a directory: {}",
            source.display()
        ));
    }
    Ok(())
}

/// walkdir 给出的完整路径去掉 source 前缀得到相对路径，再统一为 `/` 分隔。
/// 用 strip_prefix 而非字符串替换：source 带尾分隔符时字符串匹配会失效，
/// 导致相对路径错误甚至把绝对路径写进 metadata。
fn relativize(entry_path: &Path, source: &Path) -> Result<String, String> {
    let relative = entry_path.strip_prefix(source).map_err(|e| {
        format!(
            "Failed to relativize {} against {}: {e}",
            entry_path.display(),
            source.display()
        )
    })?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

pub async fn deep_generate_metadata(source: &PathBuf) -> Result<Vec<FileMeta>, String> {
    let path = Path::new(&source);
    ensure_source_dir(path).await?;
    let mut entries = async_walkdir::WalkDir::new(source);
    let mut files = Vec::new();
    loop {
        match entries.next().await {
            Some(Ok(entry)) => {
                let f = entry.file_type().await;
                if f.is_err() {
                    return Err(format!("Failed to get file type: {:?}", f.err()));
                }
                let f = f.unwrap();
                if f.is_file() {
                    let fin_path = relativize(&entry.path(), path)?;
                    let size = entry.metadata().await.unwrap().len();
                    files.push(FileMeta {
                        file_name: fin_path,
                        md5: None,
                        xxh: None,
                        size,
                        installer: None,
                    });
                }
            }
            Some(Err(e)) => {
                return Err(format!("Failed to read entry: {e:?}"));
            }
            None => break,
        }
    }

    let mut joinset = tokio::task::JoinSet::new();

    for file in files.iter() {
        let source = source.clone();
        let mut file = file.clone();
        joinset.spawn(async move {
            let real_path = source.join(&file.file_name);
            let hash = run_hash("xxh", real_path.to_str().unwrap()).await;
            if hash.is_err() {
                return Err(hash.err().unwrap());
            }
            let hash = hash.unwrap();
            file.xxh = Some(hash);
            println!("Hashed: {:?}", file.file_name);
            Ok(file)
        });
    }
    let mut finished_hashes = Vec::new();
    while let Some(res) = joinset.join_next().await {
        if let Err(e) = res {
            return Err(format!("Failed to run hashing thread: {e:?}"));
        }
        let res = res.unwrap();
        if let Err(e) = res {
            return Err(format!("Failed to finish hashing: {e:?}"));
        }
        let res = res.unwrap();
        finished_hashes.push(res);
    }
    Ok(finished_hashes)
}

pub async fn deep_get_filelist(source: &PathBuf) -> Result<Vec<String>, String> {
    let path = Path::new(&source);
    ensure_source_dir(path).await?;
    let mut entries = async_walkdir::WalkDir::new(source);
    let mut files = Vec::new();
    loop {
        match entries.next().await {
            Some(Ok(entry)) => {
                let f = entry.file_type().await;
                if f.is_err() {
                    return Err(format!("Failed to get file type: {:?}", f.err()));
                }
                let f = f.unwrap();
                if f.is_file() {
                    let fin_path = relativize(&entry.path(), path)?;
                    files.push(fin_path);
                }
            }
            Some(Err(e)) => {
                return Err(format!("Failed to read entry: {e:?}"));
            }
            None => break,
        }
    }
    Ok(files)
}
