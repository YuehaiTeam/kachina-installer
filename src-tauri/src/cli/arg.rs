use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct InstallArgs {
    pub target: Option<PathBuf>,
    pub non_interactive: bool,
    pub silent: bool,
    pub online: bool,
    pub uninstall: bool,
    pub source: Option<String>,
    pub dfs_extras: Option<String>,
    pub mirrorc_cdk: Option<String>,
    pub dump_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct UacArgs {
    pub pipe_id: String,
}

#[derive(Debug, Clone)]
pub enum Command {
    Install(InstallArgs),
    InstallWebview2,
    NativeUi(InstallArgs),
    HeadlessUac(UacArgs),
    /// panic hook 拉起的独立崩溃提示进程，本体 abort 后仍存活
    CrashDialog {
        event_id: Option<String>,
    },
}
