use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Clone, clap::Args, serde::Serialize)]
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
    #[clap(long, short = 'z')]
    pub hdiffz: Option<String>,
}
#[derive(Debug, Clone, clap::Args)]
pub struct UacArgs {
    pub pipe_id: String,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    #[clap(hide = true)]
    Install(InstallArgs),
    #[clap(hide = true)]
    InstallWebview2,
    #[clap(hide = true)]
    HeadlessUac(UacArgs),
}
