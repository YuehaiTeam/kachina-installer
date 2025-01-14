#[path = "../../cli/arg.rs"]
pub mod arg;

use clap::{Parser, Subcommand};

use crate::cli::arg::{GenArgs, PackArgs};

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    Pack(PackArgs),
    Gen(GenArgs),
}

#[derive(Parser)]
#[command(args_conflicts_with_subcommands = true, arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}
