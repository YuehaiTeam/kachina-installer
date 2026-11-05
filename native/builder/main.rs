use clap::Parser;
use cli::Command;

mod append;
mod cli;
mod extract;
mod gen;
mod local;
mod metadata;
mod pack;
// 与安装器、sfx 共用，各二进制只用到其中一部分
#[path = "../pack_format.rs"]
#[allow(dead_code)]
mod pack_format;
mod replace_bin;
mod utils;

pub fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

async fn async_main() {
    println!("Kachina Builder v{}", env!("CARGO_PKG_VERSION"));
    let now = std::time::Instant::now();
    let cli = cli::Cli::parse();
    let mut command = cli.command;
    if command.is_none() {
        panic!("No command provided");
    }
    let command = command.take().unwrap();
    match command {
        Command::Pack(args) => {
            if let Err(e) = pack::pack_cli(args).await {
                eprintln!("Pack failed: {e}");
                std::process::exit(1);
            }
        }
        Command::Gen(args) => gen::gen_cli(args).await,
        Command::Append(args) => {
            if let Err(e) = append::append_cli(args).await {
                eprintln!("Append failed: {e}");
                std::process::exit(1);
            }
        }
        Command::Extract(args) => {
            if let Err(e) = extract::extract_cli(args).await {
                eprintln!("Extract failed: {e}");
                std::process::exit(1);
            }
        }
        Command::ReplaceBin(args) => {
            if let Err(e) = replace_bin::replace_bin_cli(args).await {
                eprintln!("Replace-bin failed: {e}");
                std::process::exit(1);
            }
        }
    }
    let duration = now.elapsed();
    println!("Finished in {duration:?}");
}
