use std::path::Path;

use async_compression::tokio::bufread::ZstdEncoder;

use crate::{
    cli::arg::GenArgs,
    metadata::{deep_generate_metadata, PatchInfo, PatchItem, RepoMetadata},
    utils::hash::run_hash,
};

pub async fn gen_cli(args: GenArgs) {
    println!("Generating metadata...");
    let metadata = deep_generate_metadata(&args.input_dir)
        .await
        .expect("failed to generate metadata");
    println!("Writting metadata to {:?}", args.output_metadata);
    let mut repometa = RepoMetadata {
        repo_name: args.repo,
        tag_name: args.tag,
        assets: None,
        hashed: Some(metadata.clone()),
        patches: None,
    };
    let metadata_str = serde_json::to_string(&repometa).expect("failed to serialize metadata");
    tokio::fs::write(&args.output_metadata, metadata_str)
        .await
        .expect("failed to write metadata");
    println!("Compressing files...");
    // ensure output_dir
    let _ = tokio::fs::create_dir_all(&args.output_dir).await;
    // loop through files in metadata
    for file in metadata.iter() {
        // copy file to output_dir
        let file_path = args.input_dir.join(&file.file_name);
        let hash = if file.xxh.is_some() {
            file.xxh.as_ref().unwrap()
        } else if file.md5.is_some() {
            file.md5.as_ref().unwrap()
        } else {
            panic!("file has no hash");
        };
        let output_path = args.output_dir.join(hash);
        println!("Compressing {:?} to {:?}", file_path, output_path);
        let reader = tokio::fs::File::open(file_path).await.unwrap();
        let reader = tokio::io::BufReader::new(reader);
        let mut encoder = ZstdEncoder::new(reader);
        let mut writer = tokio::fs::File::create(output_path).await.unwrap();
        tokio::io::copy(&mut encoder, &mut writer)
            .await
            .expect("failed to compress file");
    }
    // check diffs
    if let Some(diff_vers) = args.diff_vers {
        if !diff_vers.is_empty() {
            // diff needed, check hdiffz availability
            let hdiffz_default = {
                #[cfg(target_os = "windows")]
                {
                    "hdiffz.exe"
                }
                #[cfg(not(target_os = "windows"))]
                {
                    "hdiffz"
                }
            };
            let hdiffz = args.hdiffz.unwrap_or(hdiffz_default.to_string());
            // exec --help to check if hdiffz is available
            let status = tokio::process::Command::new(&hdiffz)
                .arg("--help")
                .status()
                .await;
            if status.is_err() {
                panic!("Diff specified but hdiffz not found ar {:?}", hdiffz);
            }
            let mut diffs = Vec::new();
            // loop through diff_versions
            for diff_ver in diff_vers.iter() {
                println!("Generating diff for {}", diff_ver);
                // loop through current metadata
                for file in metadata.iter() {
                    println!("Checking diff for {:?}", file.file_name);
                    // file should > 1M
                    if file.size < 1024 * 1024 {
                        continue;
                    }
                    // check if file exists in diff_ver
                    let diff_file = Path::new(diff_ver).join(&file.file_name);
                    if !diff_file.exists() {
                        // file not found in diff_ver, skip
                        continue;
                    }
                    // file found, hash it
                    let old_hash = run_hash("xxh", &diff_file.to_str().unwrap())
                        .await
                        .expect("failed to hash diff file");
                    if old_hash == *file.xxh.as_ref().unwrap() {
                        // hash same, skip
                        continue;
                    }
                    // hash different, generate diff
                    let output_path = args.output_dir.join(format!(
                        "{}_{}",
                        old_hash,
                        file.xxh.as_ref().unwrap()
                    ));
                    println!("Generating diff for {:?} to {:?}", diff_file, output_path);
                    let status = tokio::process::Command::new(&hdiffz)
                        .arg("-m-0")
                        .arg("-SD")
                        .arg(diff_file.clone())
                        .arg(args.input_dir.join(&file.file_name))
                        .arg(output_path.clone())
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .stdin(std::process::Stdio::inherit())
                        .status()
                        .await;
                    if status.is_err() {
                        panic!("Failed to generate diff: {:?}", status.err());
                    }
                    let status = status.unwrap();
                    if !status.success() {
                        panic!("Failed to generate diff: {:?}", status);
                    }
                    let diff_size = tokio::fs::metadata(&output_path)
                        .await
                        .expect("failed to get diff size")
                        .len();
                    let old_size = tokio::fs::metadata(&diff_file)
                        .await
                        .expect("failed to get old size")
                        .len();
                    // if diff size is 50%+ of new file size, delete diff and skip
                    if diff_size > (file.size / 2) {
                        tokio::fs::remove_file(&output_path)
                            .await
                            .expect("failed to remove diff");
                        println!("File {:?} diff too large, skipped", file.file_name);
                        continue;
                    }
                    diffs.push(PatchInfo {
                        file_name: file.file_name.clone(),
                        size: diff_size,
                        from: PatchItem {
                            md5: None,
                            xxh: Some(old_hash.clone()),
                            size: old_size,
                        },
                        to: PatchItem {
                            md5: None,
                            xxh: Some(file.xxh.clone().unwrap()),
                            size: file.size,
                        },
                    });
                }
            }
            repometa.patches = Some(diffs);
            // write metadata again
            let metadata_str =
                serde_json::to_string(&repometa).expect("failed to serialize metadata");
            tokio::fs::write(&args.output_metadata, metadata_str)
                .await
                .expect("failed to write metadata");
        }
    }
    println!("Done");
}
