use crate::utils::error::{IntoTAResult, TAResult};
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum IpcOperation {
    Ping,
    InstallFile(super::install_file::InstallFileArgs),
    CreateLnk(crate::installer::lnk::CreateLnkArgs),
    WriteRegistry(crate::installer::registry::WriteRegistryParams),
    CreateUninstaller(crate::installer::uninstall::CreateUninstallerArgs),
    RunUninstall(crate::installer::uninstall::RunUninstallArgs),
    FindProcessByName {
        name: String,
    },
    KillProcess {
        pid: u32,
    },
    RmList {
        list: Vec<String>,
    },
    InstallRuntime {
        tag: String,
    },
    CheckLocalFiles {
        source: String,
        hash_algorithm: String,
        file_list: Vec<String>,
    },
    PatchInstaller {
        installer: String,
    },
}

pub async fn run_opr(
    op: IpcOperation,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
    context: Vec<(String, String)>,
) -> TAResult<serde_json::Value> {
    let op_name = match &op {
        IpcOperation::Ping => "Ping",
        IpcOperation::InstallFile(_) => "InstallFile",
        IpcOperation::CreateLnk(_) => "CreateLnk",
        IpcOperation::WriteRegistry(_) => "WriteRegistry",
        IpcOperation::CreateUninstaller(_) => "CreateUninstaller",
        IpcOperation::RunUninstall(_) => "RunUninstall",
        IpcOperation::FindProcessByName { .. } => "FindProcessByName",
        IpcOperation::KillProcess { .. } => "KillProcess",
        IpcOperation::RmList { .. } => "RmList",
        IpcOperation::InstallRuntime { .. } => "InstallRuntime",
        IpcOperation::CheckLocalFiles { .. } => "CheckLocalFiles",
        IpcOperation::PatchInstaller { .. } => "PatchInstaller",
    };
    tracing::info!("IPC operation: {}", op_name);
    let ctx_str = context
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();
    let tx_ctx = sentry::TransactionContext::continue_from_headers(op_name, op_name, ctx_str);
    let transaction = sentry::start_transaction(tx_ctx);
    let ret = match op {
        IpcOperation::Ping => Ok(serde_json::value::Value::Null),
        IpcOperation::InstallFile(args) => super::install_file::ipc_install_file(args, notify)
            .await
            .into_ta_result(),
        IpcOperation::WriteRegistry(params) => {
            crate::installer::registry::write_registry_with_params(params).await?;
            Ok(serde_json::Value::Null)
        }
        IpcOperation::CreateUninstaller(args) => {
            crate::installer::uninstall::create_uninstaller_with_args(args).await?;
            Ok(serde_json::Value::Null)
        }
        IpcOperation::RunUninstall(args) => Ok(serde_json::json!(
            crate::installer::uninstall::run_uninstall_with_args(args).await?
        )),
        IpcOperation::CreateLnk(args) => {
            crate::installer::lnk::create_lnk_with_args(args).await?;
            Ok(serde_json::Value::Null)
        }
        IpcOperation::FindProcessByName { name } => Ok(serde_json::json!(
            crate::installer::find_process_by_name(name).await?
        )),
        IpcOperation::KillProcess { pid } => Ok(serde_json::json!(
            crate::installer::kill_process(pid).await?
        )),
        IpcOperation::RmList { list } => {
            let list = list.into_iter().map(std::path::PathBuf::from).collect();
            Ok(serde_json::json!(
                crate::installer::uninstall::rm_list(list).await
            ))
        }
        IpcOperation::InstallRuntime { tag } => Ok(serde_json::json!(
            crate::installer::runtimes::install_runtime(tag, notify).await?
        )),
        IpcOperation::CheckLocalFiles {
            source,
            hash_algorithm,
            file_list,
        } => Ok(serde_json::json!(
            crate::fs::check_local_files(source, hash_algorithm, file_list, notify).await?
        )),
        IpcOperation::PatchInstaller { installer } => Ok(serde_json::json!(
            crate::installer::uninstall::clear_index_mark(&std::path::PathBuf::from(installer))
                .await?
        )),
    };
    transaction.finish();
    ret
}
