use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Clone, clap::Args, serde::Serialize)]
pub struct InstallArgs {
    #[clap(short = 'D', help = "Install directory")]
    pub target: Option<PathBuf>,
    #[clap(short = 'I', help = "Non-interactive install")]
    pub non_interactive: bool,
    #[clap(short = 'S', help = "Silent install")]
    pub silent: bool,
    #[clap(short = 'O', help = "Force online install")]
    pub online: bool,
    #[clap(short = 'U', help = "Uninstall")]
    pub uninstall: bool,
    // override install source
    #[clap(long, hide = true)]
    pub source: Option<String>,
    // dfs extra data
    #[clap(long, hide = true)]
    pub dfs_extras: Option<String>,
    // override mirrorc cdk
    #[clap(long, hide = true)]
    pub mirrorc_cdk: Option<String>,
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
