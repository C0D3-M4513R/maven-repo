use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use chrono::{Datelike, Timelike};
use rocket::http::{ContentType, Status};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use crate::err::GetRepoFileError;
use crate::maven_metadata::{MavenMetadata, Snapshot, SnapshotVersion, Versioning};
use crate::status::{Content, Return};

type MavenMetadataReturn = (PathBuf, File, MavenMetadata, String);

pub struct SnapshotInfo<'a> {
    pub timestamp: &'a str,
    pub build_number: u64,
}
pub struct PathInfo<'a> {
    pub group: Vec<&'a str>,
    pub artifact: &'a str,
    pub version: &'a str,
    pub snapshot: Option<SnapshotInfo<'a>>,
    pub classifier: Option<&'a str>,
    pub extension: Option<&'a str>,
}

impl<'a> PathInfo<'a> {
    pub fn dotted_group(&self) -> String {
        self.group.iter().fold(String::new(), |mut initial, v|{
            if !initial.is_empty() {
                initial.push('.');
            }
            initial.push_str(v);
            initial
        })
    }
    pub fn parse(path: &'a Path) -> Result<Self, Return> {
        let mut file_name = None;
        let mut version = None;
        let mut artifact = None;
        let mut group = Vec::new();
        for i in path.components().rev() {
            let i = match i {
                Component::CurDir => continue,
                Component::ParentDir |
                Component::RootDir |
                Component::Prefix(_)
                => return Err(GetRepoFileError::BadRequestPath.to_return()),
                Component::Normal(v) => v,
            };
            let i = match i.to_str() {
                None => return Err(GetRepoFileError::InvalidUTF8.to_return()),
                Some(v) => v,
            };
            if file_name.is_none() {
                file_name = Some(i);
            } else if version.is_none() {
                version = Some(i);
            } else if artifact.is_none() {
                artifact = Some(i);
            } else {
                group.push(i);
            }
        }
        let file_name = match version {
            None => return Err(Return{
                status: Status::BadRequest,
                content: Content::Str("Didn't find a File-Name in the path"),
                content_type: ContentType::Text,
                header_map: None,
            }),
            Some(v) => v,
        };
        let version = match version {
            None => return Err(Return{
                status: Status::BadRequest,
                content: Content::Str("Didn't find a Version in the path"),
                content_type: ContentType::Text,
                header_map: None,
            }),
            Some(v) => v,
        };
        let artifact = match artifact {
            None => return Err(Return{
                status: Status::BadRequest,
                content: Content::Str("Didn't find an Artifact-Id in the path"),
                content_type: ContentType::Text,
                header_map: None,
            }),
            Some(v) => v,
        };
        if group.is_empty() {
            return Err(Return{
                status: Status::BadRequest,
                content: Content::Str("Didn't find a Group-Id in the path"),
                content_type: ContentType::Text,
                header_map: None,
            })
        }
        let group = group.into_iter().rev().collect();

        let (file_name, extension) = file_name.rsplit_once(".").map(|(name, ext)|(name, Some(ext))).unwrap_or((file_name, None));
        let file_name = match file_name.strip_prefix(artifact).and_then(|v|v.strip_prefix("-")) {
            None => return Err(Return{
                    status: Status::BadRequest,
                    content: Content::Str("File didn't begin with artifact-id"),
                    content_type: ContentType::Text,
                    header_map: None,
                }),
            Some(v) => v,
        };
        let (version, is_snapshot) = match version.strip_suffix("-SNAPSHOT") {
            Some(v) => (v, true),
            None => (version, false),
        };
        let file_name = match file_name.strip_prefix(version).and_then(|v|v.strip_prefix("-")) {
            None => return Err(Return{
                status: Status::BadRequest,
                content: Content::Str("File didn't contain version"),
                content_type: ContentType::Text,
                header_map: None,
            }),
            Some(v) => v,
        };
        let (file_name, snapshot) = if is_snapshot {
            let (timestamp, file_name) = match file_name.split_once("-") {
                None => return Err(Return{
                    status: Status::BadRequest,
                    content: Content::Str("File didn't contain Snapshot timestamp"),
                    content_type: ContentType::Text,
                    header_map: None,
                }),
                Some(v) => v,
            };
            let (build_number, file_name) = match file_name.split_once("-") {
                None => return Err(Return{
                    status: Status::BadRequest,
                    content: Content::Str("File didn't contain Snapshot build-number"),
                    content_type: ContentType::Text,
                    header_map: None,
                }),
                Some(v) => v,
            };
            let build_number = match u64::from_str_radix(build_number, 10) {
                Ok(v) => v,
                Err(err) => return Err(Return{
                    status: Status::BadRequest,
                    content: Content::String(format!("build-number in filename couldn't be parsed as a string: build-number: {build_number}, err: {err}")),
                    content_type: ContentType::Text,
                    header_map: None,
                })
            };
            (file_name, Some(SnapshotInfo{
                timestamp,
                build_number,
            }))
        } else {
            (file_name, None)
        };

        Ok(Self{
            group,
            artifact,
            version,
            snapshot,
            classifier: if file_name.len() > 0 { Some(file_name) } else {None},
            extension,
        })
    }

