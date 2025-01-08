use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, clap::Args)]
pub struct InstallArgs {
    #[clap(short = 'D', help = "Install directory")]
    pub target: Option<PathBuf>,
    #[clap(short = 'I', help = "Non-interactive install")]
    pub non_interactive: bool,
    #[clap(short = 'S', help = "Silent install")]
    pub slient: bool,
    #[clap(short = 'O', help = "Force online install")]
    pub online: bool,
    #[clap(short = 'U', help = "Uninstall")]
    pub uninstall: bool,
}

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

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    #[clap(hide = true)]
    Install(InstallArgs),
    #[clap(hide = true)]
    Pack(PackArgs),
    #[clap(hide = true)]
    InstallWebview2,
}

#[derive(Parser)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[clap(flatten)]
    pub install: InstallArgs,
}
impl Cli {
    pub fn command(&self) -> Command {
        self.command
            .clone()
            .unwrap_or(Command::Install(self.install.clone()))
    }
}
