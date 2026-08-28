use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMeta {
    pub file_name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xxh: Option<String>,
    /// 由安装会话在进程内合成，不由任何写出路径填写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PatchSide {
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PatchInfo {
    pub file_name: String,
    pub size: u64,
    pub from: PatchSide,
    pub to: PatchSide,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstallerInfo {
    pub size: u64,
    pub md5: Option<String>,
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RepoMetadata {
    pub tag_name: String,
    pub hashed: Vec<FileMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<PatchInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer: Option<InstallerInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deletes: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packing_info: Vec<Vec<String>>,
}
