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
    #[serde(flatten)]
    pub inner_versioning: InnerVersioning,
    pub last_updated: u64, 
}
#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
#[serde(untagged)]
pub enum InnerVersioning {
    Versions{
        versions: Versions
    },
    Snapshot{
        snapshot: Snapshot,
        #[serde(default)]
        snapshot_versions: Vec<SnapshotVersions>
    }
}

#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
pub struct Versions {
    #[serde(default)]
    pub version: Vec<String>,
}
#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
#[serde(rename_all="camelCase")]
pub struct Snapshot {
    pub timestamp: String,
    pub build_number: u64,
}
#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
#[serde(rename_all="camelCase")]
pub struct SnapshotVersions {
    #[serde(default)]
    pub snapshot_version: Vec<SnapshotVersion>,
}
#[derive(Debug, serde_derive::Deserialize, serde_derive::Serialize)]
#[serde(rename_all="camelCase")]
pub struct SnapshotVersion {
    #[serde(default)]
    pub classifier: Option<String>,
    pub extension: String,
    pub value: String,
    pub updated: u64,
}