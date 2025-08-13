use clap::Parser;
use cli::Command;

mod append;
mod cli;
mod extract;
mod gen;
mod local;
mod metadata;
mod pack;
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
        Command::Pack(args) => pack::pack_cli(args).await,
        Command::Gen(args) => gen::gen_cli(args).await,
        Command::Append(args) => append::append_cli(args).await,
        Command::Extract(args) => extract::extract_cli(args).await,
        Command::ReplaceBin(args) => {
            if let Err(e) = replace_bin::replace_bin_cli(args).await {
                eprintln!("Replace-bin failed: {}", e);
            }
        }
    }
    let duration = now.elapsed();
    println!("Finished in {duration:?}");
}
