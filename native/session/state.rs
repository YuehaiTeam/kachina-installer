//! UI session machine. Rust holds the only copy of `UiState`; renderers apply
//! it wholesale. Named `UiSession` because `session::commands::SessionState`
//! already exists (PromptHub + PluginHub).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use crate::installer::registry::{plan_from_identity, HiveRead, Identity, RegHive};
use crate::installer::registry::{plan_registry, RegistryPlan};
use crate::installer::{probe_dir, DirState};
use crate::session::types::{elevate_from_state, SessionResult};
use crate::utils::code::{Coded, MIRRORC_CDK_MISSING};

macro_rules! progress_stages {
    ($($variant:ident => $snake:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ProgressStage {
            $($variant,)+
        }

        impl ProgressStage {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $snake,)+
                }
            }

            pub fn i18n_key(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("progress.", $snake),)+
                }
            }
        }
    };
}

progress_stages! {
    Prepare => "prepare",
    FetchMetadata => "fetch_metadata",
    ScanFiles => "scan_files",
    PrepareDownload => "prepare_download",
    CreateDownloadSession => "create_download_session",
    ProcessFiles => "process_files",
    DownloadArchive => "download_archive",
    VerifyArchive => "verify_archive",
    ExtractArchive => "extract_archive",
    Commit => "commit",
    DownloadRuntime => "download_runtime",
    InstallRuntime => "install_runtime",
    CreateShortcuts => "create_shortcuts",
    WriteRegistry => "write_registry",
    Finalize => "finalize",
    UninstallScan => "uninstall_scan",
    UninstallDelete => "uninstall_delete",
}

