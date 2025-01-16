#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum IpcOperation {
    Ping,
    InstallFile(super::install_file::InstallFileArgs),
    CreateLnk(crate::installer::lnk::CreateLnkArgs),
    WriteRegistry(crate::installer::registry::WriteRegistryParams),
    CreateUninstaller(crate::installer::uninstall::CreateUninstallerArgs),
    RunUninstall(crate::installer::uninstall::RunUninstallArgs),
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
    }
}
