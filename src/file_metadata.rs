use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use reqwest::{Response};
use crate::remote::{get_remote_url, read_remotes};
use crate::repository::{RemoteUpstream, Repository, Upstream};

#[derive(Debug, Clone, serde_derive::Deserialize, serde_derive::Serialize, Eq, PartialEq)]
#[non_exhaustive]
pub struct FileMetadata {
    pub url: Box<str>,
    pub header_map: HashMap<Box<str>, smallvec::SmallVec<[Box<str>;1]>>,
    pub local_last_modified: chrono::DateTime<chrono::Utc>,
    pub local_last_checked: chrono::DateTime<chrono::Utc>,
    pub hash: [u8; blake3::OUT_LEN],
}

impl FileMetadata {
    fn update_headers(&mut self, headers: &reqwest::header::HeaderMap) {
        self.header_map = headers.iter().fold(HashMap::new(), |mut map, (name, value)|{
            match str::from_utf8(value.as_bytes()) {
                Ok(v) => {
                    map.entry(name.to_string().to_lowercase().into_boxed_str()).or_default().push(v.into())
                },
                Err(err) => {
                    tracing::error!("Upstream sent a non-UTF8 header: {err}");
                }
            }
            map
        });
    }
    pub fn new_response(url: Box<str>, request: &'_ Response, hash: &[u8; blake3::OUT_LEN]) -> Self {
        let request_date = request.headers()
            .get("Date")
            .and_then(|v|v.to_str().ok())
            .and_then(|v|chrono::DateTime::parse_from_rfc2822(v).ok())
            .map(|v|v.into())
            .unwrap_or_else(||std::time::SystemTime::now().into());
        let request_last_modified = request.headers()
            .get("Last-Modified")
            .and_then(|v|v.to_str().ok())
            .and_then(|v|chrono::DateTime::parse_from_rfc2822(v).ok())
            .map(|v|v.into())
            .unwrap_or(request_date);
        let mut ret = Self{
            url,
            header_map: HashMap::new(),
            local_last_modified: request_last_modified,
            local_last_checked: request_date,
            hash: *hash,
        };
        ret.update_headers(request.headers());
        ret
    }

    pub async fn new_response_write(url: Box<str>, request: &'_ Response, hash: &[u8; blake3::OUT_LEN], path: &Path) -> Result<Self, std::io::Error> {
        let ret = Self::new_response(url, request, hash);
        ret.write(path).await?;
        Ok(ret)
    }

