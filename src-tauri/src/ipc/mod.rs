pub mod install_file;
pub mod manager;
pub mod operation;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dfs::InsightItem;
use crate::fs::Metadata as LocalFileMeta;
use crate::thirdparty::mirrorc::MirrorcChangeset;
use crate::utils::error::TACommandError;
use crate::utils::metadata::RepoMetadata;

use install_file::{InstallResult, MultichunkResult};

// 单字段变体与 `Chunk` 用元组：serde 为每个具名字段变体生成一整套 `__Field`
// 枚举、字符串匹配器、逐字段 Option 跟踪与缺字段错误路径，这套开销按变体计而非
// 按字段计。`Chunk` 的 u32/u64 不同型，位置写反编译不过；`BytesOf`/`CountOf`/
// `Extract` 的同型字段保留具名，写反不会被编译器发现。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Bytes(u64),
    Chunk(u32, u64),
    BytesOf { done: u64, total: u64 },
    CountOf { done: u64, total: u64 },
    Extract { file: String, done: u64, total: u64 },
    Delete(String),
}

pub type ProgressNotify = Arc<dyn Fn(Progress) + Send + Sync>;

pub fn progress_notify(f: impl Fn(Progress) + Send + Sync + 'static) -> ProgressNotify {
    Arc::new(f)
}

pub fn progress_noop() -> ProgressNotify {
    progress_notify(|_| {})
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IpcError {
    pub message: String,
    pub insight: Option<InsightItem>,
}

impl IpcError {
    pub fn from_ta(err: &TACommandError) -> Self {
        let _ = serde_json::to_value(err);
        Self {
            message: format!("{:#}", err.error),
            insight: err.insight.clone(),
        }
    }

    pub fn into_ta(self) -> TACommandError {
        TACommandError {
            error: anyhow::anyhow!(self.message),
            insight: self.insight,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum IpcResult {
    Ping,
    InstallFile(InstallResult),
    InstallMultichunkStream(MultichunkResult),
    CreateLnk,
    WriteRegistry,
    CreateUninstaller,
    RunUninstall(Vec<String>),
    FindProcessByName(Vec<(u32, String)>),
    KillProcess,
    RmList(Vec<String>),
    InstallRuntime(String),
    CheckLocalFiles(Vec<LocalFileMeta>),
    ProbeWritable(Vec<String>),
    RunMirrorcDownload,
    RunMirrorcInstall(Option<RepoMetadata>, Option<MirrorcChangeset>),
}

impl IpcResult {
    pub fn insight(&self) -> Option<InsightItem> {
        match self {
            IpcResult::InstallFile(r) => r.insight.clone(),
            IpcResult::InstallMultichunkStream(r) => Some(r.insight.clone()),
            _ => None,
        }
    }
}

// 前三个变体用元组而非具名字段：`id`/`data`/`error` 是信封位置，不带领域含义，
// 而 serde 为每个具名字段变体额外生成 `__Field` 枚举、字符串匹配器、逐字段
// Option 跟踪与缺字段错误路径。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PipeMsg {
    Progress(String, Progress),
    Ok(String, IpcResult),
    Err(String, IpcError),
    Envelope(String),
    Breadcrumb(Value),
    Disconnect(String),
}
