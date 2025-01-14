use clap::Parser;
use pcli::Command;

mod gen;
mod metadata;
mod pack;
mod pcli;
mod local;

#[path = "../cli.rs"]
mod cli;
#[path = "../utils.rs"]
mod utils;

pub fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

async fn async_main() {
    let cli = pcli::Cli::parse();
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
