use std::sync::Arc;

use crate::cli::arg::InstallArgs;
use crate::host::HostHandle;
use crate::installer::config::{resolve_installer_config, InstallerConfig};
use crate::ipc::manager::ManagedElevate;
use crate::session::run::{run_install, run_uninstall};
use crate::session::types::{elevate_from_state, SessionInput, SessionResult, Settings};
use crate::session::ui::{GuiUi, PluginHub, PromptHub};
use crate::session::ProjectConfig;
use crate::utils::error::{TACommandError, TAResult};

#[derive(Clone, Default)]
pub struct SessionState {
    pub prompts: Arc<PromptHub>,
    pub plugins: Arc<PluginHub>,
}

pub async fn start_install(
    input: SessionInput,
    args: &InstallArgs,
    mgr: &ManagedElevate,
    session: &SessionState,
    ui: HostHandle,
) -> TAResult<SessionResult> {
    let config = resolve_installer_config(args.clone(), true)
        .await
        .map_err(TACommandError::new)?;
    let (settings, project) = settings_from_input(&input, args, &config)
        .await
        .map_err(TACommandError::new)?;
    let ui = GuiUi::new(
        ui,
        session.prompts.clone(),
        session.plugins.clone(),
        settings.auto_answer,
    );
    run_install(&settings, &config, &project, &ui, mgr)
        .await
        .map_err(TACommandError::new)
}

pub async fn start_uninstall(
    input: SessionInput,
    args: &InstallArgs,
    mgr: &ManagedElevate,
    session: &SessionState,
    ui: HostHandle,
) -> TAResult<SessionResult> {
    let config = resolve_installer_config(args.clone(), true)
        .await
        .map_err(TACommandError::new)?;
    let (mut settings, project) = settings_from_input(&input, args, &config)
        .await
        .map_err(TACommandError::new)?;
    settings.delete_user_data = input.delete_user_data;
    let ui = GuiUi::new(
        ui,
        session.prompts.clone(),
        session.plugins.clone(),
        settings.auto_answer,
    );
    run_uninstall(&settings, &config, &project, &ui, mgr)
        .await
        .map_err(TACommandError::new)
}

async fn settings_from_input(
    input: &SessionInput,
    args: &InstallArgs,
    config: &InstallerConfig,
) -> anyhow::Result<(Settings, ProjectConfig)> {
    let project = config
        .embedded_config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("安装包损坏，请重新下载"))
        .and_then(ProjectConfig::from_value)?;
    let inspected =
        crate::installer::inspect_dir(input.install_path.clone(), project.exe_name.clone())
            .await
            .ok_or_else(|| anyhow::anyhow!(crate::session::error::PATH_INVALID))?;
    Ok((
        Settings {
            install_path: input.install_path.clone(),
            source_uri: input.source_uri.clone(),
            create_lnk: input.create_lnk,
            delete_user_data: input.delete_user_data,
            mirrorc_cdk: input.mirrorc_cdk.clone().or(args.mirrorc_cdk.clone()),
            online: args.online,
            silent: args.silent,
            non_interactive: args.non_interactive,
            dump_dir: args.dump_dir.clone(),
            dfs_extras: args.dfs_extras.clone(),
            elevate: elevate_from_state(&inspected.state, &project.uac_strategy),
            is_update: inspected.upgrade,
            auto_answer: args.silent || args.non_interactive,
        },
        project,
    ))
}
