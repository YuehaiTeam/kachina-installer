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
    let handle = ui;
    let ui = GuiUi::new(
        handle.clone(),
        session.prompts.clone(),
        session.plugins.clone(),
        settings.auto_answer,
    );
    run_install(&settings, &config, &project, &ui, mgr)
        .await
        .map_err(|e| {
            emit_cdk_reopen(&handle, &e);
            TACommandError::new(e)
        })
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

pub(crate) async fn settings_from_input(
    input: &SessionInput,
    args: &InstallArgs,
    config: &InstallerConfig,
) -> anyhow::Result<(Settings, ProjectConfig)> {
    let project = config
        .embedded_config
        .as_ref()
        .ok_or_else(|| anyhow::Error::from(crate::utils::code::Coded::bare(crate::utils::code::PKG_BROKEN)))
        .and_then(ProjectConfig::from_value)?;
    let inspected =
        crate::installer::inspect_dir(input.install_path.clone(), project.exe_name.clone())
            .await
            .ok_or_else(|| anyhow::Error::from(crate::utils::code::Coded::bare(crate::utils::code::INSTALL_PATH_INVALID)))?;
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

fn emit_cdk_reopen(handle: &HostHandle, err: &anyhow::Error) {
    use crate::utils::code::{extract, Extracted, MIRRORC_CDK_BANNED, MIRRORC_CDK_EXPIRED, MIRRORC_CDK_INVALID, MIRRORC_CDK_MISMATCH};
    if let Extracted::Coded(c) = extract(err) {
        if matches!(
            c.code,
            MIRRORC_CDK_EXPIRED | MIRRORC_CDK_INVALID | MIRRORC_CDK_MISMATCH | MIRRORC_CDK_BANNED
        ) {
            handle.emit("session-reopen-source", ());
        }
    }
}
