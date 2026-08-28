use serde::{Deserialize, Serialize};

use crate::utils::metadata::{FileMeta, PatchInfo, PatchSide};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashKey {
    Md5,
    Xxh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFile {
    pub file_name: String,
    pub hash: String,
    pub size: u64,
    #[serde(default)]
    pub unwritable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanInput {
    pub install_path: String,
    pub is_update: bool,
    pub hash_key: HashKey,
    pub hashed: Vec<FileMeta>,
    #[serde(default)]
    pub patches: Vec<PatchInfo>,
    #[serde(default)]
    pub deletes: Vec<String>,
    pub local: Vec<LocalFile>,
    #[serde(default)]
    pub embedded_names: Vec<String>,
    #[serde(default)]
    pub user_data_path: Vec<String>,
    #[serde(default)]
    pub ignore_nonempty: Vec<String>,
    #[serde(default)]
    pub app_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(debug_assertions, derive(Serialize))]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Unchanged,
    UserData,
    IgnoreFolder,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(debug_assertions, derive(Serialize))]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Skip,
    Install,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(debug_assertions, derive(Serialize))]
pub struct PlanFile {
    pub file_name: String,
    pub action: PlanAction,
    #[cfg_attr(debug_assertions, serde(skip_serializing_if = "Option::is_none"))]
    pub skip_reason: Option<SkipReason>,
    #[cfg_attr(debug_assertions, serde(skip_serializing_if = "Option::is_none"))]
    pub old_hash: Option<String>,
    pub unwritable: bool,
    pub has_patch: bool,
    pub has_lpatch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(debug_assertions, derive(Serialize))]
pub struct InstallPlan {
    pub files: Vec<PlanFile>,
    pub deletes: Vec<String>,
}

pub fn normalize_rel(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase()
}

