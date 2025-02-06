use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::{cli::PackArgs, local::get_reader_for_bundle, metadata::RepoMetadata};

pub struct PackFile {
    pub name: String,
    pub size: usize,
    pub data: Box<dyn AsyncRead + Unpin + Send>,
}

pub struct PackConfig {
    pub config: serde_json::Value,
    pub metadata: Option<RepoMetadata>,
    pub image: Option<PackFile>,
    pub files: Vec<PackFile>,
}

pub async fn pack_cli(args: PackArgs) {
    let reader = get_reader_for_bundle().await;
    if reader.is_err() {
        eprintln!("Failed to get reader: {:?}", reader.err());
        return;
    }
    let reader = reader.unwrap();
    let config = tokio::fs::read(&args.config).await;
    if config.is_err() {
        eprintln!(
            "Failed to read config {:?} : {:?}",
            args.config,
            config.err()
        );
        return;
    }
    let config = config.unwrap();
    let config = serde_json::from_slice(&config);
    if config.is_err() {
        eprintln!("Failed to parse config: {:?}", config.err());
        return;
    }
    let config = config.unwrap();
    let metadata = if let Some(metadata) = args.metadata {
        let metadataf = tokio::fs::read(&metadata).await;
        if metadataf.is_err() {
            eprintln!(
                "Failed to read metadata {:?} : {:?}",
                metadata,
                metadataf.err()
            );
            return;
        }
        let metadataf = metadataf.unwrap();
        let json = serde_json::from_slice::<RepoMetadata>(&metadataf);
        if json.is_err() {
            eprintln!("Failed to parse metadata: {:?}", json.err());
            return;
        }
        Some(json.unwrap())
    } else {
        None
    };
    let image = if let Some(image) = args.image {
        let image_size = tokio::fs::metadata(&image).await;
        if image_size.is_err() {
            eprintln!("Failed to get image size: {:?}", image_size.err());
            return;
        }
        let image_size = image_size.unwrap().len() as u32;
        let imagef = tokio::fs::File::open(image).await;
        if imagef.is_err() {
            eprintln!("Failed to open image: {:?}", imagef.err());
            return;
        }
        Some(PackFile {
            name: "\0IMAGE".to_string(),
            size: image_size as usize,
            data: Box::new(imagef.unwrap()) as Box<dyn AsyncRead + Unpin + Send>,
        })
    } else {
        None
    };
    let data_dir = args.data_dir;
    let mut files = vec![];
    if let Some(data_dir) = data_dir {
        if let Some(metadata) = metadata.as_ref() {
            if let Some(hashed) = metadata.hashed.as_ref() {
                for file in hashed.iter() {
                    let hash = if file.md5.is_some() {
                        file.md5.as_ref().unwrap()
                    } else if file.xxh.is_some() {
                        file.xxh.as_ref().unwrap()
                    } else {
                        eprintln!("No hash found for file: {:?}", file.file_name);
                        return;
                    };
                    if files.iter().any(|x: &PackFile| x.name == *hash) {
                        continue;
                    }
                    let path = data_dir.join(hash);
                    let size = tokio::fs::metadata(&path).await.unwrap().len() as usize;
                    let f = tokio::fs::File::open(path).await;
                    if f.is_err() {
                        eprintln!("Failed to open file {}: {:?}", hash, f.err());
                        return;
                    }
                    let data = Box::new(f.unwrap()) as Box<dyn AsyncRead + Unpin + Send>;
                    files.push(PackFile {
                        name: hash.clone(),
                        size,
                        data,
                    });
                }
            }
            if let Some(patches) = metadata.patches.as_ref() {
                for patch in patches.iter() {
                    let from_hash = if patch.from.md5.is_some() {
                        patch.from.md5.as_ref().unwrap()
                    } else if patch.from.xxh.is_some() {
                        patch.from.xxh.as_ref().unwrap()
                    } else {
                        eprintln!("No hash found for patch: {:?}", patch.file_name);
                        return;
                    };
                    let to_hash = if patch.to.md5.is_some() {
                        patch.to.md5.as_ref().unwrap()
                    } else if patch.to.xxh.is_some() {
                        patch.to.xxh.as_ref().unwrap()
                    } else {
                        eprintln!("No hash found for patch: {:?}", patch.file_name);
                        return;
                    };
                    let patch_fn = format!("{}_{}", from_hash, to_hash);
                    if files.iter().any(|x: &PackFile| x.name == *patch_fn) {
                        continue;
                    }
                    let path = data_dir.join(&patch_fn);
                    let size = tokio::fs::metadata(&path).await.unwrap().len() as usize;
                    let f = tokio::fs::File::open(path).await;
                    if f.is_err() {
                        eprintln!("Failed to open file {}: {:?}", patch_fn, f.err());
                        return;
                    }
                    let data = Box::new(f.unwrap()) as Box<dyn AsyncRead + Unpin + Send>;
                    files.push(PackFile {
                        name: patch_fn,
                        size,
                        data,
                    });
                }
            }
        } else {
            // if no metadata set, just pack all files without '_'
            let entries = tokio::fs::read_dir(data_dir).await;
            if entries.is_err() {
                eprintln!("Failed to read data dir: {:?}", entries.err());
                return;
            }
            let mut entries = entries.unwrap();
            while let Some(entry) = entries.next_entry().await.unwrap() {
                let path = entry.path();
                let name = path.file_name().unwrap().to_str().unwrap().to_string();
                // ignore if name includes '_'
                if name.contains('_') {
                    continue;
                }
                let size = tokio::fs::metadata(&path).await.unwrap().len() as usize;
                let f = tokio::fs::File::open(path).await;
                if f.is_err() {
                    eprintln!("Failed to open file {}: {:?}", name, f.err());
                    return;
                }
                let data = Box::new(f.unwrap()) as Box<dyn AsyncRead + Unpin + Send>;
                files.push(PackFile { name, size, data });
            }
        }
    }
    let config = PackConfig {
        config,
        metadata,
        image,
        files,
    };
    let output = tokio::fs::File::create(args.output).await.unwrap();
    println!(
        "Packing: metadata: {:?}, image: {:?}, files: {}",
        config.metadata.is_some(),
        config.image.is_some(),
        config.files.len()
    );
    pack(reader, output, config).await;
}

