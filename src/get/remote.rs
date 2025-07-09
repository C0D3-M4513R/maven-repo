use std::io::SeekFrom;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use reqwest::StatusCode;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::time::Instant;
use crate::err::GetRepoFileError;
use crate::get::StoredRepoPath;
use crate::repository::RemoteUpstream;
use crate::server_timings::AsServerTimingDuration;

pub async fn serve_remote_repository(
    remote: RemoteUpstream,
    str_path: Arc<str>,
    repo: String,
    path: Arc<Path>,
    stores_remote_upstream: bool,
    limit: u64,
    request_url: Arc<str>,
    remote_client: Option<IpAddr>, 
) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;
    let mut timings = Vec::new();

    let url = remote.url;
    let response = match crate::CLIENT
        .get(format!("{url}/{str_path}"))
        .header("Referrer", request_url.as_ref())
        .header("X-Forwarded-For", remote_client.map(|v|v.to_canonical().to_string()).unwrap_or_else(String::new))
        .timeout(remote.timeout)
        .send()
        .await {
        Err(err) => {
            tracing::warn!("Error contacting Upstream repo: {err}");
            return Err(vec![GetRepoFileError::UpstreamRequestError])
        },
        Ok(v) => v,
    };

    match response.status() {
        StatusCode::OK => {},
        StatusCode::NOT_FOUND => return Err(vec![GetRepoFileError::NotFound]),
        code => {
            tracing::warn!("Error contacting Upstream repo didn't respond with Ok: {code}");
            return Err(vec![GetRepoFileError::UpstreamStatus]);
        }
    }

    if stores_remote_upstream {
        next = Instant::now();
        timings.push(format!(r#"resolveImplRemoteRequestHead;dur={};desc="Resolve Impl: Remote: Send Request to Remote and wait for Headers""#, (next-start).as_server_timing_duration()));
        core::mem::swap(&mut start, &mut next);

        let path = Path::new(&repo).join(path);
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::error!("Error creating directories to {}: {err}", path.display());
            }
        }

        next = Instant::now();
        timings.push(format!(r#"resolveImplRemoteFSCreateDirAll;dur={};desc="Resolve Impl: Remote: Create All Local Dirs""#, (next-start).as_server_timing_duration()));
        core::mem::swap(&mut start, &mut next);

        let (path, file, mut timings, mut start) = match tokio::task::spawn_blocking(move ||{
            let mut start = start;
            let mut next;
            let file = std::fs::File::create_new(&path)?;

            next = Instant::now();
            timings.push(format!(r#"resolveImplRemoteFSCreateFile;dur={};desc="Resolve Impl: Remote: Create new Local File""#, (next-start).as_server_timing_duration()));
            core::mem::swap(&mut start, &mut next);

            #[cfg(feature = "locking")]
            file.lock()?;

            next = Instant::now();
            timings.push(format!(r#"resolveImplRemoteFSCreateFile;dur={};desc="Resolve Impl: Remote: Lock Local File Exclusively""#, (next-start).as_server_timing_duration()));
            core::mem::swap(&mut start, &mut next);

            Ok::<_, std::io::Error>((path, file, timings, start))
        }).await {
            Ok(Ok(v)) => v,
            Ok(Err(v)) => {
                tracing::error!("Error Creating File: {v}");
                return Err(vec![GetRepoFileError::FileCreateFailed]);
            },
            Err(v) => {
                tracing::error!("Panicked Creating File: {v}");
                return Err(vec![GetRepoFileError::FileCreateFailed]);
            }
        };
        let file = tokio::fs::File::from_std(file);

        let mut response = response;
        let mut file = tokio::io::BufWriter::new(file);
        let mut hash = blake3::Hasher::default();
        let mut current_size = 0u64;

        let mut next = Instant::now();
        timings.push(format!(r#"resolveImplRemoteBeforeBodyRead;dur={};desc="Resolve Impl: Remote: Task Scheduling Delay""#, (next-start).as_server_timing_duration()));
        core::mem::swap(&mut start, &mut next);
        loop {
            let body = match response.chunk().await {
                Err(err) => {
                    tracing::warn!("Error contacting Upstream repo: {err}");
                    return Err(vec![GetRepoFileError::UpstreamBodyReadError]);
                }
                Ok(Some(v)) => v,
                Ok(None) => break,
            };
            current_size += body.len() as u64;
            if current_size >= limit {
                return Err(vec![GetRepoFileError::UpstreamFileTooLarge])
            }
            hash.update(&*body);

            match file.write_all(&*body).await {
                Ok(()) => {},
                Err(err) => {
                    tracing::error!("Error writing to File {}: {err}", path.display());
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => {},
                        Err(err) => {
                            tracing::error!("Error deleting File after error writing to File {}: {err}", path.display());
                        }
                    }
                    return Err(vec![GetRepoFileError::FileWriteFailed]);
                }
            }
        }
        let hash = hash.finalize();
        match file.shutdown().await  {
            Ok(()) => {},
            Err(err) => {
                tracing::error!("Error flushing File {}: {err}", path.display());
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {},
                    Err(err) => {
                        tracing::error!("Error deleting File after error flushing File {}: {err}", path.display());
                    }
                }
                return Err(vec![GetRepoFileError::FileFlushFailed]);
            }
        }
        let mut file = file.into_inner();
        match file.seek(SeekFrom::Start(0)).await  {
            Ok(_) => {},
            Err(err) => {
                tracing::error!("Error seeking File {}: {err}", path.display());
                return Err(vec![GetRepoFileError::FileSeekFailed]);
            }
        }
        next = Instant::now();
        timings.push(format!(r#"resolveImplRemoteBodyRead;dur={};desc="Resolve Impl: Remote: Read Remote Response in Chunks to Local File and Hash""#, (next-start).as_server_timing_duration()));
        core::mem::swap(&mut start, &mut next);

        let file = file.into_std().await;
        let (metadata, map) = match tokio::task::spawn_blocking(move ||{
            #[cfg(feature = "locking")]
            file.unlock()?;
            #[cfg(feature = "locking")]
            file.lock_shared()?;
            let metadata = file.metadata()?;
            let map = unsafe { memmap2::Mmap::map(&file)}?;
            map.advise(memmap2::Advice::Sequential)?;
            Ok::<_, std::io::Error>((metadata, map))
        }).await {
            Ok(Ok(v)) => v,
            Ok(Err(v)) => {
                tracing::error!("Error relocking(exclusive->shared) File: {v}");
                return Err(vec![GetRepoFileError::FileLockFailed]);
            },
            Err(v) => {
                tracing::error!("Panicked relocking(exclusive->shared) File: {v}");
                return Err(vec![GetRepoFileError::FileLockFailed]);
            }
        };

        next = Instant::now();
        timings.push(format!(r#"resolveImplRemoteFSRelockMemmap;dur={};desc="Resolve Impl: Remote: Release Exclusive Lock, Aquire Shared Lock and Memory-Map File""#, (next-start).as_server_timing_duration()));
        core::mem::swap(&mut start, &mut next);
        Ok(StoredRepoPath::Mmap{
            metadata,
            data: map,
            hash,
            timing: timings
        })
    } else {
        Ok(StoredRepoPath::Upstream(response))
    }
}