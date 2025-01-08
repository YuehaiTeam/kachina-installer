use fmmap::tokio::{AsyncMmapFileExt, AsyncMmapFileReader};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::{
    cli::PackArgs,
    local::{get_embedded, mmap},
};

pub struct PackFile {
    pub name: String,
    pub size: usize,
    pub data: Box<dyn AsyncRead + Unpin + Send>,
}

pub struct PackConfig {
    pub config: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
    pub image: Option<PackFile>,
    pub files: Vec<PackFile>,
}

pub async fn pack_cli(args: PackArgs) {
    let embedded = get_embedded().await.map_err(|e| e.to_string()).unwrap();
    if !embedded.is_empty() {
        eprintln!("Already packed");
        return;
    }
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
        let json = serde_json::from_slice(&metadataf);
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
            name: ".image".to_string(),
            size: image_size as usize,
            data: Box::new(imagef.unwrap()) as Box<dyn AsyncRead + Unpin + Send>,
        })
    } else {
        None
    };
    let data_dir = args.data_dir;
    let mut files = vec![];
    if let Some(data_dir) = data_dir {
        let entries = tokio::fs::read_dir(data_dir).await;
        if entries.is_err() {
            eprintln!("Failed to read data dir: {:?}", entries.err());
            return;
        }
        let mut entries = entries.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
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
    let config = PackConfig {
        config,
        metadata,
        image,
        files,
    };
    let input = mmap().await.reader(0);
    if input.is_err() {
        eprintln!("Failed to open input: {:?}", input.err());
        return;
    }
    let input = input.unwrap();
    let output = tokio::fs::File::create(args.output).await.unwrap();
    println!(
        "Packing: image: {:?}, metadata: {:?}, files: {}",
        config.metadata.is_some(),
        config.image.is_some(),
        config.files.len()
    );
    pack(input, output, config).await;
}

pub async fn pack(
    mut base: impl AsyncRead + std::marker::Unpin,
    mut output: impl AsyncWrite + std::marker::Unpin,
    mut config: PackConfig,
) {
    // copy base to output, not closing output file
    println!("Packing base...");
    tokio::io::copy(&mut base, &mut output).await.unwrap();
    // write config
    println!("Writing config...");
    config.config.sort_all_objects();
    let config_bytes = serde_json::to_string(&config.config).unwrap();
    let config_bytes = config_bytes.as_bytes();
    let res = write_header(&mut output, ".config.json", config_bytes.len() as u32).await;
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
    // if metadata exists, write metadata
    if let Some(mut metadata) = config.metadata {
        println!("Writing metadata...");
        metadata.sort_all_objects();
        let metadata_bytes = serde_json::to_string(&metadata).unwrap();
        let metadata_bytes = metadata_bytes.as_bytes();
        let res = write_header(&mut output, ".metadata.json", metadata_bytes.len() as u32).await;
        if res.is_err() {
            eprintln!("Failed to write header: {:?}", res.err());
            return;
        }
        let res = output.write_all(metadata_bytes).await;
        if res.is_err() {
            eprintln!("Failed to write metadata: {:?}", res.err());
            return;
        }
    }
    // write files
    let mut files = config.files;
    files.sort_by_key(|x| x.name.clone());
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
    let header = "!ins".to_ascii_uppercase();
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

pub async fn write_file(
    output: &mut (impl AsyncWrite + std::marker::Unpin),
    file: &mut PackFile,
) -> Result<(), tokio::io::Error> {
    write_header(output, &file.name, file.size as u32).await?;
    tokio::io::copy(&mut file.data, output).await?;
    Ok(())
}

pub async fn get_base_with_config() -> Result<AsyncMmapFileReader<'static>, String> {
    let embedded = get_embedded().await?;
    let config_index = embedded.iter().position(|x| x.name == ".config.json");
    let image_index = embedded.iter().position(|x| x.name == ".image");
    if config_index.is_none() {
        if embedded.is_empty() {
            return mmap().await.reader(0).map_err(|e| e.to_string());
        }
        return Err("Malformed packed files: missing config".to_string());
    }
    let config_index = config_index.unwrap();
    // config index should be 0
    if config_index != 0 {
        return Err("Malformed packed files: config not at index 0".to_string());
    }
    let mut end_pos = embedded[config_index].offset + embedded[config_index].size;
    if let Some(image_index) = image_index {
        // image index should be 1
        if image_index != 1 {
            return Err("Malformed packed files: image not at index 1".to_string());
        }
        end_pos = embedded[image_index].offset + embedded[image_index].size;
    }
    mmap()
        .await
        .range_reader(0, end_pos)
        .map_err(|e| e.to_string())
}