pub async fn pack(
    mut base: impl AsyncRead + std::marker::Unpin,
    mut output: impl AsyncWrite + std::marker::Unpin,
    mut config: PackConfig,
) {
    println!("Reading base...");
    let mut base_data = vec![];
    tokio::io::copy(&mut base, &mut base_data).await.unwrap();
    let metadata_bytes = if let Some(metadata) = config.metadata {
        println!("Writing metadata...");
        let mut metadata = serde_json::json!(metadata);
        metadata.sort_all_objects();
        let metadata_bytes = serde_json::to_string(&metadata).unwrap();
        Some(metadata_bytes.as_bytes().to_vec())
    } else {
        None
    };
    println!("Generating index...");
    config.config.sort_all_objects();
    let config_bytes = serde_json::to_string(&config.config).unwrap();
    let mut files = config.files;
    // name size offset
    let mut index: Vec<(String, u32, u32)> = vec![];
    // insert config to index
    let mut current_offset = 0;
    index.push((
        "\0CONFIG".to_string(),
        config_bytes.len() as u32,
        get_header_size("\0CONFIG") as u32,
    ));
    current_offset += config_bytes.len() + get_header_size("\0CONFIG");
    // insert image to index
    if let Some(img) = config.image.as_ref() {
        let offset = current_offset + get_header_size(&img.name);
        index.push((img.name.clone(), img.size as u32, offset as u32));
        current_offset = offset + img.size;
    }
    // insert metadata to index
    if let Some(metadata_bytes) = metadata_bytes.as_ref() {
        let offset = current_offset + get_header_size("\0META");
        index.push((
            "\0META".to_string(),
            metadata_bytes.len() as u32,
            offset as u32,
        ));
        current_offset = offset + metadata_bytes.len();
    }
    files.sort_by_key(|x| x.name.clone());
    for file in files.iter_mut() {
        let name = file.name.clone();
        let size = file.size;
        let offset: usize = current_offset + get_header_size(&name);
        index.push((name, size as u32, offset as u32));
        current_offset = offset + size;
    }
    let index_len = index_to_bin(&index).len() + get_header_size("\0INDEX");
    // add index_len to offset
    for (name, _size, offset) in index.iter_mut() {
        // index is after config and image
        if name == "\0CONFIG" || name == "\0IMAGE" {
            continue;
        }
        *offset += index_len as u32;
    }
    // write pre-index to pe header
    let index_pre = if files.len() > 0 {
        gen_index_header(
            base_data.len() as u32,
            (config_bytes.len() + get_header_size("\0CONFIG")) as u32,
            if let Some(img) = config.image.as_ref() {
                (img.size + get_header_size(&img.name)) as u32
            } else {
                0
            },
            index_len as u32,
            if let Some(metadata_bytes) = metadata_bytes.as_ref() {
                (metadata_bytes.len() + get_header_size("\0META")) as u32
            } else {
                0
            },
        )
    } else {
        gen_index_header(0, 0, 0, 0, 0)
    };
    // replace 'This program cannot be run in DOS mode' in pe header to index_pre
    let pe_str_offset = base_data.iter().position(|x| *x == 0x54).unwrap();
    let pe_str = &mut base_data[pe_str_offset..pe_str_offset + index_pre.len()];
    // check if pe_str is really 'This program cannot be run in DOS mode'
    let pe_string = std::str::from_utf8_mut(pe_str).unwrap();
    if pe_string != "This program cannot be run in DOS mode" {
        eprintln!("Failed to find pe string: {:?}", pe_string);
        return;
    }
    pe_str.copy_from_slice(&index_pre);
    // copy base to output, not closing output file
    println!("Writing base...");
    output.write_all(&base_data).await.unwrap();
    // write config
    println!("Writing config...");
    let config_bytes = config_bytes.as_bytes();
    let res = write_header(&mut output, "\0CONFIG", config_bytes.len() as u32).await;
    if res.is_err() {
        eprintln!("Failed to write header: {:?}", res.err());
        return;
    }
    let res = output.write_all(config_bytes).await;
    if res.is_err() {
        eprintln!("Failed to write config: {:?}", res.err());
        return;
    }
    // if theme exists, write theme
    if let Some(image) = config.image.as_mut() {
        println!("Writing image...");
        let res = write_file(&mut output, image).await;
        if res.is_err() {
            eprintln!("Failed to write image: {:?}", res.err());
            return;
        }
    }
    // write index
    println!("Writing index...");
    let index_bytes = index_to_bin(&index);
    write_header(&mut output, "\0INDEX", index_bytes.len() as u32)
        .await
        .unwrap();

    output.write_all(&index_bytes).await.unwrap();
    // if metadata exists, write metadata
    if let Some(metadata_bytes) = metadata_bytes {
        let res = write_header(&mut output, "\0META", metadata_bytes.len() as u32).await;
        if res.is_err() {
            eprintln!("Failed to write header: {:?}", res.err());
            return;
        }
        let res = output.write_all(&metadata_bytes).await;
        if res.is_err() {
            eprintln!("Failed to write metadata: {:?}", res.err());
            return;
        }
    }
    // write files
    for file in files.iter_mut() {
        println!("Writing file: {}", file.name);
        let res = write_file(&mut output, file).await;
        if res.is_err() {
            eprintln!("Failed to write file {}: {:?}", file.name, res.err());
            return;
        }
    }
    // flush
    println!("Finalizing...");
    let res = output.flush().await;
    if res.is_err() {
        eprintln!("Failed to flush: {:?}", res.err());
        return;
    }
    println!("Done");
}

