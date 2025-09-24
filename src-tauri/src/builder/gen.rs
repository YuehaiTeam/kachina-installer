use std::{collections::HashSet, path::Path};

use async_compression::tokio::bufread::ZstdEncoder;
use hdiff_sys::safe_create_single_patch;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tokio::{io::AsyncWriteExt, task::JoinSet};

use crate::{
    cli::GenArgs,
    metadata::{deep_generate_metadata, deep_get_filelist},
    utils::{
        hash::run_hash,
        metadata::{InstallerInfo, Metadata, PatchInfo, PatchItem, RepoMetadata},
        progressed_read::ReadWithCallback,
    },
};

pub async fn gen_cli(args: GenArgs) {
    let pb_style = ProgressStyle::with_template("[{elapsed_precise}] {bar:20.cyan/blue} {msg} ")
        .unwrap()
        .progress_chars("##-");
    let pb_style_total =
        ProgressStyle::with_template("[{elapsed_precise}] {bar:20.cyan/blue} {pos}/{len} {msg} ")
            .unwrap()
            .progress_chars("##-");
    // ensure output_dir
    println!("Creating output directory...");
    let _ = tokio::fs::create_dir_all(&args.output_dir).await;
    // hash updater
    let mut installer = None;
    if let Some(updater) = args.updater.as_ref() {
        println!("Hashing updater...");
        let hash = run_hash("xxh", updater.to_str().unwrap())
            .await
            .expect("failed to hash updater");
        let size = tokio::fs::metadata(updater)
            .await
            .expect("failed to get updater size")
            .len();
        installer = Some(InstallerInfo {
            size,
            md5: None,
            xxh: Some(hash),
        });
    }
    println!("Generating metadata...");
    let mut metadata = deep_generate_metadata(&args.input_dir)
        .await
        .expect("failed to generate metadata");
    if let Some(installer) = installer.as_ref() {
        // remove updater from metadata
        metadata.retain(|x| x.xxh.as_ref().unwrap() != installer.xxh.as_ref().unwrap());
    }
    println!("Writting metadata to {:?}", args.output_metadata);
    let mut repometa = RepoMetadata {
        repo_name: args.repo,
        tag_name: args.tag,
        assets: None,
        hashed: Some(metadata.clone()),
        patches: None,
        installer,
        deletes: None,
        packing_info: None,
    };
    let metadata_str = serde_json::to_string(&repometa).expect("failed to serialize metadata");
    tokio::fs::write(&args.output_metadata, metadata_str)
        .await
        .expect("failed to write metadata");
    println!("Compressing files...");
    let multi_pg = MultiProgress::new();

    // create a progress bar to track overall status
    let pb_main = multi_pg.add(ProgressBar::new(metadata.len() as u64));
    pb_main.set_style(pb_style_total.clone());
    pb_main.set_message("TOTAL");

    // Make the main progress bar render immediately rather than waiting for the
    // first task to finish.
    pb_main.tick();

    // tokio::task::JoinSet
    // setup the JoinSet to manage the join handles for our futures
    let mut set = JoinSet::new();

    let mut last_item = false;

    // iterate over our downloads vec and
    // spawn a background task for each download (do_stuff)
    // Does not spawn more tasks than MAX_CONCURRENT "allows"
    for (index, file) in metadata.iter().enumerate() {
        let pb_main_ = pb_main.clone();
        if index == metadata.len() - 1 {
            last_item = true;
        }

        // create a progress bar for each download and set the style
        // using insert_before() so that pb_main stays below the other progress bars
        let pb_task = multi_pg.insert_before(&pb_main, ProgressBar::new(file.size));
        pb_task.set_style(pb_style.clone());

        // spawns a background task immediatly no matter if the future is awaited
        // https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html#method.spawn
        let file = file.clone();
        let output = args.output_dir.clone();
        let input: std::path::PathBuf = args.input_dir.clone();
        set.spawn(tokio::task::spawn_blocking(|| {
            // create new tokio runtime for each task
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let display_name = file.file_name.clone().replace("\\", "/");
                // copy file to output_dir
                let file_path = input.join(&file.file_name);
                let hash = if file.xxh.is_some() {
                    file.xxh.as_ref().unwrap()
                } else if file.md5.is_some() {
                    file.md5.as_ref().unwrap()
                } else {
                    panic!("file has no hash");
                };
                let output_path = output.join(hash);
                pb_task.set_message(format!("     {display_name:?}"));
                let task_ = pb_task.clone();
                let pb_main_ = pb_main_.clone();
                let reader = tokio::fs::File::open(file_path).await.unwrap();
                let reader = ReadWithCallback {
                    reader,
                    callback: move |chunk| {
                        task_.inc(chunk as u64);
                        pb_main_.tick();
                    },
                };
                let reader = tokio::io::BufReader::new(reader);
                let mut encoder = ZstdEncoder::with_quality(reader, async_compression::Level::Best);
                let mut writer = tokio::fs::File::create(output_path).await.unwrap();
                tokio::io::copy(&mut encoder, &mut writer)
                    .await
                    .expect("failed to compress file");
                if !console::Term::stdout().is_term() {
                    println!("Compressed {display_name:?}");
                }
                pb_task.finish_with_message(format!("DONE {display_name:?}"));
            });
        }));

        // when limit is reached, wait until a running task finishes
        // await the future (join_next().await) and get the execution result
        // here result would be a download id(u64), as you can see in signature of do_stuff
        while set.len() >= args.zstd_concurrency || last_item {
            match set.join_next().await {
                Some(res) => {
                    if let Err(e) = res {
                        eprintln!("Zstd Task Error: {e:?}");
                        std::process::exit(1);
                    }
                    let res = res.unwrap();
                    if let Err(e) = res {
                        eprintln!("Zstd Task Error: {e:?}");
                        std::process::exit(1);
                    }
                }
                None => {
                    break;
                }
            };
            pb_main.inc(1);
        }
    }
    pb_main.finish_with_message("Compression finished");
    // compress and copy installer
    if let Some(installer) = repometa.installer.as_ref() {
        let output_path = args.output_dir.join(installer.xxh.as_ref().unwrap());
        println!("Compressing installer to {output_path:?}");
        let reader = tokio::fs::File::open(args.updater.as_ref().unwrap())
            .await
            .expect("failed to open installer");
        let reader = tokio::io::BufReader::new(reader);
        let mut encoder: ZstdEncoder<tokio::io::BufReader<tokio::fs::File>> =
            ZstdEncoder::with_quality_and_params(
                reader,
                async_compression::Level::Best,
                &[async_compression::zstd::CParameter::nb_workers(
                    num_cpus::get() as u32,
                )],
            );
        let mut writer = tokio::fs::File::create(output_path)
            .await
            .expect("failed to create file");
        tokio::io::copy(&mut encoder, &mut writer)
            .await
            .expect("failed to compress file");
    }
    // check diffs
    if let Some(diff_vers) = args.diff_vers {
        let mut metadata_with_installer = metadata.clone();
        if let Some(installer) = repometa.installer.as_ref() {
            metadata_with_installer.push(Metadata {
                file_name: if let Some(name) = args.updater_name.as_ref() {
                    name.clone()
                } else if let Some(name) = args.updater.as_ref().unwrap().file_name() {
                    name.to_string_lossy().to_string()
                } else {
                    panic!("failed to get updater name");
                },
                size: installer.size,
                md5: installer.md5.clone(),
                xxh: installer.xxh.clone(),
            });
        }
        if !diff_vers.is_empty() {
            let mut ignore = ignore::gitignore::GitignoreBuilder::new("/");
            if let Some(diff_ignore) = args.diff_ignore {
                for ignore_file in diff_ignore.iter() {
                    ignore.add_line(None, ignore_file).unwrap();
                }
            }
            let ignore = ignore.build().unwrap();
            let mut diffs = Vec::new();
            let mut deletes = Vec::new();
            // loop through diff_versions
            for diff_ver in diff_vers.iter() {
                // loop through current metadata
                let multi_pg = MultiProgress::new();

                // create a progress bar to track overall status
                let pb_main = multi_pg.add(ProgressBar::new(metadata_with_installer.len() as u64));
                pb_main.set_style(pb_style_total.clone());
                pb_main.set_message(format!("DIFF TOTAL {diff_ver}"));

                // Make the main progress bar render immediately rather than waiting for the
                // first task to finish.
                pb_main.tick();

                // tokio::task::JoinSet
                // setup the JoinSet to manage the join handles for our futures
                let mut set = JoinSet::new();

                let mut last_item = false;

                for (index, file) in metadata_with_installer.iter().enumerate() {
                    if index == metadata.len() - 1 {
                        last_item = true;
                    }

                    let input_dir = args.input_dir.clone();
                    let output_dir = args.output_dir.clone();
                    let diff_ver = diff_ver.clone();
                    let diff_ver2 = diff_ver.clone();

                    // spawns a background task immediatly no matter if the future is awaited
                    // https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html#method.spawn
                    let file = file.clone();
                    let ignore = ignore.clone();
                    set.spawn(async move {
                        if ignore
                            .matched_path_or_any_parents(&file.file_name, false)
                            .is_ignore()
                        {
                            println!("File {:?} ignored", file.file_name);
                            return None;
                        }
                        // file should > 1M
                        if file.size < 1024 * 1024 {
                            println!("File {:?} too small, skipped", file.file_name);
                            return None;
                        }
                        // check if file exists in diff_ver
                        let diff_file = Path::new(&diff_ver).join(&file.file_name);
                        if !diff_file.exists() {
                            // file not found in diff_ver, skip
                            println!("File {:?} not found in diff_ver, skipped", file.file_name);
                            return None;
                        }
                        // file found, hash it
                        let old_hash = run_hash("xxh", diff_file.to_str().unwrap())
                            .await
                            .expect("failed to hash diff file");
                        if old_hash == *file.xxh.as_ref().unwrap() {
                            // hash same, skip
                            println!("File {:?} hash same, skipped", file.file_name);
                            return None;
                        }
                        // hash different, generate diff
                        let output_path = output_dir.join(format!(
                            "{}_{}.hdiff",
                            old_hash,
                            file.xxh.as_ref().unwrap()
                        ));
                        let compressed_path =
                            output_dir.join(format!("{}_{}", old_hash, file.xxh.as_ref().unwrap()));
                        println!("Generating diff for {diff_file:?} to {output_path:?}");
                        // read old_data and new_data to memory
                        let old_data = tokio::fs::read(&diff_file)
                            .await
                            .expect("failed to read old data");
                        let new_data = tokio::fs::read(input_dir.join(&file.file_name))
                            .await
                            .expect("failed to read new data");
                        let output_file = std::fs::File::create(&output_path)
                            .expect("failed to create output file");
                        tokio::task::spawn_blocking(move || {
                            // create output file
                            safe_create_single_patch(&new_data, &old_data, output_file, 7)
                        })
                        .await
                        .expect("failed to create diff")
                        .expect("failed to create diff");
                        // compress diff file
                        let reader = tokio::fs::File::open(&output_path)
                            .await
                            .expect("failed to open diff file");
                        let reader = tokio::io::BufReader::new(reader);
                        let mut encoder =
                            ZstdEncoder::with_quality(reader, async_compression::Level::Best);
                        let mut writer = tokio::fs::File::create(&compressed_path)
                            .await
                            .expect("failed to create compressed diff file");
                        tokio::io::copy(&mut encoder, &mut writer)
                            .await
                            .expect("failed to compress diff");
                        // flush writer
                        writer.flush().await.expect("failed to flush writer");
                        // close file
                        drop(writer);
                        let diff_original_size = tokio::fs::metadata(&output_path)
                            .await
                            .expect("failed to get diff size")
                            .len();
                        // delete uncompressed diff
                        tokio::fs::remove_file(&output_path)
                            .await
                            .expect("failed to remove uncompressed diff");
                        // if diff size is 50%+ of new file size, delete diff and skip
                        let diff_size = tokio::fs::metadata(&compressed_path)
                            .await
                            .expect("failed to get diff size")
                            .len();
                        let old_size = tokio::fs::metadata(&diff_file)
                            .await
                            .expect("failed to get old size")
                            .len();
                        if diff_size > (file.size / 2) {
                            tokio::fs::remove_file(&compressed_path)
                                .await
                                .expect("failed to remove diff");
                            println!("File {:?} diff too large, skipped", file.file_name);
                            return None;
                        }
                        Some(PatchInfo {
                            file_name: file.file_name.clone(),
                            size: diff_original_size,
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
                        })
                    });
                    while set.len() >= args.zstd_concurrency || last_item {
                        match set.join_next().await {
                            Some(res) => {
                                if let Err(e) = res {
                                    eprintln!("Diff Task Error: {e:?}");
                                    std::process::exit(1);
                                }
                                let res = res.unwrap();
                                if let Some(diff) = res {
                                    diffs.push(diff);
                                }
                            }
                            None => {
                                break;
                            }
                        };
                        pb_main.inc(1);
                    }
                }
                let diff_filelist = deep_get_filelist(&diff_ver.into())
                    .await
                    .expect("failed to get diff_ver file list");
                println!("Checking for deleted files in {diff_ver}...");
                for file in diff_filelist.iter() {
                    // check if file exists in current metadata
                    if !metadata_with_installer.iter().any(|x| x.file_name == *file) {
                        // file not found in current metadata, add to deletes
                        println!("File {file:?} not found in current metadata, added to deletes");
                        deletes.push(file.clone());
                    }
                }
            }
            repometa.deletes = Some(deletes);
            // 生成打包优化信息（在移动 diffs 之前）
            let diff_vers_pathbuf: Vec<std::path::PathBuf> =
                diff_vers.iter().map(std::path::PathBuf::from).collect();
            let packing_info =
                generate_packing_info(&metadata_with_installer, &diffs, &diff_vers_pathbuf).await;

            repometa.patches = Some(diffs);
            repometa.packing_info = Some(packing_info);

            // write metadata again
            let metadata_str =
                serde_json::to_string(&repometa).expect("failed to serialize metadata");
            tokio::fs::write(&args.output_metadata, metadata_str)
                .await
                .expect("failed to write metadata");
        }
    } else {
        // 即使没有diff版本，如果指定了diff_vers参数，也生成基础的packing_info
        println!("Generating packing info for first release...");
        let mut metadata_with_installer = metadata.clone();
        if let Some(installer) = repometa.installer.as_ref() {
            metadata_with_installer.push(Metadata {
                file_name: if let Some(name) = args.updater_name.as_ref() {
                    name.clone()
                } else if let Some(name) = args.updater.as_ref().unwrap().file_name() {
                    name.to_string_lossy().to_string()
                } else {
                    panic!("failed to get updater name");
                },
                size: installer.size,
                md5: installer.md5.clone(),
                xxh: installer.xxh.clone(),
            });
        }

        let empty_diffs = Vec::new();
        let empty_diff_vers = Vec::new();
        let packing_info =
            generate_packing_info(&metadata_with_installer, &empty_diffs, &empty_diff_vers).await;
        repometa.packing_info = Some(packing_info);

        // write metadata again
        let metadata_str = serde_json::to_string(&repometa).expect("failed to serialize metadata");
        tokio::fs::write(&args.output_metadata, metadata_str)
            .await
            .expect("failed to write metadata");
    }
    println!("Done");
}

