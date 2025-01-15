#[path = "../../cli/arg.rs"]
pub mod arg;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, clap::Args)]
pub struct PackArgs {
    #[clap(long, short = 'o', default_value = "output.exe")]
    pub output: PathBuf,
    #[clap(long, short = 'c', default_value = ".config.json")]
    pub config: PathBuf,
    #[clap(long, short = 't')]
    pub image: Option<PathBuf>,
    #[clap(long, short = 'm')]
    pub metadata: Option<PathBuf>,
    #[clap(long, short = 'd')]
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct GenArgs {
    #[clap(long, short = 'i')]
    pub input_dir: PathBuf,
    #[clap(long, short = 'm')]
    pub output_metadata: PathBuf,
    #[clap(long, short = 'o')]
    pub output_dir: PathBuf,
    #[clap(long, short = 'r')]
    pub repo: String,
    #[clap(long, short = 't')]
    pub tag: String,
    #[clap(long, short = 'd')]
    pub diff_vers: Option<Vec<String>>,
    #[clap(long, short = 'x')]
    pub diff_ignore: Option<Vec<String>>,
}

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
