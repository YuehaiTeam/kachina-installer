//! UI session machine. Rust holds the only copy of `UiState`; renderers apply
//! it wholesale. Named `UiSession` because `session::commands::SessionState`
//! already exists (PromptHub + PluginHub).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::session::types::SessionResult;
use crate::utils::code::{Coded, MIRRORC_CDK_MISSING};
use crate::utils::dir::in_private_folder;

/// Progress `stage` values. Locale keys are `progress.<stage>`.
pub const STAGE_KEYS: &[&str] = &[
    "progress.prepare",
    "progress.metadata",
    "progress.hash_scan",
    "progress.plan",
    "progress.download",
    "progress.patch",
    "progress.extract",
    "progress.delete",
    "progress.runtime_download",
    "progress.runtime_install",
    "progress.shortcut",
    "progress.registry",
    "progress.finalize",
    "progress.uninstall_scan",
    "progress.uninstall_delete",
    "progress.mirrorc_metadata",
    "progress.mirrorc_download",
    "progress.mirrorc_verify",
    "progress.done",
];

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
    pub sub_step: u32,
    pub percent: f64,
    pub stage: &'static str,
    pub subject: Option<String>,
    pub done: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Prompt {
    pub id: String,
    pub kind: &'static str,
    pub items: Vec<String>,
    pub params: BTreeMap<&'static str, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intent {
    SetPath { path: String },
    SetSource { uri: String },
    SetCreateLnk { value: bool },
    SetDeleteUserData { value: bool },
    SetCdk { cdk: String },
    Start,
    Cancel,
    Answer { id: String, ok: bool },
    Dismiss,
    Launch,
    Advanced,
    Close,
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
        Self::with_project(state, renderer, String::new(), String::new(), String::new())
    }

    pub fn with_project(
        state: UiState,
        renderer: Renderer,
        exe_name: String,
        app_name: String,
        uac_strategy: String,
    ) -> Self {
        let all_sources = state.sources.clone();
        let mut sess = Self {
            state,
            all_sources,
            renderer,
            exe_name,
            app_name,
            uac_strategy,
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
                self.state.options.source_uri = uri;
                if self.state.options.source_uri.starts_with("mirrorc://") {
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
            Intent::SetCdk { cdk } => {
                // Network check is not in this step; store the value only.
                self.state.options.mirrorc_cdk = if cdk.is_empty() { None } else { Some(cdk) };
            }
            Intent::Start => {
                if self.state.options.source_uri.starts_with("mirrorc://")
                    && !matches!(self.state.cdk, CdkStatus::Ok)
                {
                    self.state.phase = Phase::Failed(Coded::bare(MIRRORC_CDK_MISSING));
                }
                // run_install / run_uninstall are step 2.
            }
            Intent::Answer { ok, .. } => {
                let pending = self.state.pending.take();
                if let Some(prompt) = pending {
                    if prompt.kind == "occupied_files" && !ok {
                        let is_update = matches!(self.state.mode, Mode::Update);
                        self.state.phase = Phase::Done(SessionResult::cancelled(is_update));
                    }
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
            .filter(|s| {
                !(matches!(self.renderer, Renderer::Native) && s.requires_webview)
            })
            .cloned()
            .collect();
    }

    fn recompute_path(&mut self) {
        let probed = probe_path(Path::new(&self.state.options.install_path), &self.exe_name);
        if matches!(self.state.mode, Mode::Uninstall) {
            // Uninstall is a session kind, not derived from the path marker.
        } else if probed.upgrade {
            self.state.mode = Mode::Update;
        } else {
            self.state.mode = Mode::Install;
        }
        self.state.needs_elevate = compute_elevate(probed.writable, &self.uac_strategy);
        self.state.path = probed;
    }
}

/// Path probe used by `SetPath`.
///
/// * `exists` — `path.exists()`
/// * `writable` — try to create (and delete) a unique `.kachina-write-probe-<pid>`
///   file inside the directory, or inside the nearest existing ancestor if the
///   path does not exist yet. `Private` if that write succeeds and the path is
///   under a per-user folder (`in_private_folder`). `Unwritable` if the write
///   fails.
/// * `upgrade` — a `.kachina-upgrade` marker file, or `installer-meta.json`
///   (uninstall-metadata-like). Existing-install detection in production still
///   uses the project's exe name; this probe stays independent of `ProjectConfig`
///   so unit tests can set the marker.
fn compute_elevate(writable: PathWritable, strategy: &str) -> bool {
    if strategy.is_empty() {
        return !matches!(writable, PathWritable::Writable);
    }
    crate::session::types::elevate_from_state(&dir_state(writable), strategy)
}

fn dir_state(w: PathWritable) -> crate::installer::DirState {
    match w {
        PathWritable::Writable => crate::installer::DirState::Writable,
        PathWritable::Unwritable => crate::installer::DirState::Unwritable,
        PathWritable::Private => crate::installer::DirState::Private,
    }
}

fn probe_path(path: &Path, exe_name: &str) -> PathState {
    let exists = path.exists();
    let upgrade = exists
        && (path.join(".kachina-upgrade").is_file()
            || path.join("installer-meta.json").is_file()
            || (!exe_name.is_empty() && path.join(exe_name).is_file()));
    let writable = classify_writable(path);
    PathState {
        writable,
        exists,
        upgrade,
    }
}

fn classify_writable(path: &Path) -> PathWritable {
    let private = in_private_folder(path);
    let can_write = can_create_probe(path);
    if !can_write {
        PathWritable::Unwritable
    } else if private {
        PathWritable::Private
    } else {
        PathWritable::Writable
    }
}

fn can_create_probe(path: &Path) -> bool {
    if path.exists() {
        return path.is_dir() && try_probe_file(path);
    }
    for anc in path.ancestors().skip(1) {
        if anc.exists() {
            return anc.is_dir() && try_probe_file(anc);
        }
    }
    false
}

fn try_probe_file(dir: &Path) -> bool {
    let name = format!(".kachina-write-probe-{}", std::process::id());
    let p = dir.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
    {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&p);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn scratch_dir() -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.cache/ui-session-tests");
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join(format!("t-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn deny_write(dir: &Path) {
        let user = whoami::username();
        let status = Command::new("icacls")
            .arg(dir)
            .args([
                "/deny",
                &format!("{user}:(W,DC,AD)"),
            ])
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
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(dir, perms);
        }
    }

    fn session_with_sources(sources: Vec<SourceItem>) -> UiSession {
        let mut state = UiState::default();
        state.sources = sources;
        UiSession::new(state)
    }

    #[test]
    fn set_path_readonly_needs_elevate_and_mode_follows_upgrade() {
        let dir = scratch_dir();
        let mut sess = UiSession::new(UiState::default());

        sess.apply(Intent::SetPath {
            path: dir.to_string_lossy().into_owned(),
        });
        assert!(
            !sess.state.needs_elevate,
            "fresh dir on V: should be writable, writable={:?} private_probe={:?}",
            sess.state.path.writable,
            sess.state.path
        );
        assert_eq!(sess.state.mode, Mode::Install);
        assert!(!sess.state.path.upgrade);

        std::fs::write(dir.join(".kachina-upgrade"), b"1").unwrap();
        sess.apply(Intent::SetPath {
            path: dir.to_string_lossy().into_owned(),
        });
        assert_eq!(sess.state.mode, Mode::Update);
        assert!(sess.state.path.upgrade);
        assert!(
            !sess.state.needs_elevate,
            "upgrade marker must not force elevate on a writable dir"
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
        assert_eq!(mode, Mode::Update, "mode still follows upgrade after readonly");
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
            },
            SourceItem {
                id: "mirrorc".into(),
                name: "Mirrorc".into(),
                uri: "mirrorc://rid/1".into(),
                icon: None,
                requires_webview: false,
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
    fn answer_occupied_files_no_cancels_session() {
        let mut sess = UiSession::new(UiState::default());
        sess.state.pending = Some(Prompt {
            id: "p1".into(),
            kind: "occupied_files",
            items: vec!["a.dll".into()],
            params: BTreeMap::new(),
        });
        sess.apply(Intent::Answer {
            id: "p1".into(),
            ok: false,
        });
        match &sess.state.phase {
            Phase::Done(r) => {
                assert!(r.cancelled);
                assert!(!r.already_latest);
                assert!(!r.is_update);
                assert!(!r.is_uninstall);
            }
            other => panic!("expected Done(cancelled), got {other:?}"),
        }
        assert!(sess.state.pending.is_none());
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
}
