use crate::utils::code::Attach;
use serde::{Deserialize, Serialize};

use crate::cli::arg::InstallArgs;
use crate::installer::{config::InstallerConfig, DirState};
use crate::session::plan::{expand_template, HashKey};
use crate::utils::metadata::{FileMeta, RepoMetadata};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceItem {
    pub uri: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub hidden: bool,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SourceField {
    Single(String),
    List(Vec<SourceItem>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    pub source: SourceField,
    pub app_name: String,
    pub publisher: String,
    pub reg_name: String,
    pub exe_name: String,
    pub uninstall_name: String,
    pub updater_name: String,
    pub program_files_path: String,
    #[serde(default)]
    pub user_data_path: Vec<String>,
    #[serde(default)]
    pub ignore_folder_path: Vec<String>,
    #[serde(default)]
    pub extra_uninstall_path: Vec<String>,
    pub title: String,
    pub description: String,
    pub window_title: String,
    #[serde(default = "default_uac")]
    pub uac_strategy: String,
    pub runtimes: Option<Vec<String>>,
    pub window_borderless: Option<bool>,
    #[serde(default = "default_true")]
    pub need_web_view2: bool,
}

fn default_uac() -> String {
    "prefer-admin".to_string()
}

fn default_true() -> bool {
    true
}

impl ProjectConfig {
    pub fn from_value(value: &serde_json::Value) -> anyhow::Result<Self> {
        serde_json::from_value(value.clone())
            .map_err(|e| anyhow::Error::from(e).attach(crate::utils::code::PKG_BROKEN))
    }

    pub fn source_uri(&self, override_id: Option<&str>) -> anyhow::Result<String> {
        match &self.source {
            SourceField::Single(uri) => Ok(uri.clone()),
            SourceField::List(list) => {
                if let Some(id) = override_id {
                    if let Some(item) = list.iter().find(|s| s.id == id) {
                        return Ok(item.uri.clone());
                    }
                }
                list.first()
                    .map(|s| s.uri.clone())
                    .ok_or_else(|| anyhow::Error::from(crate::utils::code::Coded::bare(crate::utils::code::PKG_BROKEN)))
            }
        }
    }
}

impl RepoMetadata {
    /// 安装器会话用。不放进 `utils/metadata.rs`：该文件经 `#[path]` 编入 builder，builder 没有 session。
    pub fn hash_key(&self) -> anyhow::Result<HashKey> {
        if self.hashed.iter().all(|e| e.md5.is_some()) {
            Ok(HashKey::Md5)
        } else if self.hashed.iter().all(|e| e.xxh.is_some()) {
            Ok(HashKey::Xxh)
        } else {
            Err(anyhow::Error::from(crate::utils::code::Coded::bare(crate::utils::code::HASH_ALGORITHM_UNSUPPORTED)))
        }
    }

    pub fn item_hash<'a>(&'a self, item: &'a FileMeta, key: HashKey) -> Option<&'a str> {
        match key {
            HashKey::Md5 => item.md5.as_deref(),
            HashKey::Xxh => item.xxh.as_deref(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub install_path: String,
    pub source_uri: String,
    pub create_lnk: bool,
    pub delete_user_data: bool,
    pub mirrorc_cdk: Option<String>,
    pub online: bool,
    pub silent: bool,
    pub non_interactive: bool,
    pub dump_dir: Option<std::path::PathBuf>,
    pub dfs_extras: Option<String>,
    pub elevate: bool,
    pub is_update: bool,
    pub auto_answer: bool,
}

impl Settings {
    pub fn expand(&self, template: &str, app_name: &str) -> String {
        expand_template(template, &self.install_path, app_name)
    }
}

pub fn elevate_from_state(state: &DirState, strategy: &str) -> bool {
    match strategy {
        "force" => true,
        "prefer-admin" => !matches!(state, DirState::Private),
        "prefer-user" => matches!(state, DirState::Unwritable),
        _ => !matches!(state, DirState::Private),
    }
}

pub async fn settings_from_cli(
    args: &InstallArgs,
    config: &InstallerConfig,
    project: &ProjectConfig,
) -> anyhow::Result<Settings> {
    let install_path = args
        .target
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| config.install_path.clone());
    let source_uri = project.source_uri(args.source.as_deref())?;
    let inspected = crate::installer::inspect_dir(install_path.clone(), project.exe_name.clone())
        .await
        .ok_or_else(|| anyhow::Error::from(crate::utils::code::Coded::bare(crate::utils::code::INSTALL_PATH_INVALID)))?;
    Ok(Settings {
        install_path,
        source_uri,
        create_lnk: true,
        delete_user_data: false,
        mirrorc_cdk: args.mirrorc_cdk.clone(),
        online: args.online,
        silent: args.silent,
        non_interactive: args.non_interactive,
        dump_dir: args.dump_dir.clone(),
        dfs_extras: args.dfs_extras.clone(),
        elevate: elevate_from_state(&inspected.state, &project.uac_strategy),
        is_update: inspected.upgrade,
        auto_answer: args.silent || args.non_interactive,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInput {
    pub install_path: String,
    pub source_uri: String,
    pub create_lnk: bool,
    pub delete_user_data: bool,
    pub mirrorc_cdk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResult {
    pub already_latest: bool,
    pub is_update: bool,
    pub is_uninstall: bool,
    #[serde(default)]
    pub cancelled: bool,
}

impl SessionResult {
    pub fn install(already_latest: bool, is_update: bool) -> Self {
        Self {
            already_latest,
            is_update,
            is_uninstall: false,
            cancelled: false,
        }
    }

    pub fn cancelled(is_update: bool) -> Self {
        Self {
            already_latest: false,
            is_update,
            is_uninstall: false,
            cancelled: true,
        }
    }

    pub fn uninstall() -> Self {
        Self {
            already_latest: false,
            is_update: false,
            is_uninstall: true,
            cancelled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub sub_step: u32,
    pub percent: f64,
    pub current: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptEvent {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEvent {
    pub id: String,
    pub method: String,
    pub name: String,
    pub url: String,
    pub range: Option<String>,
    pub diffchunks: Option<Vec<String>>,
    pub insights: Option<serde_json::Value>,
}

pub fn version_gt(a: &str, b: &str) -> bool {
    version_cmp(a, b) == std::cmp::Ordering::Greater
}

#[derive(Clone)]
enum PrePart {
    Num(u64),
    Text(String),
}

impl PartialEq for PrePart {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for PrePart {}

impl PartialOrd for PrePart {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrePart {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => a.cmp(b),
            (Self::Num(_), Self::Text(_)) => std::cmp::Ordering::Less,
            (Self::Text(_), Self::Num(_)) => std::cmp::Ordering::Greater,
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
        }
    }
}

fn parse_version(s: &str) -> (Vec<u64>, Option<Vec<PrePart>>) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split_once('+').map(|(core, _)| core).unwrap_or(s);
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };
    let core = core
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect();
    let pre = pre.map(|pre| {
        pre.split('.')
            .map(|part| {
                if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) {
                    PrePart::Num(part.parse().unwrap_or(0))
                } else {
                    PrePart::Text(part.to_string())
                }
            })
            .collect()
    });
    (core, pre)
}

fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut aa, pa) = parse_version(a);
    let (mut bb, pb) = parse_version(b);
    let n = aa.len().max(bb.len()).max(3);
    aa.resize(n, 0);
    bb.resize(n, 0);
    match aa.cmp(&bb) {
        std::cmp::Ordering::Equal => match (&pa, &pb) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{version_cmp, version_gt};

    #[test]
    fn prerelease_is_older_than_release() {
        assert!(version_gt("1.0.0", "1.0.0-beta"));
        assert!(!version_gt("1.0.0-beta", "1.0.0"));
        assert_eq!(
            version_cmp("1.0.0", "1.0.0-beta"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn prerelease_order() {
        assert!(version_gt("1.0.0-beta", "1.0.0-alpha"));
        assert!(version_gt("1.0.0-beta.11", "1.0.0-beta.2"));
        assert!(version_gt("1.0.0-rc.1", "1.0.0-beta.11"));
        assert!(version_gt("1.0.0", "1.0.0-rc.1"));
    }

    #[test]
    fn v_prefix_and_short_core() {
        assert_eq!(version_cmp("v1.0.0", "1.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(version_cmp("1.0", "1.0.0"), std::cmp::Ordering::Equal);
        assert!(version_gt("1.0.1", "1.0"));
        assert!(version_gt("v2.0.0", "1.9.9"));
    }

    #[test]
    fn need_web_view2_defaults_true() {
        let v = serde_json::json!({
            "source": "https://example.com/app.exe",
            "appName": "A",
            "publisher": "P",
            "regName": "A",
            "exeName": "a.exe",
            "uninstallName": "uninst.exe",
            "updaterName": "update.exe",
            "programFilesPath": "A",
            "title": "T",
            "description": "D",
            "windowTitle": "W"
        });
        let cfg = super::ProjectConfig::from_value(&v).unwrap();
        assert!(cfg.need_web_view2);
    }
}
