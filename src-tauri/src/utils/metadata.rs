use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Metadata {
    pub file_name: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchItem {
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchInfo {
    pub file_name: String,
    pub size: u64,
    pub from: PatchItem,
    pub to: PatchItem,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InstallerInfo {
    pub size: u64,
    pub md5: Option<String>,
    pub xxh: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RepoMetadata {
    pub repo_name: String,
    pub tag_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<Metadata>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashed: Option<Vec<Metadata>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patches: Option<Vec<PatchInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installer: Option<InstallerInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packing_info: Option<Vec<Vec<String>>>,
}
