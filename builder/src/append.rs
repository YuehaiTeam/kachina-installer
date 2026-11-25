use tokio::io::AsyncSeekExt;

use crate::{
    cli::AppendArgs,
    pack::{write_file, PackFile},
    pack_format::embedded_name::check_tlv_name,
};

pub async fn append_cli(args: AppendArgs) -> Result<(), String> {
    // files len should equals to names len, or names len should be 0
    if args.file.len() != args.name.len() && !args.name.is_empty() {
        return Err("Files length must equal to names length, or names length must be 0".into());
    }
    let mut names = Vec::with_capacity(args.file.len());
    for (i, file) in args.file.iter().enumerate() {
        let name = if !args.name.is_empty() {
            args.name[i].clone()
        } else {
            file.file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| format!("{} has no usable file name", file.display()))?
                .to_string()
        };
        check_tlv_name(&name).map_err(|err| err.to_string())?;
        names.push(name);
    }
    // open file as append mode
    let mut output = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&args.output)
        .await
        .map_err(|e| format!("Failed to open output file: {e}"))?;
    // seek to the end of the file
    output
        .seek(std::io::SeekFrom::End(0))
        .await
        .map_err(|e| format!("Failed to seek to the end of the file: {e}"))?;
    for (file, name) in args.file.iter().zip(names) {
        let input_stream = tokio::fs::File::open(file)
            .await
            .map_err(|e| format!("Failed to open input file {}: {e}", file.display()))?;
        let input_length = input_stream
            .metadata()
            .await
            .map_err(|e| format!("Failed to get input file metadata: {e}"))?
            .len();
        let size = usize::try_from(input_length)
            .map_err(|_| format!("{name} is too large: {input_length} bytes"))?;
        write_file(
            &mut output,
            &mut PackFile {
                name: name.clone(),
                data: Box::new(input_stream),
                size,
            },
        )
        .await
        .map_err(|e| format!("Failed to write file: {e}"))?;
        println!("Appended file: {name} ({input_length} bytes)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name the installer cannot read is an error, and the output is not
    /// touched.
    #[tokio::test]
    async fn invalid_name_fails_before_writing() {
        let dir = std::env::temp_dir().join(format!("kachina-append-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.exe");
        let input = dir.join("in.bin");
        std::fs::write(&output, b"base").unwrap();
        std::fs::write(&input, b"data").unwrap();
        let err = append_cli(AppendArgs {
            output: output.clone(),
            file: vec![input],
            name: vec!["a/b".into()],
        })
        .await
        .unwrap_err();
        let kept = std::fs::read(&output).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!err.is_empty());
        assert_eq!(kept, b"base");
    }
}