pub const PROMPT_KEYS: &[&str] = &[
    "prompt.process_running.title",
    "prompt.process_running.message",
    "prompt.occupied_files.title",
    "prompt.occupied_files.message",
    "prompt.version_mismatch.title",
    "prompt.version_mismatch.message",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    Native,
    WebView,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiState {
    pub phase: Phase,
    pub mode: Mode,
    /// 离线整包（带 embedded_index）不含联网下载路径，渲染端据此隐藏
    /// 安装源选择入口，避免切到需要联网/CDK 的源。
    pub offline: bool,
    pub project: ProjectView,
    pub options: Options,
    pub sources: Vec<SourceItem>,
    pub path: PathState,
    pub needs_elevate: bool,
    pub cdk: CdkStatus,
    pub theme: Theme,
    pub pending: Option<Prompt>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase {
    Ready,
    Running(Progress),
    Done(SessionResult),
    Failed(Coded),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Install,
    Update,
    Uninstall,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub window_title: String,
    pub title: String,
    pub description: String,
    pub borderless: bool,
    pub lang: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Options {
    pub install_path: String,
    pub source_uri: String,
    pub create_lnk: bool,
    pub delete_user_data: bool,
    pub mirrorc_cdk: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceItem {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub icon: Option<String>,
    pub requires_webview: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PathState {
    pub writable: PathWritable,
    pub exists: bool,
    pub upgrade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathWritable {
    Writable,
    Unwritable,
    Private,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CdkStatus {
    Idle,
    Checking,
    Ok,
    Invalid(Coded),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    None,
    Image,
    Css,
    Html,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub step: Option<u8>,
    pub stage: ProgressStage,
    pub subject: Option<String>,
    pub percent: Option<f64>,
    pub cancel: CancelState,
    pub summary: Option<ProgressCounter>,
    pub processing_bps: Option<u64>,
    pub network_bps: Option<u64>,
    pub network_pending: bool,
    pub files: Vec<FileProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    Available,
    Requested,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressCounter {
    pub unit: ProgressUnit,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressUnit {
    Bytes,
    Files,
    Operations,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileProgress {
    pub id: u32,
    pub name: String,
    pub action: FileAction,
    pub bytes: Option<ByteProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    Download,
    Extract,
    Patch,
    Verify,
    Flush,
    Retry,
}

#[derive(Debug, Clone, Serialize)]
pub struct ByteProgress {
    pub done: u64,
    pub total: Option<u64>,
}

impl ProgressStage {
    pub fn cancellable(self) -> bool {
        !matches!(
            self,
            Self::Commit
                | Self::DownloadRuntime
                | Self::InstallRuntime
                | Self::CreateShortcuts
                | Self::WriteRegistry
                | Self::Finalize
                | Self::UninstallScan
                | Self::UninstallDelete
        )
    }
    pub fn unit(self) -> ProgressUnit {
        match self {
            Self::ProcessFiles | Self::DownloadArchive | Self::DownloadRuntime => {
                ProgressUnit::Bytes
            }
            Self::Commit => ProgressUnit::Operations,
            _ => ProgressUnit::Files,
        }
    }
}
impl FileAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Extract => "extract",
            Self::Patch => "patch",
            Self::Verify => "verify",
            Self::Flush => "flush",
            Self::Retry => "retry",
        }
    }
}
impl Progress {
    pub fn new(stage: ProgressStage, step: Option<u8>, percent: Option<f64>) -> Self {
        Self {
            stage,
            step,
            percent: percent
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 100.0)),
            subject: None,
            cancel: if stage.cancellable() {
                CancelState::Available
            } else {
                CancelState::Unavailable
            },
            summary: None,
            processing_bps: None,
            network_bps: None,
            network_pending: false,
            files: Vec::new(),
        }
    }

    /// A fresh stage starts as `Available`. Once the user has cancelled, later
    /// snapshots stay `Requested` until commit marks cancel unavailable.
    pub fn apply_user_cancel(&mut self, cancelled: bool) {
        if cancelled && self.cancel == CancelState::Available {
            self.cancel = CancelState::Requested;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Prompt {
    pub id: String,
    pub kind: &'static str,
    pub items: Vec<String>,
    pub params: BTreeMap<&'static str, String>,
}

/// Wire shape is `{ "kind": "<snake_case variant>", ...fields }`.
#[derive(Debug, Clone)]
pub enum Intent {
    SetPath { path: String },
    SetSource { uri: String },
    SetCreateLnk { value: bool },
    SetDeleteUserData { value: bool },
    SetCdk { cdk: String, uri: Option<String> },
    CancelCdk,
    Start,
    Cancel,
    Answer { id: String, ok: bool },
    Dismiss,
    Launch,
    Advanced,
    Close,
}

impl Intent {
    /// Hand-written instead of `#[derive(Deserialize)]` with `tag = "kind"`: an
    /// internally tagged derive drags serde's `Content` buffering in (about
    /// 14 KiB of .text for this enum alone). Keep `intent_from_value_covers_every_variant`
    /// in sync when adding a variant.
    pub fn from_value(v: &Value) -> anyhow::Result<Self> {
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .context("intent: missing kind")?;
        let text = |name: &str| -> anyhow::Result<String> {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("intent {kind}: missing string field {name}"))
        };
        let flag = |name: &str| -> anyhow::Result<bool> {
            v.get(name)
                .and_then(Value::as_bool)
                .with_context(|| format!("intent {kind}: missing bool field {name}"))
        };
        Ok(match kind {
            "set_path" => Intent::SetPath {
                path: text("path")?,
            },
            "set_source" => Intent::SetSource { uri: text("uri")? },
            "set_create_lnk" => Intent::SetCreateLnk {
                value: flag("value")?,
            },
            "set_delete_user_data" => Intent::SetDeleteUserData {
                value: flag("value")?,
            },
            "set_cdk" => Intent::SetCdk {
                cdk: text("cdk")?,
                uri: v
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            },
            "cancel_cdk" => Intent::CancelCdk,
            "start" => Intent::Start,
            "cancel" => Intent::Cancel,
            "answer" => Intent::Answer {
                id: text("id")?,
                ok: flag("ok")?,
            },
            "dismiss" => Intent::Dismiss,
            "launch" => Intent::Launch,
            "advanced" => Intent::Advanced,
            "close" => Intent::Close,
            other => anyhow::bail!("intent: unknown kind {other}"),
        })
    }
}

/// Holds `UiState` and applies user intents. `apply` is sync this step:
/// option / path / source / cdk-gate / answer / dismiss. `Start` only gates
/// mirrorc CDK; it does not call `run_install`.
pub struct UiSession {
    pub state: UiState,
    all_sources: Vec<SourceItem>,
    renderer: Renderer,
    exe_name: String,
    #[allow(dead_code)] // native ReadyState (step 4); GUI pick_path uses ProjectConfig.app_name
    app_name: String,
    uac_strategy: String,
    discovered_path: String,
    reg_name: String,
    pub registry: Result<RegistryPlan, Coded>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            phase: Phase::Ready,
            mode: Mode::Install,
            project: ProjectView {
                window_title: String::new(),
                title: String::new(),
                description: String::new(),
                borderless: false,
                lang: "zh-CN".into(),
            },
            options: Options {
                install_path: String::new(),
                source_uri: String::new(),
                create_lnk: true,
                delete_user_data: false,
                mirrorc_cdk: None,
            },
            sources: Vec::new(),
            offline: false,
            path: PathState {
                writable: PathWritable::Writable,
                exists: false,
                upgrade: false,
            },
            needs_elevate: false,
            cdk: CdkStatus::Idle,
            theme: Theme::None,
            pending: None,
        }
    }
}

impl UiSession {
    pub fn new(state: UiState) -> Self {
        Self::with_renderer(state, Renderer::Native)
    }

    pub fn with_renderer(state: UiState, renderer: Renderer) -> Self {
        Self::with_project(
            state,
            renderer,
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        )
    }

    pub fn with_project(
        state: UiState,
        renderer: Renderer,
        exe_name: String,
        app_name: String,
        uac_strategy: String,
        discovered_path: String,
        reg_name: String,
    ) -> Self {
        let all_sources = state.sources.clone();
        let mut sess = Self {
            state,
            all_sources,
            renderer,
            exe_name,
            app_name,
            uac_strategy,
            discovered_path,
            reg_name,
            registry: Ok(RegistryPlan::default()),
        };
        sess.refresh_sources();
        sess
    }

    pub fn apply(&mut self, intent: Intent) {
        match intent {
            Intent::SetPath { path } => {
                self.state.options.install_path = path;
                self.recompute_path();
            }
            Intent::SetSource { uri } => {
                // 换到另一条 mirrorc URI 才作废旧校验；同 URI 重选必须保住 Ok。
                let changed = self.state.options.source_uri != uri;
                self.state.options.source_uri = uri;
                if changed && self.state.options.source_uri.starts_with("mirrorc://") {
                    self.state.cdk = CdkStatus::Idle;
                }
                self.refresh_sources();
            }
            Intent::SetCreateLnk { value } => {
                self.state.options.create_lnk = value;
            }
            Intent::SetDeleteUserData { value } => {
                self.state.options.delete_user_data = value;
            }
            Intent::SetCdk { cdk, .. } => {
                self.state.options.mirrorc_cdk = if cdk.is_empty() { None } else { Some(cdk) };
            }
            Intent::CancelCdk => {}
            Intent::Start => {
                // 卸载不下载任何文件（静默卸载直接走 run_uninstall），CDK 校验
                // 只属于确实需要下载的安装/更新路径，否则 GUI 卸载会被卡在
                // CDK 面板，三个入口行为不一致。
                if !matches!(self.state.mode, Mode::Uninstall)
                    && self.state.options.source_uri.starts_with("mirrorc://")
                    && !matches!(self.state.cdk, CdkStatus::Ok)
                {
                    self.state.phase = Phase::Failed(Coded::bare(MIRRORC_CDK_MISSING));
                }
                // run_install / run_uninstall are step 2.
            }
            // The answer itself goes to `PromptHub`; the running session decides what
            // follows (cancelled → `Ready`), so only the modal is cleared here.
            Intent::Answer { id, .. } => {
                if self.state.pending.as_ref().is_some_and(|p| p.id == id) {
                    self.state.pending = None;
                }
            }
            Intent::Dismiss => {
                if matches!(self.state.phase, Phase::Failed(_)) {
                    self.state.phase = Phase::Ready;
                }
            }
            // Cancel / Launch / Advanced / Close: host-owned this step.
            Intent::Cancel | Intent::Launch | Intent::Advanced | Intent::Close => {}
        }
    }

    fn refresh_sources(&mut self) {
        self.state.sources = self
            .all_sources
            .iter()
            .filter(|s| !(matches!(self.renderer, Renderer::Native) && s.requires_webview))
            .cloned()
            .collect();
    }

    /// Same probe as `Settings` uses (`installer::probe_dir`), so what the
    /// renderer shows and what the session runs with cannot disagree. A path
    /// that is an existing file is treated as unwritable.
    fn recompute_path(&mut self) {
        let probe = probe_dir(Path::new(&self.state.options.install_path), &self.exe_name);
        let state = probe.map(|p| p.state()).unwrap_or(DirState::Unwritable);
        let upgrade = probe.is_some_and(|p| p.upgrade);
        if matches!(self.state.mode, Mode::Uninstall) {
            // Uninstall is a session kind, not derived from the path.
        } else if upgrade {
            self.state.mode = Mode::Update;
        } else {
            self.state.mode = Mode::Install;
        }
        self.apply_registry(upgrade, &state);
        self.state.path = PathState {
            writable: PathWritable::from(state),
            exists: probe.is_some_and(|p| p.exists),
            upgrade,
        };
    }

    fn apply_registry(&mut self, is_update: bool, dir_state: &DirState) {
        let dir_elevate = elevate_from_state(dir_state, &self.uac_strategy);
        let plan = plan_registry(
            &self.reg_name,
            &self.exe_name,
            &self.discovered_path,
            &self.state.options.install_path,
            is_update,
            dir_elevate,
        );
        self.set_registry_plan(plan, dir_elevate);
    }

    fn set_registry_plan(&mut self, plan: Result<RegistryPlan, Coded>, dir_elevate: bool) {
        self.state.needs_elevate = plan.as_ref().map_or(dir_elevate, |p| p.elevate);
        self.registry = plan;
    }

    #[cfg(test)]
    fn force_identity(&mut self, identity: Identity) {
        let probe = probe_dir(Path::new(&self.state.options.install_path), &self.exe_name);
        let state = probe.map(|p| p.state()).unwrap_or(DirState::Unwritable);
        let upgrade = probe.is_some_and(|p| p.upgrade);
        let dir_elevate = elevate_from_state(&state, &self.uac_strategy);
        let plan = plan_from_identity(
            identity,
            &self.reg_name,
            &self.exe_name,
            &self.discovered_path,
            &self.state.options.install_path,
            upgrade,
            dir_elevate,
        );
        self.set_registry_plan(plan, dir_elevate);
    }
}

impl From<DirState> for PathWritable {
    fn from(state: DirState) -> Self {
        match state {
            DirState::Writable => PathWritable::Writable,
            DirState::Unwritable => PathWritable::Unwritable,
            DirState::Private => PathWritable::Private,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn stage_names_follow_one_list() {
        for stage in ProgressStage::ALL {
            assert_eq!(serde_json::to_value(stage).unwrap(), stage.as_str());
            assert_eq!(stage.i18n_key(), format!("progress.{}", stage.as_str()));
        }
    }

    #[test]
    fn later_progress_keeps_a_requested_cancel() {
        let mut progress = Progress::new(ProgressStage::ProcessFiles, Some(2), Some(1.0));
        progress.apply_user_cancel(false);
        assert_eq!(progress.cancel, CancelState::Available);
        progress.apply_user_cancel(true);
        assert_eq!(progress.cancel, CancelState::Requested);
        progress.cancel = CancelState::Unavailable;
        progress.apply_user_cancel(true);
        assert_eq!(progress.cancel, CancelState::Unavailable);
    }

    fn scratch_dir() -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/ui-session-tests");
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join(format!("t-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn deny_write(dir: &Path) {
        let user = whoami::username();
        let status = Command::new("icacls")
            .arg(dir)
            .args(["/deny", &format!("{user}:(W,DC,AD)")])
            .status()
            .expect("icacls deny");
        assert!(status.success(), "icacls deny failed: {status}");
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(dir, perms);
    }

    fn restore_write(dir: &Path) {
        let user = whoami::username();
        let _ = Command::new("icacls")
            .arg(dir)
            .args(["/remove:d", &user])
            .status();
        if let Ok(meta) = std::fs::metadata(dir) {
            let mut perms = meta.permissions();
            // Windows: clears FILE_ATTRIBUTE_READONLY so the test directory can be deleted.
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(dir, perms);
        }
    }

    fn session_with_sources(sources: Vec<SourceItem>) -> UiSession {
        let state = UiState {
            sources,
            ..Default::default()
        };
        UiSession::new(state)
    }

    /// `prefer-user` is the strategy where writability decides elevation; the
    /// default `prefer-admin` elevates for every non-private directory.
    #[test]
    fn set_path_readonly_needs_elevate_and_mode_follows_upgrade() {
        let dir = scratch_dir();
        let mut sess = UiSession::with_project(
            UiState::default(),
            Renderer::Native,
            "app.exe".into(),
            "App".into(),
            "prefer-user".into(),
            String::new(),
            String::new(),
        );

        sess.apply(Intent::SetPath {
            path: dir.to_string_lossy().into_owned(),
        });
        assert!(
            !sess.state.needs_elevate,
            "fresh scratch dir should be writable, path={:?}",
            sess.state.path
        );
        assert_eq!(sess.state.mode, Mode::Install);
        assert!(!sess.state.path.upgrade);

        std::fs::write(dir.join("app.exe"), b"1").unwrap();
        sess.apply(Intent::SetPath {
            path: dir.to_string_lossy().into_owned(),
        });
        assert_eq!(sess.state.mode, Mode::Update);
        assert!(sess.state.path.upgrade);
        assert!(
            !sess.state.needs_elevate,
            "an existing install must not force elevate on a writable dir"
        );

        deny_write(&dir);
        sess.apply(Intent::SetPath {
            path: dir.to_string_lossy().into_owned(),
        });
        let elevate = sess.state.needs_elevate;
        let writable = sess.state.path.writable;
        let mode = sess.state.mode;
        restore_write(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            elevate,
            "read-only dir must need elevate, writable={writable:?}"
        );
        assert!(
            matches!(writable, PathWritable::Unwritable | PathWritable::Private),
            "expected Unwritable/Private, got {writable:?}"
        );
        assert_eq!(
            mode,
            Mode::Update,
            "mode still follows upgrade after readonly"
        );
    }

    fn present(location: &str) -> HiveRead {
        HiveRead::Present {
            location: location.into(),
            meta: Some("{}".into()),
        }
    }

    #[test]
    fn writable_dir_with_hklm_record_needs_elevate() {
        let dir = scratch_dir();
        let path = dir.to_string_lossy().into_owned();
        let mut sess = UiSession::with_project(
            UiState::default(),
            Renderer::Native,
            "app.exe".into(),
            "App".into(),
            "prefer-user".into(),
            path.clone(),
            "App".into(),
        );
        sess.apply(Intent::SetPath { path: path.clone() });
        assert!(!sess.state.needs_elevate);
        sess.force_identity(Identity {
            hkcu: HiveRead::Absent,
            hklm: present(&path),
        });
        let elevate = sess.state.needs_elevate;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            elevate,
            "updating an HKLM record needs elevate even on a writable dir"
        );
        assert_eq!(sess.registry.unwrap().hives, [RegHive::Hklm]);
    }

    #[test]
    fn set_source_mirrorc_start_without_cdk_fails() {
        let mut sess = session_with_sources(vec![
            SourceItem {
                id: "http".into(),
                name: "HTTP".into(),
                uri: "https://example.com/app.json".into(),
                icon: None,
                requires_webview: false,
                hidden: false,
            },
            SourceItem {
                id: "mirrorc".into(),
                name: "Mirrorc".into(),
                uri: "mirrorc://rid/1".into(),
                icon: None,
                requires_webview: false,
                hidden: false,
            },
        ]);
        sess.state.cdk = CdkStatus::Ok;
        sess.apply(Intent::SetSource {
            uri: "mirrorc://rid/1".into(),
        });
        assert_eq!(sess.state.cdk, CdkStatus::Idle);
        assert!(
            sess.state
                .sources
                .iter()
                .any(|s| s.uri.starts_with("mirrorc://")),
            "mirrorc source must stay visible"
        );

        sess.apply(Intent::Start);
        match &sess.state.phase {
            Phase::Failed(c) => assert_eq!(c.code, MIRRORC_CDK_MISSING),
            other => panic!("expected Failed(MIRRORC_CDK_MISSING), got {other:?}"),
        }
    }

    #[test]
    fn set_cdk_uri_does_not_change_source() {
        let mut sess = session_with_sources(vec![
            SourceItem {
                id: "http".into(),
                name: "HTTP".into(),
                uri: "https://example.com/app.json".into(),
                icon: None,
                requires_webview: false,
                hidden: false,
            },
            SourceItem {
                id: "mirrorc".into(),
                name: "Mirrorc".into(),
                uri: "mirrorc://rid/1".into(),
                icon: None,
                requires_webview: false,
                hidden: false,
            },
        ]);
        sess.apply(Intent::SetSource {
            uri: "https://example.com/app.json".into(),
        });
        sess.apply(Intent::SetCdk {
            cdk: "abc".into(),
            uri: Some("mirrorc://rid/1".into()),
        });
        assert_eq!(
            sess.state.options.source_uri,
            "https://example.com/app.json"
        );
        assert_eq!(sess.state.options.mirrorc_cdk.as_deref(), Some("abc"));
    }

    #[test]
    fn answer_clears_matching_prompt_and_leaves_phase_to_the_session() {
        let mut sess = UiSession::new(UiState::default());
        let running = Phase::Running(Progress::new(ProgressStage::ScanFiles, Some(1), Some(10.0)));
        sess.state.phase = running.clone();
        sess.state.pending = Some(Prompt {
            id: "p1".into(),
            kind: "occupied_files",
            items: vec!["a.dll".into()],
            params: BTreeMap::new(),
        });
        sess.apply(Intent::Answer {
            id: "other".into(),
            ok: false,
        });
        assert!(
            sess.state.pending.is_some(),
            "answer for another prompt is ignored"
        );
        sess.apply(Intent::Answer {
            id: "p1".into(),
            ok: false,
        });
        assert!(sess.state.pending.is_none());
        assert!(
            matches!(sess.state.phase, Phase::Running(_)),
            "phase is owned by run_install; got {:?}",
            sess.state.phase
        );
    }

    #[test]
    fn dismiss_failed_preserves_options() {
        let mut sess = UiSession::new(UiState::default());
        sess.apply(Intent::SetCreateLnk { value: false });
        sess.apply(Intent::SetDeleteUserData { value: true });
        sess.apply(Intent::SetSource {
            uri: "https://example.com/app.json".into(),
        });
        let before = sess.state.options.clone();
        sess.state.phase = Phase::Failed(Coded::bare(MIRRORC_CDK_MISSING));
        sess.apply(Intent::Dismiss);
        assert!(matches!(sess.state.phase, Phase::Ready));
        assert_eq!(sess.state.options, before);
    }

    #[test]
    fn intent_from_value_covers_every_variant() {
        use serde_json::json;
        let parse = |v: Value| Intent::from_value(&v).unwrap();
        assert!(matches!(
            parse(json!({"kind": "set_path", "path": "C:\\App"})),
            Intent::SetPath { path } if path == "C:\\App"
        ));
        assert!(matches!(
            parse(json!({"kind": "set_source", "uri": "https://x/y.json"})),
            Intent::SetSource { uri } if uri == "https://x/y.json"
        ));
        assert!(matches!(
            parse(json!({"kind": "set_create_lnk", "value": false})),
            Intent::SetCreateLnk { value: false }
        ));
        assert!(matches!(
            parse(json!({"kind": "set_delete_user_data", "value": true})),
            Intent::SetDeleteUserData { value: true }
        ));
        assert!(matches!(
            parse(json!({"kind": "set_cdk", "cdk": "abc"})),
            Intent::SetCdk { cdk, uri: None } if cdk == "abc"
        ));
        assert!(matches!(
            parse(json!({"kind": "set_cdk", "cdk": "abc", "uri": "mirrorc://rid"})),
            Intent::SetCdk { cdk, uri: Some(uri) } if cdk == "abc" && uri == "mirrorc://rid"
        ));
        assert!(matches!(
            parse(json!({"kind": "cancel_cdk"})),
            Intent::CancelCdk
        ));
        assert!(matches!(parse(json!({"kind": "start"})), Intent::Start));
        assert!(matches!(parse(json!({"kind": "cancel"})), Intent::Cancel));
        assert!(matches!(
            parse(json!({"kind": "answer", "id": "p1", "ok": true})),
            Intent::Answer { id, ok: true } if id == "p1"
        ));
        assert!(matches!(parse(json!({"kind": "dismiss"})), Intent::Dismiss));
        assert!(matches!(parse(json!({"kind": "launch"})), Intent::Launch));
        assert!(matches!(
            parse(json!({"kind": "advanced"})),
            Intent::Advanced
        ));
        assert!(matches!(parse(json!({"kind": "close"})), Intent::Close));

        assert!(Intent::from_value(&json!({"kind": "nope"})).is_err());
        assert!(Intent::from_value(&json!({"kind": "set_path"})).is_err());
        assert!(Intent::from_value(&json!({"kind": "answer", "id": "p1", "ok": "yes"})).is_err());
        assert!(Intent::from_value(&json!({"path": "C:\\App"})).is_err());
    }
}
