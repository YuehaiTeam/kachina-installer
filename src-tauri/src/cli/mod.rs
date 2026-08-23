pub mod arg;
use arg::{Command, InstallArgs};
use clap::Parser;

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
