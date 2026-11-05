use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::pack_format::embedded_name::{check_index_name, check_tlv_name, EmbeddedNameError};
use crate::pack_format::{get_header_size, preferred_file_hash, LAYOUT_MARKER, TLV_MAGIC};
use crate::{cli::PackArgs, local::get_reader_for_bundle, utils::metadata::RepoMetadata};

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
    pub icon_path: Option<PathBuf>,
}

pub async fn pack_cli(args: PackArgs) -> Result<(), String> {
    let reader = get_reader_for_bundle().await;
    if reader.is_err() {
        return Err(format!("Failed to get reader: {:?}", reader.err()));
    }
    let reader = reader.unwrap();
    let config = tokio::fs::read(&args.config).await;
    if config.is_err() {
        return Err(format!(
            "Failed to read config {:?} : {:?}",
            args.config,
            config.err()
        ));
    }
    let config = config.unwrap();
    let config = serde_json::from_slice(&config);
    if config.is_err() {
        return Err(format!("Failed to parse config: {:?}", config.err()));
    }
    let config = config.unwrap();
    let metadata = if let Some(metadata) = args.metadata {
        let metadataf = tokio::fs::read(&metadata).await;
        if metadataf.is_err() {
            return Err(format!(
                "Failed to read metadata {:?} : {:?}",
                metadata,
                metadataf.err()
            ));
        }
        let metadataf = metadataf.unwrap();
        let json = serde_json::from_slice::<RepoMetadata>(&metadataf);
        if json.is_err() {
            return Err(format!("Failed to parse metadata: {:?}", json.err()));
        }
        Some(json.unwrap())
    } else {
        None
    };
    let image = if let Some(image) = args.image {
        let image_size = tokio::fs::metadata(&image).await;
        if image_size.is_err() {
            return Err(format!("Failed to get image size: {:?}", image_size.err()));
        }
        let image_size = image_size.unwrap().len() as u32;
        let imagef = tokio::fs::File::open(image).await;
        if imagef.is_err() {
            return Err(format!("Failed to open image: {:?}", imagef.err()));
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
            if !metadata.hashed.is_empty() {
                for file in metadata.hashed.iter() {
                    let Some(hash) = preferred_file_hash(&file.md5, &file.xxh) else {
                        return Err(format!("No hash found for file: {:?}", file.file_name));
                    };
                    if files.iter().any(|x: &PackFile| x.name == *hash) {
                        continue;
                    }
                    let path = data_dir.join(hash);
                    let size = tokio::fs::metadata(&path).await.unwrap().len() as usize;
                    let f = tokio::fs::File::open(path).await;
                    if f.is_err() {
                        return Err(format!("Failed to open file {}: {:?}", hash, f.err()));
                    }
                    let data = Box::new(f.unwrap()) as Box<dyn AsyncRead + Unpin + Send>;
                    files.push(PackFile {
                        name: hash.clone(),
                        size,
                        data,
                    });
                }
            }
            if !metadata.patches.is_empty() {
                for patch in metadata.patches.iter() {
                    let Some(from_hash) = preferred_file_hash(&patch.from.md5, &patch.from.xxh)
                    else {
                        return Err(format!("No hash found for patch: {:?}", patch.file_name));
                    };
                    let Some(to_hash) = preferred_file_hash(&patch.to.md5, &patch.to.xxh) else {
                        return Err(format!("No hash found for patch: {:?}", patch.file_name));
                    };
                    let patch_fn = format!("{from_hash}_{to_hash}");
                    if files.iter().any(|x: &PackFile| x.name == *patch_fn) {
                        continue;
                    }
                    let path = data_dir.join(&patch_fn);
                    let size = tokio::fs::metadata(&path).await.unwrap().len() as usize;
                    let f = tokio::fs::File::open(path).await;
                    if f.is_err() {
                        return Err(format!("Failed to open file {}: {:?}", patch_fn, f.err()));
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
                return Err(format!("Failed to read data dir: {:?}", entries.err()));
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
                    return Err(format!("Failed to open file {}: {:?}", name, f.err()));
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
        icon_path: args.icon,
    };
    validate_pack_names(&config).map_err(|err| err.to_string())?;
    let output = tokio::fs::File::create(&args.output)
        .await
        .map_err(|err| format!("Failed to create output {:?}: {err}", args.output))?;
    println!(
        "Packing: metadata: {:?}, image: {:?}, files: {}",
        config.metadata.is_some(),
        config.image.is_some(),
        config.files.len()
    );
    let result = pack(reader, output, config).await;
    if result.is_err() {
        // a half-written installer must not be mistaken for a build output
        let _ = tokio::fs::remove_file(&args.output).await;
    }
    result
}

pub async fn pack(
    mut base: impl AsyncRead + std::marker::Unpin,
    mut output: impl AsyncWrite + std::marker::Unpin,
    mut config: PackConfig,
) -> Result<(), String> {
    if let Err(err) = validate_pack_names(&config) {
        return Err(format!("{err}"));
    }
    println!("Generating exe with version info...");
    let tmppath = std::env::temp_dir().join(format!(
        "kachina_installer_tmp_{}_{}.exe",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut tmpfile = tokio::fs::File::create(&tmppath).await.unwrap();
    tokio::io::copy(&mut base, &mut tmpfile).await.unwrap();
    tmpfile.sync_all().await.unwrap();
    tmpfile.shutdown().await.unwrap();
    drop(tmpfile);
    let tmp_len = tokio::fs::metadata(&tmppath).await.unwrap().len();
    let mut updater = rcedit::ResourceUpdater::new();
    if let Err(e) = updater.load(&tmppath) {
        panic!(
            "rcedit load {} ({} bytes): {e:?}",
            tmppath.display(),
            tmp_len
        );
    }
    let unwrapped_config = config.config.as_object().unwrap();
    let title = unwrapped_config
        .get("windowTitle")
        .unwrap()
        .as_str()
        .unwrap();
    let product = unwrapped_config.get("appName").unwrap().as_str().unwrap();
    updater
        .set_version_string("FileDescription", title)
        .unwrap();
    updater.set_version_string("ProductName", product).unwrap();

    // Set icon if provided
    if let Some(icon_path) = &config.icon_path {
        if icon_path.exists() {
            if let Err(e) = updater.set_icon(icon_path) {
                eprintln!("Warning: Failed to set icon: {:?}", e);
            } else {
                println!("Icon set successfully: {:?}", icon_path);
            }
        } else {
            eprintln!("Warning: Icon file not found: {:?}", icon_path);
        }
    }

    updater.commit().unwrap();
    drop(updater);
    println!("Reading base...");
    let mut base_data = vec![];
    // read tmp file
    let mut tmpfile = tokio::fs::File::open(tmppath.clone()).await.unwrap();
    tokio::io::copy(&mut tmpfile, &mut base_data).await.unwrap();
    // close tmp file
    tmpfile.shutdown().await.unwrap();
    drop(tmpfile);
    // remove tmp file
    tokio::fs::remove_file(tmppath).await.unwrap();

    // packing_info 为空时不参与分类排序（避免按 [0]..=[4] 打印时越界）。
    let packing_info_clone = config
        .metadata
        .as_ref()
        .map(|m| m.packing_info.clone())
        .filter(|p| !p.is_empty());

    let metadata_bytes = if let Some(metadata) = config.metadata {
        println!("Writing metadata...");
        Some(embed_metadata_bytes(metadata))
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
    // 使用打包优化信息进行智能排序
    if let Some(ref packing_info) = packing_info_clone {
        println!("Packing order optimization enabled:");
        println!("  Large files: {}", packing_info[0].len());
        println!("  Unchanged small files: {}", packing_info[1].len());
        println!("  Changed small files: {}", packing_info[2].len());
        println!("  Small patches: {}", packing_info[3].len());
        println!("  Large patches: {}", packing_info[4].len());
    }

    files.sort_by_key(|file| get_file_pack_priority(&file.name, packing_info_clone.as_ref()));
    for file in files.iter_mut() {
        let name = file.name.clone();
        let size = file.size;
        let offset: usize = current_offset + get_header_size(&name);
        index.push((name, size as u32, offset as u32));
        current_offset = offset + size;
    }
    let index_len = match index_to_bin(&index) {
        Ok(bytes) => bytes.len() + get_header_size("\0INDEX"),
        Err(err) => {
            return Err(format!("{err}"));
        }
    };
    // add index_len to offset
    for (name, _size, offset) in index.iter_mut() {
        // index is after config and image
        if name == "\0CONFIG" || name == "\0IMAGE" {
            continue;
        }
        *offset += index_len as u32;
    }
    // write pre-index to pe header
    let index_pre = if !files.is_empty() {
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
        return Err(format!("Failed to find pe string: {pe_string:?}"));
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
        return Err(format!("Failed to write header: {:?}", res.err()));
    }
    let res = output.write_all(config_bytes).await;
    if res.is_err() {
        return Err(format!("Failed to write config: {:?}", res.err()));
    }
    // if theme exists, write theme
    if let Some(image) = config.image.as_mut() {
        println!("Writing image...");
        let res = write_file(&mut output, image).await;
        if res.is_err() {
            return Err(format!("Failed to write image: {:?}", res.err()));
        }
    }
    if !files.is_empty() {
        // write index
        println!("Writing index...");
        let index_bytes = match index_to_bin(&index) {
            Ok(bytes) => bytes,
            Err(err) => {
                return Err(format!("{err}"));
            }
        };
        write_header(&mut output, "\0INDEX", index_bytes.len() as u32)
            .await
            .unwrap();

        output.write_all(&index_bytes).await.unwrap();
    }
    // if metadata exists, write metadata
    if let Some(metadata_bytes) = metadata_bytes {
        let res = write_header(&mut output, "\0META", metadata_bytes.len() as u32).await;
        if res.is_err() {
            return Err(format!("Failed to write header: {:?}", res.err()));
        }
        let res = output.write_all(&metadata_bytes).await;
        if res.is_err() {
            return Err(format!("Failed to write metadata: {:?}", res.err()));
        }
    }
    // write files
    for file in files.iter_mut() {
        println!("Writing file: {}", file.name);
        let res = write_file(&mut output, file).await;
        if res.is_err() {
            return Err(format!(
                "Failed to write file {}: {:?}",
                file.name,
                res.err()
            ));
        }
    }
    // flush
    println!("Finalizing...");
    let res = output.flush().await;
    if res.is_err() {
        return Err(format!("Failed to flush: {:?}", res.err()));
    }
    println!("Done");
    Ok(())
}

fn validate_pack_names(config: &PackConfig) -> Result<(), EmbeddedNameError> {
    if let Some(image) = &config.image {
        check_tlv_name(&image.name)?;
        check_index_name(&image.name)?;
    }
    for file in &config.files {
        check_tlv_name(&file.name)?;
        check_index_name(&file.name)?;
    }
    Ok(())
}

pub async fn write_header(
    output: &mut (impl AsyncWrite + std::marker::Unpin),
    name: &str,
    size: u32,
) -> Result<(), tokio::io::Error> {
    check_tlv_name(name).map_err(|err| {
        tokio::io::Error::new(tokio::io::ErrorKind::InvalidInput, err.to_string())
    })?;
    let name = name.as_bytes();
    let namelen = u16::try_from(name.len()).map_err(|_| {
        tokio::io::Error::new(
            tokio::io::ErrorKind::InvalidInput,
            format!(
                "embedded name is {} bytes, limit is {}",
                name.len(),
                u16::MAX
            ),
        )
    })?;
    let size = size.to_be_bytes();
    output.write_all(TLV_MAGIC).await?;
    output.write_all(&namelen.to_be_bytes()).await?;
    output.write_all(name).await?;
    output.write_all(&size).await?;
    Ok(())
}

pub fn index_to_bin(index: &[(String, u32, u32)]) -> Result<Vec<u8>, EmbeddedNameError> {
    let mut data = vec![];
    // u8: name_len var: name u32: size u32: offset
    for (name, size, offset) in index.iter() {
        check_index_name(name)?;
        let name = name.as_bytes();
        data.push(name.len() as u8);
        data.extend_from_slice(name);
        data.extend_from_slice(&size.to_be_bytes());
        data.extend_from_slice(&offset.to_be_bytes());
    }
    Ok(data)
}

pub fn gen_index_header(
    base_end: u32,
    config_end: u32,
    theme_end: u32,
    index_end: u32,
    manifest_end: u32,
) -> Vec<u8> {
    let mut data = LAYOUT_MARKER.to_vec();
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

/// 嵌入安装包的元数据：去掉 packing_info 后按键排序再序列化。
fn embed_metadata_bytes(mut metadata: RepoMetadata) -> Vec<u8> {
    metadata.packing_info.clear();
    let mut metadata = serde_json::json!(metadata);
    metadata.sort_all_objects();
    serde_json::to_string(&metadata).unwrap().into_bytes()
}

fn get_file_pack_priority(
    file_name: &str,
    packing_info: Option<&Vec<Vec<String>>>,
) -> (u8, String) {
    if let Some(info) = packing_info {
        // 检查每个分类
        for (priority, category) in info.iter().enumerate() {
            if category.contains(&file_name.to_string()) {
                return (priority as u8, file_name.to_string());
            }
        }
    }

    // 未分类的文件放在最后，使用原有的字母排序
    (5, file_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::{index_to_bin, write_header};
    use crate::pack_format::{embedded_name::INDEX_NAME_MAX, scan_embedded};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn index_rejects_names_over_255_bytes() {
        let ok = vec![("n".repeat(INDEX_NAME_MAX), 1u32, 0u32)];
        assert!(index_to_bin(&ok).is_ok());
        let over = vec![("n".repeat(INDEX_NAME_MAX + 1), 1u32, 0u32)];
        assert!(index_to_bin(&over).is_err());
    }

    #[tokio::test]
    async fn tlv_name_over_512_bytes_round_trips() {
        let name = "n".repeat(600);
        let mut bytes = b"stub".to_vec();
        write_header(&mut bytes, &name, 3).await.unwrap();
        bytes.write_all(b"abc").await.unwrap();

        let entries = scan_embedded(&bytes);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, name);
        assert_eq!(entries[0].size, 3);
        assert_eq!(&bytes[entries[0].offset..], b"abc");
    }
}
