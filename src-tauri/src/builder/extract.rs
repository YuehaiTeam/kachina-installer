use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};

use crate::{cli::ExtractArgs, local::get_embedded};

pub async fn extract_cli(args: ExtractArgs) {
    // files len should equals to names len, or names len should be 0
    if args.file.len() != args.name.len() && !args.file.is_empty() {
        panic!("Files length must equal to names length, or files length must be 0");
    }
    // open file as mmap
    let mmap = AsyncMmapFile::open(args.input.clone())
        .await
        .map_err(|e| e.to_string())
        .unwrap();
    let embedded = get_embedded(&mmap).await.unwrap();
    // find embedded files with names
    for (i, cli_name) in args.name.iter().enumerate() {
        // replace '\0' with 0x0
        let name = cli_name.replace("\\0", "\0");
        let file = embedded
            .iter()
            .find(|f| f.name == *name)
            .expect("Failed to find embedded file");
        // output file is corespoiding file arg, or default to the name in embedded file
        let mut output_path = args.input.clone();
        let output_name = args.file.get(i).cloned().unwrap_or_else(|| {
            output_path.set_file_name(file.name.clone());
            output_path
        });
        let mut output = tokio::fs::File::create(&output_name)
            .await
            .expect("Failed to create output file");
        let mut data = mmap
            .range_reader(file.offset, file.size)
            .expect("Failed to read embedded file");
        tokio::io::copy(&mut data, &mut output)
            .await
            .expect("Failed to write embedded file");
        println!("Extracted file: {} ({})", file.name, output_name.display());
    }
}
