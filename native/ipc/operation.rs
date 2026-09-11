use crate::fs::commit::CommitArgs;
use crate::ipc::{IpcResult, ProgressNotify, StagingOpened};
use crate::utils::error::TAResult;

// 外部标签（serde 默认）：内部标签/untagged 会拉进 serde 的 Content 缓冲机制，
// 单态化体积大；此协议仅在同一二进制的两个进程间使用，形状可自由选择
//
// 单字段变体用元组：serde 为每个具名字段变体生成一整套 `__Field` 枚举、字符串
// 匹配器、逐字段 Option 跟踪与缺字段错误路径，这套开销按变体计而非按字段计，
// 单字段变体的性价比最差；而变体名已经说明了那唯一一个字段是什么。
// 多字段变体保留具名形式，字段名在那里是有信息的。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub enum IpcOperation {
    Ping,
    InstallFile(super::install_file::InstallFileArgs),
    InstallMultichunkStream(super::install_file::InstallMultiStreamArgs),
    CreateLnk(crate::installer::lnk::CreateLnkArgs),
    WriteRegistry(crate::installer::registry::WriteRegistryParams),
    StageSelfImage(crate::installer::uninstall::StageSelfImageArgs),
    RunUninstall(crate::installer::uninstall::RunUninstallArgs),
    FindProcessByName(String),
    KillProcess(u32),
    InstallRuntime {
        tag: String,
        offset: Option<usize>,
        size: Option<usize>,
        /// Staging `dl\` directory for the runtime installer download.
        dl_dir: String,
    },
    CheckLocalFiles {
        source: String,
        hash_algorithm: String,
        file_list: Vec<String>,
        #[serde(default)]
        skip_hash: Vec<String>,
    },
    ProbeWritable(Vec<String>),
    RunMirrorcDownload {
        zip_path: String,
        url: String,
    },
    RunMirrorcInstall {
        zip_path: String,
        /// Staging `new\` directory the archive is extracted into.
        new_dir: String,
        /// Expected archive digest from the Mirror酱 API.
        sha256: String,
    },
    /// Find or create the staging directory for `install_dir`.
    OpenStaging(String),
    Commit(CommitArgs),
    Recover(CommitArgs),
    /// Delete a staging directory by root path.
    DiscardStaging(String),
}

pub async fn run_opr(op: IpcOperation, notify: ProgressNotify) -> TAResult<IpcResult> {
    let op_name = match &op {
        IpcOperation::Ping => "Ping",
        IpcOperation::InstallFile(_) => "InstallFile",
        IpcOperation::InstallMultichunkStream(_) => "InstallMultichunkStream",
        IpcOperation::CreateLnk(_) => "CreateLnk",
        IpcOperation::WriteRegistry(_) => "WriteRegistry",
        IpcOperation::StageSelfImage(_) => "StageSelfImage",
        IpcOperation::RunUninstall(_) => "RunUninstall",
        IpcOperation::FindProcessByName(..) => "FindProcessByName",
        IpcOperation::KillProcess(..) => "KillProcess",
        IpcOperation::InstallRuntime { .. } => "InstallRuntime",
        IpcOperation::CheckLocalFiles { .. } => "CheckLocalFiles",
        IpcOperation::ProbeWritable(..) => "ProbeWritable",
        IpcOperation::RunMirrorcDownload { .. } => "RunMirrorcDownload",
        IpcOperation::RunMirrorcInstall { .. } => "RunMirrorcInstall",
        IpcOperation::OpenStaging(..) => "OpenStaging",
        IpcOperation::Commit(..) => "Commit",
        IpcOperation::Recover(..) => "Recover",
        IpcOperation::DiscardStaging(..) => "DiscardStaging",
    };
    tracing::info!("IPC operation: {}", op_name);
    match op {
        IpcOperation::Ping => Ok(IpcResult::Ping),
        IpcOperation::InstallFile(args) => Ok(IpcResult::InstallFile(
            super::install_file::ipc_install_file(args, notify).await?,
        )),
        IpcOperation::InstallMultichunkStream(args) => Ok(IpcResult::InstallMultichunkStream(
            super::install_file::ipc_install_multichunk_stream(args, notify).await?,
        )),
        IpcOperation::WriteRegistry(params) => {
            crate::installer::registry::write_registry_with_params(params).await?;
            Ok(IpcResult::WriteRegistry)
        }
        IpcOperation::StageSelfImage(args) => Ok(IpcResult::StageSelfImage(
            crate::installer::uninstall::stage_self_image(args).await?,
        )),
        IpcOperation::RunUninstall(args) => Ok(IpcResult::RunUninstall(
            crate::installer::uninstall::run_uninstall_with_args(args).await?,
        )),
        IpcOperation::CreateLnk(args) => {
            crate::installer::lnk::create_lnk_with_args(args).await?;
            Ok(IpcResult::CreateLnk)
        }
        IpcOperation::FindProcessByName(name) => Ok(IpcResult::FindProcessByName(
            crate::installer::find_process_by_name(name).await?,
        )),
        IpcOperation::KillProcess(pid) => {
            crate::installer::kill_process(pid).await?;
            Ok(IpcResult::KillProcess)
        }
        IpcOperation::InstallRuntime {
            tag,
            offset,
            size,
            dl_dir,
        } => Ok(IpcResult::InstallRuntime(
            crate::installer::runtimes::install_runtime(tag, offset, size, dl_dir, notify).await?,
        )),
        IpcOperation::CheckLocalFiles {
            source,
            hash_algorithm,
            file_list,
            skip_hash,
        } => Ok(IpcResult::CheckLocalFiles(
            crate::fs::check_local_files(source, hash_algorithm, file_list, skip_hash, notify)
                .await?,
        )),
        IpcOperation::ProbeWritable(file_list) => Ok(IpcResult::ProbeWritable(
            crate::fs::probe_writable(file_list).await,
        )),
        IpcOperation::RunMirrorcDownload { zip_path, url } => {
            crate::thirdparty::mirrorc::run_mirrorc_download(&zip_path, &url, notify).await?;
            Ok(IpcResult::RunMirrorcDownload)
        }
        IpcOperation::RunMirrorcInstall {
            zip_path,
            new_dir,
            sha256,
        } => Ok(IpcResult::RunMirrorcInstall(
            crate::thirdparty::mirrorc::run_mirrorc_install(&zip_path, &new_dir, &sha256, notify)
                .await?,
        )),
        IpcOperation::OpenStaging(install_dir) => {
            let opened = tokio::task::spawn_blocking(move || {
                crate::fs::staging::Staging::open(&install_dir)
            })
            .await
            .map_err(anyhow::Error::from)??;
            Ok(IpcResult::OpenStaging(StagingOpened {
                root: opened.staging.root().to_string_lossy().to_string(),
                journal: opened.journal,
            }))
        }
        IpcOperation::Commit(args) => Ok(IpcResult::Commit(
            crate::fs::commit::commit(args, notify).await?,
        )),
        IpcOperation::Recover(args) => Ok(IpcResult::Recover(
            crate::fs::commit::recover(args, notify).await?,
        )),
        IpcOperation::DiscardStaging(root) => {
            tokio::task::spawn_blocking(move || crate::fs::commit::discard(&root))
                .await
                .map_err(anyhow::Error::from)?;
            Ok(IpcResult::DiscardStaging)
        }
    }
}