pub fn normalize_full(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

pub fn expand_template(template: &str, install_path: &str, app_name: &str) -> String {
    template
        .replace("${INSTALL_PATH}", install_path)
        .replace("${APP_NAME}", app_name)
}

pub fn join_install(install_path: &str, rel: &str) -> String {
    let rel = rel.replace('/', "\\").trim_start_matches('\\').to_string();
    let base = install_path.trim_end_matches(['/', '\\']);
    format!("{base}\\{rel}")
}

pub fn is_under(file: &str, dir: &str) -> bool {
    let file = normalize_full(file);
    let dir = normalize_full(dir);
    file == dir || file.starts_with(&format!("{dir}\\"))
}

fn hash_of(info: &FileMeta, key: HashKey) -> Option<&str> {
    match key {
        HashKey::Md5 => info.md5.as_deref(),
        HashKey::Xxh => info.xxh.as_deref(),
    }
}

fn side_hash(side: &PatchSide, key: HashKey) -> Option<&str> {
    match key {
        HashKey::Md5 => side.md5.as_deref(),
        HashKey::Xxh => side.xxh.as_deref(),
    }
}

pub fn find_local<'a>(locals: &'a [LocalFile], file_name: &str) -> Option<&'a LocalFile> {
    let want = normalize_rel(file_name);
    locals
        .iter()
        .find(|local| normalize_rel(&local.file_name) == want)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SettingsDump {
    pub install_path: String,
    pub is_update: bool,
    #[serde(default)]
    pub user_data_path: Vec<String>,
    #[serde(default)]
    pub app_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetaScanDump {
    pub hash_key: HashKey,
    pub hashed: Vec<FileMeta>,
    #[serde(default)]
    pub patches: Vec<PatchInfo>,
    #[serde(default)]
    pub deletes: Vec<String>,
    pub local: Vec<LocalFile>,
    #[serde(default)]
    pub embedded_names: Vec<String>,
    #[serde(default)]
    pub ignore_nonempty: Vec<String>,
}

pub fn plan_from_dumps(settings: SettingsDump, meta: MetaScanDump) -> InstallPlan {
    build_plan(&PlanInput {
        install_path: settings.install_path,
        is_update: settings.is_update,
        hash_key: meta.hash_key,
        hashed: meta.hashed,
        patches: meta.patches,
        deletes: meta.deletes,
        local: meta.local,
        embedded_names: meta.embedded_names,
        user_data_path: settings.user_data_path,
        ignore_nonempty: meta.ignore_nonempty,
        app_name: settings.app_name,
    })
}

pub fn build_plan(input: &PlanInput) -> InstallPlan {
    let user_data_dirs: Vec<String> = input
        .user_data_path
        .iter()
        .map(|tpl| expand_template(tpl, &input.install_path, &input.app_name))
        .collect();
    let ignore_dirs: Vec<String> = input
        .ignore_nonempty
        .iter()
        .map(|p| normalize_full(p))
        .collect();

    let mut files = Vec::with_capacity(input.hashed.len());
    for item in &input.hashed {
        let local = find_local(&input.local, &item.file_name);
        let full = join_install(&input.install_path, &item.file_name);

        if let Some(local) = local {
            let local_full = join_install(&input.install_path, &local.file_name);
            if user_data_dirs.iter().any(|dir| is_under(&local_full, dir)) {
                files.push(PlanFile {
                    file_name: item.file_name.clone(),
                    action: PlanAction::Skip,
                    skip_reason: Some(SkipReason::UserData),
                    old_hash: Some(local.hash.clone()),
                    unwritable: local.unwritable,
                    has_patch: false,
                    has_lpatch: false,
                });
                continue;
            }
        }

        if input.is_update && ignore_dirs.iter().any(|dir| is_under(&full, dir)) {
            files.push(PlanFile {
                file_name: item.file_name.clone(),
                action: PlanAction::Skip,
                skip_reason: Some(SkipReason::IgnoreFolder),
                old_hash: local.map(|l| l.hash.clone()),
                unwritable: local.map(|l| l.unwritable).unwrap_or(false),
                has_patch: false,
                has_lpatch: false,
            });
            continue;
        }

        let item_hash = hash_of(item, input.hash_key);
        if let (Some(local), Some(want)) = (local, item_hash) {
            if local.hash == want {
                files.push(PlanFile {
                    file_name: item.file_name.clone(),
                    action: PlanAction::Skip,
                    skip_reason: Some(SkipReason::Unchanged),
                    old_hash: Some(local.hash.clone()),
                    unwritable: local.unwritable,
                    has_patch: false,
                    has_lpatch: false,
                });
                continue;
            }
        }

        let has_patch = input.patches.iter().any(|patch| {
            side_hash(&patch.from, input.hash_key) == local.map(|l| l.hash.as_str())
                && side_hash(&patch.to, input.hash_key) == item_hash
        });
        let has_lpatch = input.patches.iter().any(|patch| {
            side_hash(&patch.to, input.hash_key) == item_hash
                && side_hash(&patch.from, input.hash_key)
                    .is_some_and(|from| input.embedded_names.iter().any(|name| name == from))
        });

        files.push(PlanFile {
            file_name: item.file_name.clone(),
            action: PlanAction::Install,
            skip_reason: None,
            old_hash: local.map(|l| l.hash.clone()),
            unwritable: local.map(|l| l.unwritable).unwrap_or(false),
            has_patch,
            has_lpatch,
        });
    }

    let deletes = input
        .deletes
        .iter()
        .filter(|delete_file| {
            if !input.is_update || ignore_dirs.is_empty() {
                return true;
            }
            let full = join_install(&input.install_path, delete_file);
            !ignore_dirs.iter().any(|dir| is_under(&full, dir))
        })
        .cloned()
        .collect();

    InstallPlan { files, deletes }
}

pub fn collect_skip_hash(
    hashed: &[FileMeta],
    install_path: &str,
    app_name: &str,
    user_data_path: &[String],
    ignore_nonempty: &[String],
) -> Vec<String> {
    let user_data_dirs: Vec<String> = user_data_path
        .iter()
        .map(|tpl| expand_template(tpl, install_path, app_name))
        .collect();
    let ignore_dirs: Vec<String> = ignore_nonempty.iter().map(|p| normalize_full(p)).collect();
    hashed
        .iter()
        .filter(|item| {
            let full = join_install(install_path, &item.file_name);
            user_data_dirs.iter().any(|dir| is_under(&full, dir))
                || ignore_dirs.iter().any(|dir| is_under(&full, dir))
        })
        .map(|item| item.file_name.clone())
        .collect()
}

pub fn files_to_probe_writable(plan: &InstallPlan, local: &[LocalFile]) -> Vec<String> {
    plan.files
        .iter()
        .filter(|file| file.action == PlanAction::Install)
        .filter(|file| find_local(local, &file.file_name).is_some())
        .map(|file| file.file_name.clone())
        .collect()
}

pub fn mark_unwritable(files: &mut [PlanFile], install_path: &str, unwritable_abs: &[String]) {
    let set: std::collections::HashSet<String> = unwritable_abs
        .iter()
        .map(|path| normalize_full(path))
        .collect();
    for file in files {
        let full = normalize_full(&join_install(install_path, &file.file_name));
        file.unwritable = set.contains(&full);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hash_info(name: &str, md5: &str) -> FileMeta {
        FileMeta {
            file_name: name.to_string(),
            size: 1,
            md5: Some(md5.to_string()),
            xxh: None,
            installer: None,
        }
    }

    fn local(name: &str, hash: &str) -> LocalFile {
        LocalFile {
            file_name: name.to_string(),
            hash: hash.to_string(),
            size: 1,
            unwritable: false,
        }
    }

    fn base_input() -> PlanInput {
        PlanInput {
            install_path: r"C:\app".to_string(),
            is_update: true,
            hash_key: HashKey::Md5,
            hashed: vec![],
            patches: vec![],
            deletes: vec![],
            local: vec![],
            embedded_names: vec![],
            user_data_path: vec!["${INSTALL_PATH}/User".to_string()],
            ignore_nonempty: vec![],
            app_name: "Test".to_string(),
        }
    }

    #[test]
    fn skip_unchanged() {
        let mut input = base_input();
        input.hashed = vec![hash_info("app.exe", "aaa")];
        input.local = vec![local("app.exe", "aaa")];
        let plan = build_plan(&input);
        assert_eq!(plan.files[0].action, PlanAction::Skip);
        assert_eq!(plan.files[0].skip_reason, Some(SkipReason::Unchanged));
    }

    #[test]
    fn install_when_hash_differs() {
        let mut input = base_input();
        input.hashed = vec![hash_info("app.exe", "bbb")];
        input.local = vec![local("app.exe", "aaa")];
        input.patches = vec![PatchInfo {
            file_name: "app.exe".to_string(),
            size: 10,
            from: PatchSide {
                size: 1,
                md5: Some("aaa".to_string()),
                xxh: None,
            },
            to: PatchSide {
                size: 1,
                md5: Some("bbb".to_string()),
                xxh: None,
            },
        }];
        let plan = build_plan(&input);
        assert_eq!(plan.files[0].action, PlanAction::Install);
        assert!(plan.files[0].has_patch);
        assert!(!plan.files[0].has_lpatch);
    }

    #[test]
    fn user_data_uses_expanded_install_path() {
        let mut input = base_input();
        input.hashed = vec![
            hash_info("app.exe", "bbb"),
            hash_info("User/settings.json", "v2"),
        ];
        input.local = vec![
            local("app.exe", "aaa"),
            local(r"\User\settings.json", "user-modified"),
        ];
        let plan = build_plan(&input);
        let user = plan
            .files
            .iter()
            .find(|f| f.file_name == "User/settings.json")
            .unwrap();
        assert_eq!(user.action, PlanAction::Skip);
        assert_eq!(user.skip_reason, Some(SkipReason::UserData));
        let app = plan
            .files
            .iter()
            .find(|f| f.file_name == "app.exe")
            .unwrap();
        assert_eq!(app.action, PlanAction::Install);
    }

    #[test]
    fn ignore_nonempty_folder_on_update() {
        let mut input = base_input();
        input.ignore_nonempty = vec![r"C:\app\cache".to_string()];
        input.hashed = vec![
            hash_info("cache/keep.dat", "v2"),
            hash_info("cache/new.dat", "new"),
        ];
        input.local = vec![local("cache/keep.dat", "modified")];
        let plan = build_plan(&input);
        assert!(plan.files.iter().all(
            |f| f.action == PlanAction::Skip && f.skip_reason == Some(SkipReason::IgnoreFolder)
        ));
    }

    #[test]
    fn lpatch_only_when_to_matches_item() {
        let mut input = base_input();
        input.hashed = vec![hash_info("app.exe", "bbb"), hash_info("other.dll", "ddd")];
        input.local = vec![local("app.exe", "aaa")];
        input.embedded_names = vec!["emb".to_string()];
        input.patches = vec![PatchInfo {
            file_name: "other.dll".to_string(),
            size: 10,
            from: PatchSide {
                size: 1,
                md5: Some("emb".to_string()),
                xxh: None,
            },
            to: PatchSide {
                size: 1,
                md5: Some("ddd".to_string()),
                xxh: None,
            },
        }];
        let plan = build_plan(&input);
        let app = plan
            .files
            .iter()
            .find(|f| f.file_name == "app.exe")
            .unwrap();
        assert!(!app.has_lpatch);
        let other = plan
            .files
            .iter()
            .find(|f| f.file_name == "other.dll")
            .unwrap();
        assert!(other.has_lpatch);
    }

    #[test]
    fn dumps_with_install_path_user_data_skip() {
        let settings = SettingsDump {
            install_path: r"C:\tmp\inst".to_string(),
            is_update: true,
            user_data_path: vec!["${INSTALL_PATH}/User".to_string()],
            app_name: "Test Application".to_string(),
        };
        let meta = MetaScanDump {
            hash_key: HashKey::Md5,
            hashed: vec![
                hash_info("app.exe", "v2"),
                hash_info("User/settings.json", "pkg-v2"),
            ],
            patches: vec![],
            deletes: vec![],
            local: vec![
                local("app.exe", "v1"),
                local("User/settings.json", "USER_MODIFIED"),
            ],
            embedded_names: vec![],
            ignore_nonempty: vec![],
        };
        let plan = plan_from_dumps(settings, meta);
        let user = plan
            .files
            .iter()
            .find(|f| f.file_name == "User/settings.json")
            .unwrap();
        assert_eq!(user.skip_reason, Some(SkipReason::UserData));
        assert_eq!(user.action, PlanAction::Skip);
    }

    #[test]
    fn compare_offline_install_dump_if_present() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/plan-dumps/offline-install");
        let settings_path = dir.join("01-settings.json");
        let meta_path = dir.join("02-meta-scan.json");
        let js_plan_path = dir.join("03-plan.json");
        if !settings_path.exists() {
            return;
        }
        let settings: SettingsDump =
            serde_json::from_slice(&std::fs::read(settings_path).unwrap()).unwrap();
        let meta: MetaScanDump =
            serde_json::from_slice(&std::fs::read(meta_path).unwrap()).unwrap();
        let js_plan: InstallPlan =
            serde_json::from_slice(&std::fs::read(js_plan_path).unwrap()).unwrap();
        let rust_plan = plan_from_dumps(settings, meta);
        assert_eq!(rust_plan, js_plan);
    }

    #[test]
    fn deletes_skip_ignored_folder() {
        let mut input = base_input();
        input.deletes = vec!["readme.txt".to_string(), "cache/old.dat".to_string()];
        input.ignore_nonempty = vec![r"C:\app\cache".to_string()];
        let plan = build_plan(&input);
        assert_eq!(plan.deletes, vec!["readme.txt".to_string()]);
    }

    #[test]
    fn skip_hash_covers_user_data_and_ignore_folder() {
        let skip = collect_skip_hash(
            &[
                hash_info("app.exe", "aaa"),
                hash_info("User/settings.json", "v2"),
                hash_info("cache/keep.dat", "v2"),
            ],
            r"C:\app",
            "Test",
            &["${INSTALL_PATH}/User".to_string()],
            &[r"C:\app\cache".to_string()],
        );
        assert_eq!(skip, vec!["User/settings.json", "cache/keep.dat"]);
    }

    #[test]
    fn probe_list_only_existing_install_files() {
        let mut input = base_input();
        input.hashed = vec![
            hash_info("app.exe", "bbb"),
            hash_info("new.dll", "ccc"),
            hash_info("same.bin", "aaa"),
        ];
        input.local = vec![local("app.exe", "aaa"), local("same.bin", "aaa")];
        let plan = build_plan(&input);
        assert_eq!(plan.files[2].action, PlanAction::Skip);
        assert_eq!(
            files_to_probe_writable(&plan, &input.local),
            vec!["app.exe".to_string()]
        );
    }

    #[test]
    fn mark_unwritable_only_sets_probed_paths() {
        let mut input = base_input();
        input.hashed = vec![hash_info("app.exe", "bbb"), hash_info("data.dll", "ccc")];
        input.local = vec![local("app.exe", "aaa"), local("data.dll", "old")];
        let mut plan = build_plan(&input);
        mark_unwritable(&mut plan.files, r"C:\app", &[r"C:\app\app.exe".to_string()]);
        let app = plan
            .files
            .iter()
            .find(|f| f.file_name == "app.exe")
            .unwrap();
        let data = plan
            .files
            .iter()
            .find(|f| f.file_name == "data.dll")
            .unwrap();
        assert!(app.unwritable);
        assert!(!data.unwritable);
    }
}
