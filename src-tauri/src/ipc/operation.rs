#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum IpcOperation {
    Ping,
    InstallFile(super::install_file::InstallFileArgs),
    CreateLnk,
    WriteRegistry,
    CreateUninstaller,
    RunUninstall,
    CheckDirInstallable,
    ReadLocalMetadata,
}

pub async fn run_opr(
    op: IpcOperation,
    notify: impl Fn(serde_json::Value) + std::marker::Send + 'static,
) -> Result<serde_json::Value, String> {
    match op {
        IpcOperation::Ping => Ok(serde_json::value::Value::Null),
        IpcOperation::InstallFile(args) => {
            super::install_file::ipc_install_file(args, notify).await
        }
        _ => Err("Not implemented".to_string()),
    }
}
