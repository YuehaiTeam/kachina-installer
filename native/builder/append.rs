use tokio::io::AsyncSeekExt;

use crate::{
    cli::AppendArgs,
    local::check_tlv_name,
    pack::{write_file, PackFile},
};

pub async fn append_cli(args: AppendArgs) {
    // files len should equals to names len, or names len should be 0
    if args.file.len() != args.name.len() && !args.name.is_empty() {
        panic!("Files length must equal to names length, or names length must be 0");
    }
    let mut names = Vec::with_capacity(args.file.len());
    for (i, file) in args.file.iter().enumerate() {
        let name = if !args.name.is_empty() {
            args.name[i].clone()
        } else {
            file.file_name()
                .and_then(|s| s.to_str())
                .unwrap()
                .to_string()
        };
        if let Err(err) = check_tlv_name(&name) {
            panic!("{err}");
        }
        names.push(name);
    }
    // open file as append mode
    let mut output = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&args.output)
        .await
        .expect("Failed to open output file");
    // seek to the end of the file
    output
        .seek(std::io::SeekFrom::End(0))
        .await
        .expect("Failed to seek to the end of the file");
    for (file, name) in args.file.iter().zip(names) {
        let input_stream = tokio::fs::File::open(file)
            .await
            .expect("Failed to open input file");
        let input_length = input_stream
            .metadata()
            .await
            .expect("Failed to get input file metadata")
            .len();
        println!("Appended file: {name} ({input_length} bytes)");
        write_file(
            &mut output,
            &mut PackFile {
                name,
                data: Box::new(input_stream),
                size: input_length.try_into().expect("File size too large"),
            },
        )
        .await
        .expect("Failed to write file");
    }
}