    #[allow(dead_code)]
    pub async fn get_metadata(&self, repo: &str) -> Result<MavenMetadataReturn, Return> {
        self.get_metadata_int(repo, false, false).await
    }
    async fn get_metadata_int(&self, repo: &str, snapshot: bool, lock_exclusive: bool) -> Result<MavenMetadataReturn, Return> {
        let mut metadata_path = PathBuf::new();
        metadata_path.push(repo);
        metadata_path.extend(&self.group);
        metadata_path.push(self.artifact);
        if snapshot {
            metadata_path.push(format!("{}-SNAPSHOT", self.version));
        }
        metadata_path.push("maven-metadata.xml");
        let file = match tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&metadata_path)
            .await
        {
            Err(err) => {
                tracing::error!("Error creating or opening maven-metadata {}: {err}", metadata_path.display());
                return Err(Return{
                    status: Status::InternalServerError,
                    content: Content::Str("Error creating or opening maven-metadata file"),
                    content_type: ContentType::Text,
                    header_map: None,
                })
            }
            Ok(v) => v,
        };
        let mut file = if lock_exclusive {
            let file = file.into_std().await;
            //This potentially waits for any other tasks to 
            let file = match tokio::task::spawn_blocking(||{
                #[cfg(feature = "locking")]
                let lock = file.lock();
                #[cfg(not(feature = "locking"))]
                let lock = Ok::<_, std::io::Error>(());

                (file, lock)
            }).await {
                Ok((file, Ok(()))) => {
                    file
                }
                Ok((_, Err(err))) => {
                    tracing::error!("Error locking maven-metadata to String {}: {err}", metadata_path.display());
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Error reading maven-metadata to String"),
                        content_type: ContentType::Text,
                        header_map: None,
                    })
                }
                Err(err) => {
                    tracing::error!("Error locking maven-metadata to String {}: {err}", metadata_path.display());
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Error reading maven-metadata to String"),
                        content_type: ContentType::Text,
                        header_map: None,
                    })
                }
            };
            File::from_std(file)
        } else {file};
        let mut contents = String::new();
        match file.read_to_string(&mut contents).await {
            Err(err) => {
                tracing::error!("Error reading maven-metadata to String {}: {err}", metadata_path.display());
                return Err(Return{
                    status: Status::InternalServerError,
                    content: Content::Str("Error reading maven-metadata to String"),
                    content_type: ContentType::Text,
                    header_map: None,
                })
            }
            Ok(_) => {},
        }
        let metadata = if !contents.is_empty(){
            match quick_xml::de::from_str(&contents) {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!("Failed to parse maven-metadata.xml {}: {err}", metadata_path.display());
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Error deserializing maven-metadata"),
                        content_type: ContentType::Text,
                        header_map: None,
                    })
                }
            }
        } else {
          MavenMetadata{
              group_id: self.dotted_group(),
              artifact_id: self.artifact.to_string(),
              versioning: Versioning {
                  latest: self.version.to_string(),
                  release: self.version.to_string(),
                  versions: None,
                  snapshot: None,
                  snapshot_versions: None,
                  last_updated: Some(get_timestamp_last_updated()),
              },
          }
        };

        Ok((metadata_path, file, metadata, contents))
    }
    pub async fn get_merged_metadata(&self, repo: &str, action: rocket::http::Method) -> Result<Vec<MavenMetadataReturn>, Return> {
        let mut out = Vec::new();
        let (path, file, mut metadata, _) = self.get_metadata_int(repo, false, true).await?;
        
        match &self.snapshot {
            Some(snapshot) => {
                let (snapshot_path, snapshot_file, mut snapshot_metadata, _) = self.get_metadata_int(repo, true, true).await?;
                match action {
                    rocket::http::Method::Delete => {
                        let value = format!("{}-{}-{}", self.version, snapshot.timestamp, snapshot.build_number);
                        snapshot_metadata.versioning.snapshot_versions.get_or_insert_default().snapshot_version.retain(|v|
                            v.value != value ||
                                v.extension != self.extension.map(ToOwned::to_owned) ||
                                v.classifier != self.classifier.map(ToOwned::to_owned)
                        );
                        
                        //There is a snapshot entry in the snapshot metadata
                        if let Some(v) = &snapshot_metadata.versioning.snapshot {
                            //Which matches what we are deleting
                            if v.timestamp == snapshot.timestamp && v.build_number == snapshot.build_number {
                                //And there aren't any other artifacts with the same version,
                                if !snapshot_metadata.versioning.snapshot_versions
                                    .get_or_insert_default()
                                    .snapshot_version
                                    .iter()
                                    .any(|version|version.value == value) 
                                {
                                    //So we need to find the highest version and replace the snapshot entry.
                                    if let Some(version) = snapshot_metadata.versioning.snapshot_versions.get_or_insert_default().snapshot_version.iter().max() {
                                        let mut iter = version.value.rsplitn(2, "-");
                                        let build_number = match iter.next() {
                                            Some(v) => v,
                                            None => return Err(Return{
                                                status: Status::InternalServerError,
                                                content: Content::Str("Next version doesn't have a build_number"),
                                                content_type: ContentType::Text,
                                                header_map: None,
                                            })
                                        };
                                        let build_number = match u64::from_str_radix(build_number, 10) {
                                            Ok(v) => v,
                                            Err(err) => {
                                                tracing::error!("Failed to parse build_number {}: build_number:{build_number} err:{err}", path.display());
                                                return Err(Return{
                                                    status: Status::InternalServerError,
                                                    content: Content::Str("Next version doesn't have a valid build_number"),
                                                    content_type: ContentType::Text,
                                                    header_map: None,
                                                })
                                            }
                                        };
                                        let timestamp = match iter.next() {
                                            Some(v) => v,
                                            None => return Err(Return{
                                                status: Status::InternalServerError,
                                                content: Content::Str("Next version doesn't have a timestamp"),
                                                content_type: ContentType::Text,
                                                header_map: None,
                                            })
                                        };
                                        snapshot_metadata.versioning.snapshot = Some(Snapshot{
                                            timestamp: timestamp.to_owned(),
                                            build_number,
                                        });
                                    } else {
                                        snapshot_metadata.versioning.snapshot = None;
                                    }
                                    
                                }
                            }
                        }
                        // No need to modify metadata here.
                        // I currently see no issue with having a snapshot listed inside the main maven-metadata,
                        // but having no snapshotVersion's in the Version-Level Maven-Metadata.
                        out.push((snapshot_path, snapshot_file, snapshot_metadata));
                    }
                    rocket::http::Method::Put => {
                        snapshot_metadata.versioning.snapshot = Some(Snapshot {
                            timestamp: snapshot.timestamp.to_owned(),
                            build_number: snapshot.build_number,
                        });
                        snapshot_metadata.versioning.snapshot_versions.get_or_insert_default().snapshot_version.insert(SnapshotVersion {
                            value: format!("{}-{}-{}", self.version, snapshot.timestamp, snapshot.build_number),
                            extension: self.extension.map(ToOwned::to_owned),
                            classifier: self.classifier.map(ToOwned::to_owned),
                            updated: get_timestamp_last_updated(),
                        });
                        out.push((snapshot_path, snapshot_file, snapshot_metadata));
                        if metadata.versioning.versions.get_or_insert_default().version.insert(format!("{}-SNAPSHOT", self.version)) {
                            out.push((path, file, metadata));
                        }
                    }
                    _ => { },
                }
            },
            None => {
                if match action {
                    rocket::http::Method::Delete => metadata.versioning.versions.get_or_insert_default().version.remove(self.version),
                    rocket::http::Method::Put => metadata.versioning.versions.get_or_insert_default().version.insert(self.version.to_owned()),
                   _ => false,
                } {
                    out.push((path, file, metadata));
                }
            }
        };

        for (_, _, metadata) in &mut out {
            metadata.versioning.last_updated = Some(get_timestamp_last_updated());
        }
        let out = {
            let mut new_out = Vec::new();
            for (path, file, meta)  in out {
                let ser = match quick_xml::se::to_string(&meta) {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::error!("Failed to serialize maven metadata '{}' value '{meta:#?}': {err}", path.display());
                        return Err(Return{
                            status: Status::InternalServerError,
                            content: Content::Str("Failed to serialize altered maven metadata"),
                            content_type: ContentType::Text,
                            header_map: None,
                        })
                    }
                };
                new_out.push((path, file, meta, ser));
            }
            new_out
        };

        Ok(out)
    }
}

pub fn get_timestamp_last_updated() -> String{
    let time = chrono::DateTime::<chrono::Utc>::from(SystemTime::now());
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", time.year(), time.month(), time.day(), time.hour(), time.minute(), time.second())
}