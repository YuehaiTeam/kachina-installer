use crate::ipc::{IpcResult, ProgressNotify};
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
    CreateUninstaller(crate::installer::uninstall::CreateUninstallerArgs),
    RunUninstall(crate::installer::uninstall::RunUninstallArgs),
    FindProcessByName(String),
    KillProcess(u32),
    RmList(Vec<String>),
    InstallRuntime {
        tag: String,
        offset: Option<usize>,
        size: Option<usize>,
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
        target_path: String,
    },
}

pub async fn run_opr(op: IpcOperation, notify: ProgressNotify) -> TAResult<IpcResult> {
    let op_name = match &op {
        IpcOperation::Ping => "Ping",
        IpcOperation::InstallFile(_) => "InstallFile",
        IpcOperation::InstallMultichunkStream(_) => "InstallMultichunkStream",
        IpcOperation::CreateLnk(_) => "CreateLnk",
        IpcOperation::WriteRegistry(_) => "WriteRegistry",
        IpcOperation::CreateUninstaller(_) => "CreateUninstaller",
        IpcOperation::RunUninstall(_) => "RunUninstall",
        IpcOperation::FindProcessByName(..) => "FindProcessByName",
        IpcOperation::KillProcess(..) => "KillProcess",
        IpcOperation::RmList(..) => "RmList",
        IpcOperation::InstallRuntime { .. } => "InstallRuntime",
        IpcOperation::CheckLocalFiles { .. } => "CheckLocalFiles",
        IpcOperation::ProbeWritable(..) => "ProbeWritable",
        IpcOperation::RunMirrorcDownload { .. } => "RunMirrorcDownload",
        IpcOperation::RunMirrorcInstall { .. } => "RunMirrorcInstall",
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
        IpcOperation::CreateUninstaller(args) => {
            crate::installer::uninstall::create_uninstaller_with_args(args).await?;
            Ok(IpcResult::CreateUninstaller)
        }
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
        IpcOperation::RmList(list) => {
            let list = list.into_iter().map(std::path::PathBuf::from).collect();
            Ok(IpcResult::RmList(
                crate::installer::uninstall::rm_list(list).await,
            ))
        }
        IpcOperation::InstallRuntime { tag, offset, size } => Ok(IpcResult::InstallRuntime(
            crate::installer::runtimes::install_runtime(tag, offset, size, notify).await?,
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
            target_path,
        } => {
            let (meta, changeset) =
                crate::thirdparty::mirrorc::run_mirrorc_install(&zip_path, &target_path, notify)
                    .await?;
            Ok(IpcResult::RunMirrorcInstall(meta, changeset))
        }
    }
}
