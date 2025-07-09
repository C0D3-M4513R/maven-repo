use std::collections::HashSet;
use std::fs::Metadata;
use std::io::ErrorKind;
use std::path::PathBuf;
use tokio::time::Instant;
use crate::err::GetRepoFileError;
use crate::get::StoredRepoPath;
use crate::server_timings::AsServerTimingDuration;

pub async fn serve_repository_stored_path(path: PathBuf, display_dir: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;
    let mut errors = Vec::new();
    let mut timing = Vec::new();
    
    macro_rules! handle_err {
        ($err:ident, $path:ident) => {
            match $err.kind(){
                ErrorKind::NotFound => errors.push(GetRepoFileError::NotFound),
                _ => {
                    tracing::warn!("Error opening file {}: {}", $path.display(), $err);
                    errors.push(GetRepoFileError::OpenFile)
                },
            }
            return Err(errors);
        };
    }
    match tokio::fs::metadata(&path).await {
        Ok(metadata) => {
            //We only check, if the metadata says, that this is a dir, because non-files might also be able to be read (e.g. unix sockets).
            //Theoretically everything inside the maven repos should be a directory or file.
            if metadata.is_dir() {
                if !display_dir {
                    errors.push(GetRepoFileError::NotFound);
                } else {
                    match serve_repository_stored_dir(&path, metadata).await {
                        Ok(v) => return Ok(v),
                        Err(mut err) => errors.append(&mut err),
                    }
                }
                return Err(errors);
            }

            next = Instant::now();
            timing.push(format!(r#"resolveImplLocalFSMetadata;dur={};desc="Resolve Impl: Local: Query File Metadata""#, (next-start).as_server_timing_duration()));
            core::mem::swap(&mut start, &mut next);

            let (map, hash, mut timing, mut start) = match tokio::task::spawn_blocking(move ||{
                let mut timings = Vec::new();
                let mut start = start;
                let mut next;

                let file = match std::fs::File::open(&path) {
                    Ok(v) => v,
                    Err(v) => return (Err(v), path),
                };

                next = Instant::now();
                timings.push(format!(r#"resolveImplLocalOpenFile;dur={};desc="Resolve Impl: Local: Opening File""#, (next-start).as_server_timing_duration()));
                core::mem::swap(&mut start, &mut next);

                #[cfg(feature = "locking")]
                {
                    match file.lock_shared() {
                        Ok(()) => {},
                        Err(v) => return (Err(v), path),
                    };

                    next = Instant::now();
                    timings.push(format!(r#"resolveImplLocalSharedFileLock;dur={};desc="Resolve Impl: Local: Aquiring Shared File Lock""#, (next-start).as_server_timing_duration()));
                    core::mem::swap(&mut start, &mut next);
                }

                let map = match unsafe { memmap2::Mmap::map(&file) }  {
                    Ok(v) => v,
                    Err(v) => return (Err(v), path),
                };
                match map.advise(memmap2::Advice::Sequential)  {
                    Ok(()) => {},
                    Err(v) => return (Err(v), path),
                };
                match map.advise(memmap2::Advice::WillNeed)  {
                    Ok(()) => {},
                    Err(v) => return (Err(v), path),
                };
                #[cfg(target_os = "linux")]
                {
                    match map.advise(memmap2::Advice::PopulateRead)  {
                        Ok(()) => {},
                        Err(v) => return (Err(v), path),
                    };

                }
                next = Instant::now();
                timings.push(format!(r#"resolveImplLocalMemMapFile;dur={};desc="Resolve Impl: Local: Memory Map file""#, (next-start).as_server_timing_duration()));
                core::mem::swap(&mut start, &mut next);

                let hash = blake3::Hasher::default().update(&*map).finalize();
                next = Instant::now();
                timings.push(format!(r#"resolveImplLocalETagFile;dur={};desc="Resolve Impl: Local: Calculate File ETag""#, (next-start).as_server_timing_duration()));
                core::mem::swap(&mut start, &mut next);

                (Ok::<_, std::io::Error>((map, hash, timings, start)), path)
            }).await {
                Ok((Ok(v), _)) => v,
                Ok((Err(err), path)) => {
                    handle_err!(err, path);
                }
                Err(err) => {
                    tracing::error!("Panicked whilst opening file: {}", err);
                    errors.push(GetRepoFileError::OpenFile);
                    return Err(errors);
                }
            };

            next = Instant::now();
            timing.push(format!(r#"resolveImplLocalScheduleDelay;dur={};desc="Resolve Impl: Local: Scheduling Delay""#, (next-start).as_server_timing_duration()));
            core::mem::swap(&mut start, &mut next);

            Ok(StoredRepoPath::Mmap{
                metadata,
                data: map,
                hash,
                timing,
            })
        }
        Err(err) => {
            handle_err!(err, path);
        }
    }
}

async fn serve_repository_stored_dir(path: &PathBuf, metadata: Metadata) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    match tokio::fs::read_dir(&path).await {
        Err(err) => {
            match err.kind() {
                ErrorKind::NotFound => Err(vec![GetRepoFileError::NotFound]),
                _ => {
                    tracing::warn!("Error reading directory: {err}");
                    Err(vec![GetRepoFileError::ReadDirectory])
                }
            }
        }
        Ok(mut v) => {
            let mut out = HashSet::new();
            loop {
                let entry = match v.next_entry().await {
                    Err(err) => {
                        tracing::warn!("Error reading directory entry: {err}");
                        return Err(vec![GetRepoFileError::ReadDirectoryEntry]);
                    }
                    Ok(None) => break,
                    Ok(Some(v)) => v,
                };
                let entry = match entry.file_name().into_string() {
                    Err(_) => {
                        tracing::warn!("Error: directory contains entries with non UTF-8 names");
                        return Err(vec![GetRepoFileError::ReadDirectoryEntryNonUTF8Name]);
                    }
                    Ok(v) => v,
                };
                out.insert(entry);
            }
            Ok(StoredRepoPath::DirListing{
                metadata: vec![metadata],
                entries: out,
            })
        }
    }
}