pub async fn write_header(
    output: &mut (impl AsyncWrite + std::marker::Unpin),
    name: &str,
    size: u32,
) -> Result<(), tokio::io::Error> {
    let header = "!in\0".to_ascii_uppercase();
    let header = header.as_bytes();
    let name = name.as_bytes();
    let size = size.to_be_bytes();
    output.write_all(header).await?;
    let namelen = (name.len() as u16).to_be_bytes();
    output.write_all(&namelen).await?;
    output.write_all(name).await?;
    output.write_all(&size).await?;
    Ok(())
}

pub fn get_header_size(name: &str) -> usize {
    "!in\0".len() + 2 + name.len() + 4
}

pub fn index_to_bin(index: &[(String, u32, u32)]) -> Vec<u8> {
    let mut data = vec![];
    // u8: name_len var: name u32: size u32: offset
    for (name, size, offset) in index.iter() {
        let name = name.as_bytes();
        let name_len = name.len() as u8;
        data.push(name_len);
        data.extend_from_slice(name);
        data.extend_from_slice(&size.to_be_bytes());
        data.extend_from_slice(&offset.to_be_bytes());
    }
    data
}

pub fn gen_index_header(
    base_end: u32,
    config_end: u32,
    theme_end: u32,
    index_end: u32,
    manifest_end: u32,
) -> Vec<u8> {
    let mut data = "!KachinaInstaller!".as_bytes().to_vec();
    data.extend_from_slice(&base_end.to_be_bytes());
    data.extend_from_slice(&config_end.to_be_bytes());
    data.extend_from_slice(&theme_end.to_be_bytes());
    data.extend_from_slice(&index_end.to_be_bytes());
    data.extend_from_slice(&manifest_end.to_be_bytes());
    data
}

pub async fn write_file(
    output: &mut (impl AsyncWrite + std::marker::Unpin),
    file: &mut PackFile,
) -> Result<(), tokio::io::Error> {
    write_header(output, &file.name, file.size as u32).await?;
    tokio::io::copy(&mut file.data, output).await?;
    Ok(())
}
