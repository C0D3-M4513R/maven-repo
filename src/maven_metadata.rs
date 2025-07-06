use std::collections::HashSet;

#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
pub struct MavenMetadata {
    pub group_id: String,
    pub artifact_id: String,
    pub versioning: Versioning,
}
#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
#[serde(rename_all="camelCase")]
pub struct Versioning {
    pub latest: String,
    pub release: String,
    #[serde(default)]
    pub versions: Option<Versions>,
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
    #[serde(default)]
    pub snapshot_versions: Option<SnapshotVersions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>, 
}

#[derive(Debug, Clone, Default, serde_derive::Deserialize, serde_derive::Serialize)]
pub struct Versions {
    #[serde(default)]
    pub version: HashSet<String>,
}
#[derive(Debug, Clone, serde_derive::Deserialize, serde_derive::Serialize, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[serde(rename_all="camelCase")]
pub struct Snapshot {
    pub timestamp: String,
    pub build_number: u64,
}
#[derive(Debug, Clone, Default, serde_derive::Deserialize, serde_derive::Serialize)]
#[serde(rename_all="camelCase")]
pub struct SnapshotVersions {
    #[serde(default)]
    pub snapshot_version: HashSet<SnapshotVersion>,
}
#[derive(Debug, Clone, serde_derive::Deserialize, serde_derive::Serialize, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[serde(rename_all="camelCase")]
pub struct SnapshotVersion {
    pub value: String,
    pub extension: Option<String>,
    #[serde(default)]
    pub classifier: Option<String>,
    pub updated: String,
}