use crate::session::plan::normalize_full;
use crate::utils::code::{Coded, REGISTRY_READ_FAILED};
use anyhow::{Context, Result};

const UNINSTALL_PREFIX: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\";

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegHive {
    Hkcu,
    Hklm,
}

impl RegHive {
    fn root(self) -> &'static windows_registry::Key {
        match self {
            Self::Hkcu => windows_registry::CURRENT_USER,
            Self::Hklm => windows_registry::LOCAL_MACHINE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HiveRead {
    #[default]
    Absent,
    Denied,
    Failed,
    Present {
        location: String,
        meta: Option<String>,
    },
}

impl HiveRead {
    pub fn incomplete(&self) -> bool {
        matches!(self, Self::Denied | Self::Failed)
    }

    fn present_location(&self) -> Option<&str> {
        match self {
            Self::Present { location, .. } => Some(location.as_str()),
            _ => None,
        }
    }

    fn present_meta(&self) -> Option<&str> {
        match self {
            Self::Present { meta, .. } => meta.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Identity {
    pub hkcu: HiveRead,
    pub hklm: HiveRead,
}

impl Identity {
    pub fn incomplete(&self) -> bool {
        self.hkcu.incomplete() || self.hklm.incomplete()
    }

    fn hive(&self, hive: RegHive) -> &HiveRead {
        match hive {
            RegHive::Hkcu => &self.hkcu,
            RegHive::Hklm => &self.hklm,
        }
    }

    pub fn matched(&self, path: &str, exe_name: &str, reg_name: &str) -> Vec<RegHive> {
        [RegHive::Hkcu, RegHive::Hklm]
            .into_iter()
            .filter(|h| {
                self.hive(*h)
                    .present_location()
                    .is_some_and(|loc| record_matches(loc, path, exe_name, reg_name))
            })
            .collect()
    }

    pub fn matched_meta(&self, path: &str, exe_name: &str, reg_name: &str) -> Option<String> {
        for hive in self.matched(path, exe_name, reg_name) {
            if let Some(meta) = self.hive(hive).present_meta() {
                return Some(meta.to_string());
            }
        }
        None
    }
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
pub struct WriteRegistryParams {
    pub reg_name: String,
    pub name: String,
    pub version: String,
    pub exe: String,
    pub source: String,
    pub uninstaller: String,
    pub publisher: String,
    /// `None` 时保留记录里已有的 `InstallerMeta` 和 `EstimatedSize`。
    pub manifest: Option<Manifest>,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
pub struct Manifest {
    pub metadata: String,
    pub size: u64,
}

pub fn write_at_hive(hive: RegHive, params: &WriteRegistryParams) -> Result<()> {
    let key_path = format!("{UNINSTALL_PREFIX}{}", params.reg_name);
    let key = hive.root().create(&key_path).context("OPEN_REG_ERR")?;
    {
        key.set_string("DisplayName", &params.name)?;
        key.set_string("DisplayVersion", &params.version)?;
        key.set_string("UninstallString", &params.uninstaller)?;
        key.set_string("InstallLocation", &params.source)?;
        key.set_string("DisplayIcon", &params.exe)?;
        key.set_string("Publisher", &params.publisher)?;
        key.set_u32("NoModify", 1u32)?;
        key.set_u32("NoRepair", 1u32)?;
        if let Some(manifest) = &params.manifest {
            key.set_u32("EstimatedSize", (manifest.size as u32) / 1024)?;
            key.set_string("InstallerMeta", &manifest.metadata)?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .context("WRITE_REG_ERR")
}

pub fn remove_record(hive: RegHive, reg_name: &str) -> Result<()> {
    let key_path = format!("{UNINSTALL_PREFIX}{reg_name}");
    match hive.root().remove_tree(&key_path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let io = io_from_hresult(err.code().0);
            if is_not_found(&io) {
                Ok(())
            } else {
                Err(anyhow::Error::from(io).context("DELETE_REG_ERR"))
            }
        }
    }
}

fn install_paths_match(a: &str, b: &str) -> bool {
    normalize_full(a) == normalize_full(b)
}

fn record_matches(location: &str, install_path: &str, exe_name: &str, reg_name: &str) -> bool {
    if install_paths_match(location, install_path) {
        return true;
    }
    let folded = std::path::Path::new(location).join(reg_name);
    install_paths_match(&folded.to_string_lossy(), install_path)
        && std::path::Path::new(install_path).join(exe_name).is_file()
}

/// 返回本次要写入的 hive，空表示不登记。
fn registration_plan(
    discovered: &str,
    final_path: &str,
    identity: &Identity,
    is_update: bool,
    dir_elevate: bool,
    exe_name: &str,
    reg_name: &str,
) -> Vec<RegHive> {
    let at_final = identity.matched(final_path, exe_name, reg_name);
    if !at_final.is_empty() {
        return at_final;
    }
    let scope = if dir_elevate {
        RegHive::Hklm
    } else {
        RegHive::Hkcu
    };
    if install_paths_match(final_path, discovered) {
        return if is_update { Vec::new() } else { vec![scope] };
    }
    let abandoned = identity.matched(discovered, exe_name, reg_name);
    if abandoned.is_empty() {
        vec![scope]
    } else {
        abandoned
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegistryPlan {
    /// 空表示不登记。提权时只含 HKLM。
    pub hives: Vec<RegHive>,
    /// 提权会话把启动用户的 HKCU 记录迁到 HKLM：HKLM 写入成功后删除 HKCU 记录。
    pub drop_hkcu: bool,
    pub identity: Identity,
    pub elevate: bool,
}

/// 最终目录确定时得出登记计划和本次是否提权。`dir_elevate` 是目录按
/// `uacStrategy` 的判定；任一 hive 读取失败时返回 `REGISTRY_READ_FAILED`。
pub fn plan_registry(
    reg_name: &str,
    exe_name: &str,
    discovered: &str,
    final_path: &str,
    is_update: bool,
    dir_elevate: bool,
) -> Result<RegistryPlan, Coded> {
    if reg_name.is_empty() {
        return Ok(RegistryPlan {
            elevate: dir_elevate,
            ..RegistryPlan::default()
        });
    }
    plan_from_identity(
        read_identity(reg_name),
        reg_name,
        exe_name,
        discovered,
        final_path,
        is_update,
        dir_elevate,
    )
}

pub fn plan_from_identity(
    identity: Identity,
    reg_name: &str,
    exe_name: &str,
    discovered: &str,
    final_path: &str,
    is_update: bool,
    dir_elevate: bool,
) -> Result<RegistryPlan, Coded> {
    if identity.incomplete() {
        return Err(Coded::bare(REGISTRY_READ_FAILED));
    }
    let hives = registration_plan(
        discovered,
        final_path,
        &identity,
        is_update,
        dir_elevate,
        exe_name,
        reg_name,
    );
    let elevate = dir_elevate || hives.contains(&RegHive::Hklm);
    let (hives, drop_hkcu) = if elevate && !hives.is_empty() {
        (vec![RegHive::Hklm], hives.contains(&RegHive::Hkcu))
    } else {
        (hives, false)
    };
    Ok(RegistryPlan {
        hives,
        drop_hkcu,
        identity,
        elevate,
    })
}

pub fn read_identity(reg_name: &str) -> Identity {
    Identity {
        hkcu: read_hive(RegHive::Hkcu, reg_name),
        hklm: read_hive(RegHive::Hklm, reg_name),
    }
}

fn read_hive(hive: RegHive, reg_name: &str) -> HiveRead {
    let key_path = format!("{UNINSTALL_PREFIX}{reg_name}");
    let key = match hive.root().options().read().open(&key_path) {
        Ok(key) => key,
        Err(err) => return classify_open(io_from_hresult(err.code().0)),
    };
    let location = key.get_string("InstallLocation").unwrap_or_default();
    let meta = key.get_string("InstallerMeta").ok().and_then(|raw| {
        serde_json::from_str::<serde::de::IgnoredAny>(&raw)
            .ok()
            .map(|_| raw)
    });
    HiveRead::Present { location, meta }
}

/// `windows-registry` 的错误转换成 `std::io::Error` 时直接把 HRESULT 当作系统错误码；
/// Win32 设施的 HRESULT 须先还原成 Win32 错误码，`ErrorKind` 才能分类。
fn io_from_hresult(hr: i32) -> std::io::Error {
    let bits = hr as u32;
    let code = if bits & 0xFFFF_0000 == 0x8007_0000 {
        (bits & 0xFFFF) as i32
    } else {
        hr
    };
    std::io::Error::from_raw_os_error(code)
}

// ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND
fn is_not_found(io: &std::io::Error) -> bool {
    io.kind() == std::io::ErrorKind::NotFound || matches!(io.raw_os_error(), Some(2 | 3))
}

fn classify_open(io: std::io::Error) -> HiveRead {
    if is_not_found(&io) {
        HiveRead::Absent
    } else if io.kind() == std::io::ErrorKind::PermissionDenied || io.raw_os_error() == Some(5) {
        HiveRead::Denied
    } else {
        HiveRead::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(location: &str) -> HiveRead {
        HiveRead::Present {
            location: location.into(),
            meta: Some("{}".into()),
        }
    }

    fn identity(hkcu: HiveRead, hklm: HiveRead) -> Identity {
        Identity { hkcu, hklm }
    }

    #[test]
    fn open_errors_classify_from_hresult() {
        let open = |hr: u32| classify_open(io_from_hresult(hr as i32));
        assert_eq!(open(0x8007_0002), HiveRead::Absent);
        assert_eq!(open(0x8007_0003), HiveRead::Absent);
        assert_eq!(open(0x8007_0005), HiveRead::Denied);
        assert_eq!(open(0x8000_4005), HiveRead::Failed);
    }

    #[test]
    fn install_paths_match_separators_case_and_trailing_slash() {
        assert!(install_paths_match(r"C:\Games\App", r"C:/Games/App/"));
        assert!(install_paths_match(r"C:\Games\App", r"c:\games\app"));
        assert!(!install_paths_match(r"C:\Games\App", r"C:\Games\App2"));
        assert!(!install_paths_match(r"C:\Games\App", r"C:\Games\AppExtra"));
        assert!(!install_paths_match(r"\\?\C:\Games\App", r"C:\Games\App"));
    }

    #[test]
    fn plan_same_path_update_keeps_hives() {
        let id = identity(present(r"C:\App"), HiveRead::Absent);
        assert_eq!(
            registration_plan(r"C:\App", r"C:\App\", &id, true, false, "a.exe", "App"),
            [RegHive::Hkcu]
        );
    }

    #[test]
    fn plan_same_path_no_record_is_green_when_updating() {
        let id = identity(HiveRead::Absent, HiveRead::Absent);
        assert!(
            registration_plan(r"C:\App", r"C:\App", &id, true, true, "a.exe", "App").is_empty()
        );
    }

    #[test]
    fn plan_same_path_other_dir_record_is_green() {
        let id = identity(present(r"D:\Other"), HiveRead::Absent);
        assert!(
            registration_plan(r"C:\Green", r"C:\Green", &id, true, false, "a.exe", "App")
                .is_empty()
        );
    }

    #[test]
    fn plan_new_install_at_discovered_creates_in_dir_scope() {
        let id = identity(HiveRead::Absent, HiveRead::Absent);
        assert_eq!(
            registration_plan(r"C:\PF\App", r"C:\PF\App", &id, false, true, "a.exe", "App"),
            [RegHive::Hklm]
        );
        assert_eq!(
            registration_plan(
                r"C:\Me\App",
                r"C:\Me\App",
                &id,
                false,
                false,
                "a.exe",
                "App"
            ),
            [RegHive::Hkcu]
        );
    }

    #[test]
    fn dual_same_dir_records_update_both() {
        let id = identity(present(r"C:\App"), present(r"C:\App"));
        assert_eq!(
            registration_plan(r"C:\App", r"C:\App", &id, true, false, "a.exe", "App"),
            [RegHive::Hkcu, RegHive::Hklm]
        );
    }

    #[test]
    fn damaged_meta_is_still_present() {
        let read = HiveRead::Present {
            location: r"C:\App".into(),
            meta: None,
        };
        let id = identity(read, HiveRead::Absent);
        assert!(!id.incomplete());
        assert_eq!(id.matched(r"C:\App", "a.exe", "App"), vec![RegHive::Hkcu]);
        assert!(id.matched_meta(r"C:\App", "a.exe", "App").is_none());
    }

    #[test]
    fn changed_path_rewrites_only_the_discovered_record() {
        let id = identity(present(r"D:\Other"), present(r"C:\Old"));
        assert_eq!(
            registration_plan(r"C:\Old", r"E:\New", &id, false, false, "a.exe", "App"),
            [RegHive::Hklm]
        );
    }

    #[test]
    fn changed_path_onto_a_recorded_dir_updates_that_record() {
        let id = identity(present(r"D:\Other"), present(r"C:\Old"));
        assert_eq!(
            registration_plan(r"C:\Old", r"D:\Other", &id, true, false, "a.exe", "App"),
            [RegHive::Hkcu]
        );
    }

    #[test]
    fn changed_path_from_green_dir_creates_in_dir_scope() {
        let id = identity(present(r"D:\Other"), HiveRead::Absent);
        assert_eq!(
            registration_plan(r"C:\Green", r"E:\New", &id, false, false, "a.exe", "App"),
            [RegHive::Hkcu]
        );
    }

    fn plan(id: Identity, discovered: &str, final_path: &str, update: bool) -> RegistryPlan {
        plan_from_identity(id, "App", "a.exe", discovered, final_path, update, true).unwrap()
    }

    #[test]
    fn elevated_update_moves_the_hkcu_record_to_hklm() {
        let id = identity(present(r"C:\PF\App"), HiveRead::Absent);
        let p = plan(id, r"C:\PF\App", r"C:\PF\App", true);
        assert_eq!(p.hives, [RegHive::Hklm]);
        assert!(p.drop_hkcu && p.elevate);
    }

    #[test]
    fn elevated_path_change_moves_the_discovered_hkcu_record() {
        let id = identity(present(r"C:\Old"), HiveRead::Absent);
        let p = plan(id, r"C:\Old", r"C:\PF\App", false);
        assert_eq!(p.hives, [RegHive::Hklm]);
        assert!(p.drop_hkcu);
    }

    #[test]
    fn elevated_new_install_keeps_other_dir_hkcu_record() {
        let id = identity(present(r"D:\Other"), HiveRead::Absent);
        let p = plan(id, r"C:\PF\App", r"C:\PF\App", false);
        assert_eq!(p.hives, [RegHive::Hklm]);
        assert!(!p.drop_hkcu);
    }

    #[test]
    fn hklm_record_on_writable_dir_elevates_and_drops_the_hkcu_twin() {
        let id = identity(present(r"C:\App"), present(r"C:\App"));
        let p = plan_from_identity(id, "App", "a.exe", r"C:\App", r"C:\App", true, false).unwrap();
        assert_eq!(p.hives, [RegHive::Hklm]);
        assert!(p.elevate && p.drop_hkcu);
    }

    #[test]
    fn denied_is_incomplete() {
        let id = identity(HiveRead::Absent, HiveRead::Denied);
        assert!(id.incomplete());
        let plan = plan_from_identity(id, "App", "a.exe", r"C:\App", r"C:\App", true, false);
        assert_eq!(plan.unwrap_err().code, REGISTRY_READ_FAILED);
    }
}
