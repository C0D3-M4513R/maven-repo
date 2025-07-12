use std::io::SeekFrom;
use std::net::IpAddr;
use std::ops::Deref;
use std::time::Duration;
use anyhow::Context;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use reqwest::StatusCode;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use crate::repository::RemoteUpstream;

pub fn get_remote_url(
    remote: &str,
    path: &str,
) -> String {
    let mut out = String::with_capacity(remote.len() + path.len() + 1);
    out.push_str(remote.strip_suffix("/").unwrap_or(remote));
    if !path.starts_with("/") {
        out.push('/');
    } 
    out.push_str(path);
    out
}
pub fn get_remote_request(
    remote: &RemoteUpstream,
    str_path: &str,
    request_url: &str,
    remote_client: Option<IpAddr>,
) -> (String, reqwest::RequestBuilder) {
    let url = get_remote_url(&remote.url, str_path);
    let mut req = crate::CLIENT
        .get(&url)
        .timeout(remote.timeout);

    if !request_url.is_empty() {
        req = req.header("X-Downstream-Repo-Link", request_url);
    }

    if let Some(v) = remote_client {
        req = req.header("X-Forwarded-For", v.to_canonical().to_string());
    }

    (url, req)
}
pub async fn read_remote<T: Deref<Target = str>>(
    url: T,
    timeout: Duration,
    headers: reqwest::header::HeaderMap,
    mem: &memmap2::Mmap,
    file: &tokio::sync::Mutex<&mut tokio::fs::File>,
) -> anyhow::Result<(T, reqwest::Response, Option<blake3::Hash>, bool)>{
    let mut res = crate::CLIENT.get(&*url)
        .timeout(timeout)
        .headers(headers)
        .send()
        .await?;
    let is304 = res.status() == StatusCode::NOT_MODIFIED;
    if res.status() != StatusCode::OK && !is304 {
        return Err(anyhow::Error::msg(format!("Response Status-Code was not Ok or NotModified: {res:?}")));
    }

    let mut hasher = blake3::Hasher::new();
    let hash;
    let mut byte_to_write = None;
    let needed_writes;
    if !is304 {
        let mut current_pos = 0usize;
        while let Some(chunk) = res.chunk().await? {
            hasher.update(chunk.as_ref());
            if let Some(mem) = mem.get(current_pos..current_pos+chunk.len()) {
                if mem != &*chunk {
                    byte_to_write = Some(chunk);
                    break;
                }
            } else {
                byte_to_write = Some(chunk);
                break;
            }

            current_pos = match current_pos.checked_add(chunk.len()){
                Some(v) => v,
                None => return Err(anyhow::Error::msg("Failed to add to current position")),
            };
        }
        if let Some(byte_to_write) = byte_to_write {
            tracing::info!("Got a newer file for {}", &*url);
            needed_writes = true;
            let current_pos = u64::try_from(current_pos).context("Could not convert current position from usize to u64")?;
            let mut file = file.lock().await;
            let file = &mut **file;

            #[cfg(feature = "locking")]
            {
                use crate::file_ext::TokioFileExt;
                file.relock().await.map_err(|err|anyhow::Error::from(err).context("Failed to lock the file"))?;
            }

            file.seek(SeekFrom::Start(current_pos)).await.context("Error Seeking file")?;
            file.set_len(current_pos).await.context("Error setting File Length")?;

            let mut file = tokio::io::BufWriter::new(file);
            file.write_all(&*byte_to_write).await.context("Error writing to the File")?;

            while let Some(chunk) = res.chunk().await? {
                hasher.update(chunk.as_ref());
                file.write_all(&*chunk).await.context("Error writing to the File")?;
            }
            let file = file.into_inner();
            file.flush().await.context("Error flushing file")?;
            file.sync_all().await.context("Error syncing file")?;

            #[cfg(feature = "locking")]
            {
                use crate::file_ext::TokioFileExt;
                file.relock_shared().await.map_err(|err|anyhow::Error::from(err).context("Failed to lock the file shared"))?;
            }
        } else{
            needed_writes = false;
        }
        hash = Some(hasher.finalize());
    } else {
        hash = None;
        needed_writes = false;
    }

    Ok((url, res, hash, needed_writes))
}
pub async fn read_remotes<'a, T: Deref<Target = str> + Send + 'a>(
    upstreams: impl IntoIterator<Item = (Duration, T)>,
    str_path: &str,
    headers: reqwest::header::HeaderMap,
    mem: &mut memmap2::Mmap,
    file: &mut tokio::fs::File,
    hash: &blake3::Hash,
) -> Result<(T, reqwest::Response, Option<blake3::Hash>), Vec<anyhow::Error>> {
    let file = tokio::sync::Mutex::new(file);
    let mut futures = FuturesUnordered::new();
    for (i, url) in upstreams {
        tracing::info!("Requesting {} for {str_path} metadata creation", &*url);
        futures.push(read_remote(url, i, headers.clone(), &mem, &file));
    }

    let mut errors = Vec::new();
    while let Some(v) = futures.next().await {
        match v {
            Err(err) => {
                errors.push(err);
            }
            Ok((url, resp, new_hash, needs_update)) => {
                drop(futures);
                let file = file.into_inner();
                if needs_update {
                    *mem = match unsafe{memmap2::Mmap::map(&*file)}.context("Failed to memmap file") {
                        Ok(v) => v,
                        Err(err) => {
                            errors.push(err);
                            return Err(errors);
                        }
                    }
                } else {
                    if let Some(new_hash) = new_hash
                        && new_hash != *hash {
                        errors.push(anyhow::anyhow!("Hash check failed. new_hash: {new_hash} != old_hash: {hash}"));
                        return Err(errors);
                    }

                    tracing::info!("File unchanged for {}", &*url);
                }
                return Ok((url, resp, new_hash))
            }
        }
    }
    Err(errors)
}
