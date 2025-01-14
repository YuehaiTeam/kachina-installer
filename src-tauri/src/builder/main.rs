use clap::Parser;
use cli::Command;

mod cli;
mod gen;
mod local;
mod metadata;
mod pack;
mod utils;

pub fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

async fn async_main() {
    let cli = cli::Cli::parse();
    let mut command = cli.command;
    if command.is_none() {
        panic!("No command provided");
    }
    let command = command.take().unwrap();
    match command {
        Command::Pack(args) => pack::pack_cli(args).await,
        Command::Gen(args) => gen::gen_cli(args).await,
    }
}