async fn generate_packing_info(
    metadata_with_installer: &[Metadata],
    patches: &[PatchInfo],
    diff_vers: &[std::path::PathBuf],
) -> Vec<Vec<String>> {
    let mut packing_info = vec![
        Vec::new(), // [0] 大文件
        Vec::new(), // [1] 没有更新的小文件
        Vec::new(), // [2] 有变化或新增的小文件
        Vec::new(), // [3] 小patch
        Vec::new(), // [4] 大patch
    ];

    // 收集变化文件的hash集合
    let mut changed_hashes = HashSet::new();
    let mut new_hashes = HashSet::new();

    if !diff_vers.is_empty() {
        // 分析每个文件的变化状态
        for file in metadata_with_installer {
            let hash = file.xxh.as_ref().unwrap();

            // 检查文件是否存在于旧版本
            let mut found_in_old = false;
            let mut hash_changed = false;

            for diff_ver in diff_vers {
                let old_file_path = diff_ver.join(&file.file_name);
                if old_file_path.exists() {
                    found_in_old = true;
                    let old_hash_result = run_hash("xxh", old_file_path.to_str().unwrap()).await;
                    if let Ok(old_hash) = old_hash_result {
                        if old_hash != *hash {
                            hash_changed = true;
                        }
                    }
                    break;
                }
            }

            if !found_in_old {
                new_hashes.insert(hash.clone());
            } else if hash_changed {
                changed_hashes.insert(hash.clone());
            }
        }
    } else {
        // 没有diff版本，所有文件都是新增
        for file in metadata_with_installer {
            new_hashes.insert(file.xxh.as_ref().unwrap().clone());
        }
    }

    // 分类原始文件
    for file in metadata_with_installer {
        let hash = file.xxh.as_ref().unwrap();

        if file.size > 1024 * 1024 {
            packing_info[0].push(hash.clone()); // 大文件
        } else if new_hashes.contains(hash) || changed_hashes.contains(hash) {
            packing_info[2].push(hash.clone()); // 有变化的小文件
        } else {
            packing_info[1].push(hash.clone()); // 没有更新的小文件
        }
    }

    // 分类patch文件
    for patch in patches {
        let patch_name = format!(
            "{}_{}",
            patch.from.xxh.as_ref().unwrap(),
            patch.to.xxh.as_ref().unwrap()
        );
        if patch.size > 1024 * 1024 {
            packing_info[4].push(patch_name); // 大patch
        } else {
            packing_info[3].push(patch_name); // 小patch
        }
    }

    println!("Packing info generated:");
    println!("  Large files: {}", packing_info[0].len());
    println!("  Unchanged small files: {}", packing_info[1].len());
    println!("  Changed small files: {}", packing_info[2].len());
    println!("  Small patches: {}", packing_info[3].len());
    println!("  Large patches: {}", packing_info[4].len());

    packing_info
}
