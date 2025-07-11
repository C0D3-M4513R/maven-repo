use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{ErrorKind, Read, SeekFrom, Write};
use std::path::{Path, PathBuf};
use reqwest::{Response, StatusCode};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;
use crate::remote::{read_remote};
use crate::repository::{RemoteUpstream, Repository, Upstream};

#[derive(Debug, Clone, serde_derive::Deserialize, serde_derive::Serialize, Eq, PartialEq)]
#[non_exhaustive]
pub struct FileMetadata {
    pub url: String,
    pub header_map: HashMap<String, Vec<String>>,
    pub local_last_modified: chrono::DateTime<chrono::Utc>,
    pub local_last_checked: chrono::DateTime<chrono::Utc>,
    pub hash: [u8; blake3::OUT_LEN],
}

impl FileMetadata {
    fn update_headers(&mut self, headers: &reqwest::header::HeaderMap) {
        self.header_map = headers.iter().fold(HashMap::new(), |mut map, (name, value)|{
            match str::from_utf8(value.as_bytes()) {
                Ok(v) => {
                    map.entry(name.to_string().to_lowercase()).or_default().push(v.to_string())
                },
                Err(err) => {
                    tracing::error!("Upstream sent a non-UTF8 header: {err}");
                }
            }
            map
        });
    }
    pub fn new_response(url: String, request: &'_ Response, hash: &[u8; blake3::OUT_LEN]) -> Self {
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

    pub async fn new_response_write(url: String, request: &'_ Response, hash: &[u8; blake3::OUT_LEN], path: &Path) -> Result<Self, std::io::Error> {
        let ret = Self::new_response(url, request, hash);
        ret.write(path).await?;
        Ok(ret)
    }

    #[inline]
    pub async fn validate(config: &Repository, str_path: &str, path: &Path, mem: &mut memmap2::Mmap, file: &mut tokio::fs::File, metadata: &std::fs::Metadata, hash: &[u8; blake3::OUT_LEN]) -> Result<Option<Self>, Vec<anyhow::Error>> {
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


    pub fn get_upstream(&self, config: &Repository) -> Option<RemoteUpstream> {
        for i in &config.upstreams {
            let i = match i {
                Upstream::Remote(i) => i,
                _ => continue,
            };
            if self.url.starts_with(&i.url) {
                return Some(i.clone());
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

    async fn new_file_impl(
        self_: Option<Self>,
        config: &Repository,
        str_path: &str,
        path: &Path,
        mem: &mut memmap2::Mmap,
        file: &mut tokio::fs::File,
        metadata: &std::fs::Metadata,
        hash: &[u8; blake3::OUT_LEN],
    ) -> Result<Option<Self>, Vec<anyhow::Error>> {
        let mut errors = Vec::new();
        type Ret = (String, Response, Vec<u8>, blake3::Hash);
        async fn check_task(
            task: anyhow::Result<Ret>,
            hash: &[u8; blake3::OUT_LEN],
            mem: &mut memmap2::Mmap,
            file: &mut tokio::fs::File,
        ) -> anyhow::Result<FileMetadata> {
            let (url, resp, bytes, new_hash) = match task {
                Ok(v) => v,
                Err(err) => {
                    return Err(anyhow::Error::from(err).context("Failed to read from remote"));
                },
            };

            if resp.status() != StatusCode::NOT_MODIFIED && hash != new_hash.as_bytes() && (mem.len() != bytes.len() || mem.as_ref() != bytes.as_slice()) {
                tracing::info!("Got a newer file for {url}");
                #[cfg(feature = "locking")]
                {
                    //Todo: This is hacky, to work around not being able to call lock on tokio's File.
                    // I don't use the rust BorrowedFD -> OwnedFD, since that duplicates the file handle,
                    // which might interact weirdly on non-linux platforms (it should be fine on specifically linux with the systemcalls being made).
                    // This should be replaced, once there is a tokio lock call available.
                    //
                    //Specifically use block_in_place here, instead of spawn_blocking,
                    // so that we know that there is no possible way for the tokio File to be dropped.
                    tokio::task::block_in_place(||{
                        use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd};
                        //Create a std File object from the file-descriptor of the tokio File-Object.
                        
                        let file = unsafe { core::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(file.as_fd().as_raw_fd())) };
                        //Re-Lock the file exclusively.
                        file.unlock()?;
                        file.lock()?;
                        Ok::<_, std::io::Error>(())
                    }).map_err(|err|anyhow::Error::from(err).context("Failed to lock the file"))?;

                }

                file.set_len(0).await.map_err(|err|anyhow::Error::from(err).context("Failed to set the original file length"))?;
                file.seek(SeekFrom::Start(0)).await.map_err(|err|anyhow::Error::from(err).context("Failed to seek the original file"))?;
                file.write_all(bytes.as_slice()).await.map_err(|err|anyhow::Error::from(err).context("Failed to write to the original file"))?;
                file.sync_all().await.map_err(|err|anyhow::Error::from(err).context("Failed to sync file (meta)data"))?;
                *mem = unsafe{memmap2::Mmap::map(&*file)}.map_err(|err|anyhow::Error::from(err).context("Failed to memmap file"))?;

                #[cfg(feature = "locking")]
                {
                    //Todo: This is hacky, to work around not being able to call lock on tokio's File.
                    // I don't use the rust BorrowedFD -> OwnedFD, since that duplicates the file handle,
                    // which might interact weirdly on non-linux platforms (it should be fine on specifically linux with the systemcalls being made).
                    // This should be replaced, once there is a tokio lock call available.
                    //
                    //Specifically use block_in_place here, instead of spawn_blocking,
                    // so that we know that there is no possible way for the tokio File to be dropped.
                    tokio::task::block_in_place(||{
                        use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd};
                        //Create a std File object from the file-descriptor of the tokio File-Object.
                        let file = unsafe { core::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(file.as_fd().as_raw_fd())) };
                        //Re-Lock the file shared
                        file.unlock()?;
                        file.lock_shared()?;
                        Ok::<_, std::io::Error>(())
                    }).map_err(|err|anyhow::Error::from(err).context("Failed to lock the file"))?;

                }
            } else {
                tracing::info!("File unchanged for {url}");
            }
            drop(bytes);

            let meta = FileMetadata::new_response(url, &resp, new_hash.as_bytes());
            Ok(meta)
        }
        let mut js = JoinSet::new();
        let mut header_map = if let Some(self_) = self_ {
            let headers = self_.get_request_headers();
            for i in &config.upstreams {
                let i = match i {
                    Upstream::Remote(v) => v,
                    _ => continue,
                };
                if self_.url.starts_with(&i.url) {
                    tracing::info!("Requesting {} for {str_path} metadata creation", self_.url);
                    match check_task(read_remote(self_.url.clone(), i.timeout, headers.clone()).await, hash, mem, file).await {
                        Ok(mut v) => {
                            v.local_last_modified = core::cmp::max(self_.local_last_modified, v.local_last_modified);
                            v.write(path).await.map_err(|err|vec![anyhow::Error::from(err).context("Failed to write file")])?;
                            return Ok(Some(v))
                        },
                        Err(err) => {
                            errors.push(err);
                        }
                    }
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
                        header_map.insert("If-Modified-Since", v);
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

        for upstream in &config.upstreams{
            let upstream = match upstream{
                Upstream::Remote(v) => v,
                _ => continue,
            };
            let url = format!("{}/{str_path}", upstream.url);
            tracing::info!("Requesting {url} for {str_path} metadata creation");
            js.spawn(read_remote(url, upstream.timeout, header_map.clone()));
        }

        while let Some(task) = js.join_next().await {
            let task = match task {
                Ok(v) => v,
                Err(err) => {
                    errors.push(anyhow::Error::from(err).context("Failed to request for metadata"));
                    continue;
                },
            };
            match check_task(task, hash, mem, file).await {
                Ok(v) => {
                    v.write(path).await.map_err(|err|vec![anyhow::Error::from(err).context("Failed to write file")])?;
                    return Ok(Some(v))
                },
                Err(err) => {
                    errors.push(err);
                }
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