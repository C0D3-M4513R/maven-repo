use std::io::SeekFrom;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use reqwest::StatusCode;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::time::Instant;
use crate::err::GetRepoFileError;
use crate::file_metadata::FileMetadata;
use crate::get::StoredRepoPath;
use crate::remote::get_remote_request;
use crate::repository::{RemoteUpstream, Repository};
use crate::server_timings::AsServerTimingDuration;
use crate::timings::ServerTimings;

pub async fn serve_remote_repository(
    remote: RemoteUpstream,
    str_path: Arc<str>,
    repo: String,
    path: Arc<Path>,
    config: Arc<Repository>,
    request_url: Arc<str>,
    remote_client: Option<IpAddr>, 
) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;
    let mut timings = ServerTimings::new();

    let (url, response) = get_remote_request(
        &remote,
        &str_path,
        &request_url,
        remote_client
    );
    let response = match 
        response
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

    if config.stores_remote_upstream.unwrap_or(true) {
        next = Instant::now();
        timings.push_iter_nodelim(["resolveImplRemoteRequestHead;dur=", (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Send Request to Remote and wait for Headers""#]);
        core::mem::swap(&mut start, &mut next);

        let path = Path::new(&repo).join(path);
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::error!("Error creating directories to {}: {err}", path.display());
            }
        }

        next = Instant::now();
        timings.push_iter_nodelim([r#"resolveImplRemoteFSCreateDirAll;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Create All Local Dirs""#]);
        core::mem::swap(&mut start, &mut next);

        let (path, file, mut timings, mut start) = match tokio::task::spawn_blocking(move ||{
            let mut start = start;
            let mut next;
            let file = std::fs::File::create_new(&path)?;

            next = Instant::now();
            timings.push_iter_nodelim([r#"resolveImplRemoteFSCreateFile;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Create new Local File""#]);
            core::mem::swap(&mut start, &mut next);

            #[cfg(feature = "locking")]
            file.lock()?;

            next = Instant::now();
            timings.push_iter_nodelim([r#"resolveImplRemoteFSCreateFile;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Lock Local File Exclusively""#]);
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
        timings.push_iter_nodelim([r#"resolveImplRemoteBeforeBodyRead;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Task Scheduling Delay""#]);
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
            if current_size >= config.max_file_size.unwrap_or(crate::DEFAULT_MAX_FILE_SIZE) {
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
        timings.push_iter_nodelim([r#"resolveImplRemoteBodyRead;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Read Remote Response in Chunks to Local File and Hash""#]);
        core::mem::swap(&mut start, &mut next);
        
        match FileMetadata::new_response_write(url, &response, hash.as_bytes(), &path).await {
            Ok(_) => {},
            Err(err) => {
                tracing::error!("Failed to write Metadata for {repo}/{str_path}: {err:#?}");
            }
        };
        next = Instant::now();
        timings.push_iter_nodelim([r#"resolveImplRemoteMetadataWrite;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Write File Metadata Info""#]);
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
        timings.push_iter_nodelim([r#"resolveImplRemoteFSRelockMemmap;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Remote: Release Exclusive Lock, Aquire Shared Lock and Memory-Map File""#]);
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