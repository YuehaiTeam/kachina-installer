use std::path::PathBuf;

// 本模块同时被 kachina-installer 与 kachina-builder 两个 bin 以 #[path] 共享；
// builder 不构造这些类型，dead_code 只在 builder 侧误报。
#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct UacArgs {
    pub pipe_id: String,
    /// The launching process's log file. The helper may run as another
    /// account, whose `%TEMP%` the launching user cannot read.
    pub log_path: Option<PathBuf>,
}

#[allow(dead_code)]
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