    #[inline]
    pub async fn validate(
        config: &Repository,
        str_path: &str,
        path: &Path,
        mem: &mut memmap2::Mmap,
        file: &mut tokio::fs::File,
        metadata: &std::fs::Metadata,
        hash: &blake3::Hash
    ) -> Result<Option<Self>, Vec<anyhow::Error>> {
        let self_ = match Self::open(path).await {
            Ok(v) => {
                let upstream = v.get_upstream(config);
                let fresh = upstream.and_then(|v|v.time_fresh)
                    .or(config.time_fresh)
                    .unwrap_or(crate::DEFAULT_FRESH);
                let fresh = chrono::TimeDelta::from_std(fresh).unwrap_or_else(|err| {
                    tracing::warn!("time_fresh is too large: {err}");
                    chrono::TimeDelta::MAX
                });

                let diff = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()) - v.local_last_checked;
                if diff > fresh || chrono::TimeDelta::zero() > diff {
                    tracing::info!("Revalidating metadata for {str_path}");
                    Some(v)
                } else {
                    return Ok(Some(v));
                }
            },
            Err(err) => match err.kind() {
                ErrorKind::NotFound => {
                    tracing::info!("Creating metadata for {str_path}");
                    None
                },
                _ => return Err(vec![anyhow::Error::from(err)])
            },
        };
        Self::new_file_impl(self_, config, str_path, path, mem, file, metadata, hash).await
    }


    pub fn get_upstream<'a>(&self, config: &'a Repository) -> Option<&'a RemoteUpstream> {
        for i in &config.upstreams {
            let i = match i {
                Upstream::Remote(i) => i,
                _ => continue,
            };
            if self.url.starts_with(&i.url) {
                return Some(i);
            }
        }
        None
    }

    #[inline]
    pub async fn open(path: &Path) -> Result<Self, std::io::Error> {
        let path = Self::file_path_to_metadata_path(path)?;
        let task = tokio::task::spawn_blocking(move ||{
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .open(&path)?;
            #[cfg(feature = "locking")]
            {
                file.lock_shared()?;
            }
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            let meta:Self = serde_json::from_slice(buf.as_slice())?;
            Ok::<_, std::io::Error>(meta)
        });

        task.await.unwrap_or_else(|err| Err(err.into()))
    }

    #[inline]
    async fn write(&self, path: &Path) -> Result<(), std::io::Error> {
        let path = Self::file_path_to_metadata_path(path)?;
        tracing::info!("Writing metadata to {}", path.display());
        let task = {
            let meta = self.clone();
            tokio::task::spawn_blocking(move ||{
                let json = serde_json::to_string(&meta)?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(path)?;
                #[cfg(feature = "locking")]
                {
                    file.lock()?;
                }
                file.set_len(0)?;
                file.write_all(json.as_bytes())?;
                drop(json);
                Ok::<_, std::io::Error>(())
            })
        };
        task.await.unwrap_or_else(|err|Err(err.into()))
    }

    fn file_path_to_metadata_path(
        path: &Path,
    ) -> Result<PathBuf, std::io::Error> {
        let mut path = path.to_path_buf();
        match path.file_name() {
            None => return Err(std::io::Error::other(anyhow::Error::msg("Path has no file-name"))),
            Some(v) => {
                let mut name = OsString::with_capacity(v.len() + 1 + 5);
                name.push(".");
                name.push(v);
                name.push(".json");
                path.set_file_name(name)
            }
        }
        Ok(path)
    }

    async fn new_file_impl<'a>(
        self_: Option<Self>,
        config: &Repository,
        str_path: &str,
        path: &Path,
        mem: &'a mut memmap2::Mmap,
        file: &mut tokio::fs::File,
        metadata: &std::fs::Metadata,
        hash: &blake3::Hash,
    ) -> Result<Option<Self>, Vec<anyhow::Error>> {
        let mut errors = Vec::new();
        let mut headers = if let Some(self_) = self_ {
            let headers = self_.get_request_headers();
            let urls = config.upstreams.iter().flat_map(|v|match v {
                    Upstream::Remote(v) => Some(v),
                    _ => None
                }).filter(|v|self_.url.starts_with(&v.url))
                .map(|v|(v.timeout, &*self_.url));
            let remote_responses = read_remotes(urls, str_path, headers.clone(), mem, file, hash).await;
            match remote_responses {
                Err(mut err) => {
                    errors.append(&mut err);
                },
                Ok((url, resp, new_hash)) => {
                    let mut meta = FileMetadata::new_response(Box::from(url), &resp, new_hash.unwrap_or(*hash).as_bytes());
                    meta.local_last_modified = core::cmp::max(self_.local_last_modified, meta.local_last_modified);
                    meta.write(path).await.map_err(|err|vec![anyhow::Error::from(err).context("Failed to write file")])?;
                    return Ok(Some(meta));
                }
            }
            
            headers
        } else {
            reqwest::header::HeaderMap::new()
        };
        match metadata.modified() {
            Ok(v) => {
                let date = chrono::DateTime::<chrono::Utc>::from(v).to_rfc2822();
                match reqwest::header::HeaderValue::from_str(date.as_str()) {
                    Ok(v)  => {
                        headers.insert("If-Modified-Since", v);
                    },
                    Err(err) => {
                        tracing::warn!("Error whilst converting RFC2822 Timestamp to Header value for If-Modified-Since header: {err}");
                    }
                }
            },
            Err(err) => {
                tracing::warn!("Error getting Modification Time: {err}");
            }
        }

        let urls = config.upstreams.iter().flat_map(|v|match v {
            Upstream::Remote(v) => Some(v),
            _ => None
        }).map(|v|{
            let url = get_remote_url(&v.url, str_path);
            tracing::info!("Requesting {url} for {str_path} metadata creation");
            (v.timeout, url)
        });
        let remote_responses = read_remotes(urls, str_path, headers.clone(), mem, file, hash).await;
        match remote_responses {
            Err(mut err) => {
                errors.append(&mut err);
            },
            Ok((url, resp, new_hash)) => {
                let meta = FileMetadata::new_response(url.into_boxed_str(), &resp, new_hash.unwrap_or(*hash).as_bytes());
                meta.write(path).await.map_err(|err|vec![anyhow::Error::from(err).context("Failed to write file")])?;
                return Ok(Some(meta));
            }
        }

        if !errors.is_empty() {
            Err(errors)
        } else {
            Ok(None)
        }
    }
    fn get_request_headers(&self) -> reqwest::header::HeaderMap {
        let mut header_map = reqwest::header::HeaderMap::new();
        if let Some(etag) = self.header_map.get("etag") {
            match reqwest::header::HeaderValue::from_str(&etag.join(", ")) {
                Ok(v) => {
                    header_map.insert("If-None-Match", v);
                },
                Err(err) => {
                    tracing::warn!("invalid header value when constructing If-None-Match header: {err}");
                }
            }
        }

        let mut last_modified = core::cmp::max(self.local_last_checked, self.local_last_modified);
        if let Some(values) = self.header_map.get("last-modified") {
            for value in values {
                match chrono::DateTime::parse_from_rfc2822(value) {
                    Ok(v) => {
                        last_modified = core::cmp::max(last_modified, v.to_utc());
                    },
                    Err(err) => {
                        tracing::warn!("Failed to parse Last-Modified header: {err}");
                    }
                }
            }
        }
        if let Some(values) = self.header_map.get("date") {
            for value in values {
                match chrono::DateTime::parse_from_rfc2822(value) {
                    Ok(v) => {
                        last_modified = core::cmp::max(last_modified, v.to_utc());
                    },
                    Err(err) => {
                        tracing::warn!("Failed to parse Date header: {err}");
                    }
                }
            }
        }
        match reqwest::header::HeaderValue::from_str(&last_modified.to_rfc2822()) {
            Ok(v) => {
                header_map.insert("If-Modified-Since", v);
            },
            Err(err) => {
                tracing::warn!("Error whilst converting RFC2822 Timestamp to Header value for If-Modified-Since header: {err}");
            }
        }

        header_map
    }
}