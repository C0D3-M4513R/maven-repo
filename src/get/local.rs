use std::collections::HashMap;
use std::fs::FileType;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::Instant;
use crate::err::GetRepoFileError;
use crate::file_metadata::FileMetadata;
use crate::get::StoredRepoPath;
use crate::repository::Repository;
use crate::server_timings::AsServerTimingDuration;
use crate::timings::ServerTimings;

pub async fn serve_repository_stored_path(path: PathBuf, display_dir: bool, has_trailing_slash: bool, config: Arc<Repository>, str_path: Arc<str>) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;
    let mut errors = Vec::new();
    let mut timing = ServerTimings::new();

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
    if has_trailing_slash {
        if !display_dir {
            errors.push(GetRepoFileError::NotFound);
        } else {
            match tokio::join!(
                serve_repository_stored_dir(&path),
                tokio::fs::metadata(&path)
            ) {
                (Ok(entries), Ok(meta)) => return Ok(StoredRepoPath::DirListing {entries, metadata: vec![meta]}),
                (entry, meta) => {
                    if let Err(mut entry) = entry {
                        errors.append(&mut entry);
                    }
                    if let Err(meta) = meta {
                        tracing::warn!("Error opening metadata {}: {meta}", path.display());
                        errors.push(GetRepoFileError::OpenFile)
                    }
                },
            }
        }
        Err(errors)
    } else {
        next = Instant::now();
        timing.push_iter_nodelim([r#"resolveImplLocalFSMetadata;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Local: Query File Metadata""#]);
        core::mem::swap(&mut start, &mut next);


        let path = Arc::<std::path::Path>::from(path);
        let metadata = {
            let path = path.clone();
            tokio::task::spawn_blocking(move ||std::fs::metadata(path))
        };
        let task = {
            let path = path.clone();
            tokio::task::spawn_blocking(move ||{
                let mut timings = timing;
                let mut start = start;
                let mut next;

                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)?;

                next = Instant::now();
                timings.push_iter_nodelim([r#"resolveImplLocalOpenFile;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Local: Opening File""#]);
                core::mem::swap(&mut start, &mut next);

                #[cfg(feature = "locking")]
                {
                    file.lock_shared()?;

                    next = Instant::now();
                    timings.push_iter_nodelim([r#"resolveImplLocalSharedFileLock;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Local: Aquiring Shared File Lock""#]);
                    core::mem::swap(&mut start, &mut next);
                }

                let map = unsafe { memmap2::Mmap::map(&file) }?;
                map.advise(memmap2::Advice::Sequential)?;
                map.advise(memmap2::Advice::WillNeed)?;
                #[cfg(target_os = "linux")]
                {
                    map.advise(memmap2::Advice::PopulateRead)?;
                }
                next = Instant::now();
                timings.push_iter_nodelim([r#"resolveImplLocalMemMapFile;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Local: Memory Map file""#]);
                core::mem::swap(&mut start, &mut next);

                let hash = blake3::Hasher::default().update(&*map).finalize();
                next = Instant::now();
                timings.push_iter_nodelim([r#"resolveImplLocalETagFile;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Local: Calculate File ETag""#]);
                core::mem::swap(&mut start, &mut next);

                Ok::<_, std::io::Error>((map, file, hash, timings, start))
            })
        };
        let metadata = match metadata.await {
            Ok(Ok(v)) => v,
            Ok(Err(err)) => {
                tracing::warn!("Error opening metadata {}: {err}", path.display());
                errors.push(GetRepoFileError::OpenFile);
                return Err(errors);
            }
            Err(err) => {
                tracing::warn!("Panicked opening metadata {}: {err}", path.display());
                errors.push(GetRepoFileError::OpenFile);
                return Err(errors);
            }
        };

        if metadata.is_dir() {
            task.abort();
            return Ok(StoredRepoPath::IsADir);
        }

        let (mut data, file, hash, mut timing, mut start) = match task.await {
            Ok(Ok(v)) => v,
            Ok(Err(err)) => {
                handle_err!(err, path);
            }
            Err(err) => {
                tracing::error!("Panicked whilst opening file: {err}");
                errors.push(GetRepoFileError::OpenFile);
                return Err(errors);
            }
        };

        next = Instant::now();
        timing.push_iter_nodelim([r#"resolveImplLocalScheduleDelay;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Resolve Impl: Local: Scheduling Delay""#]);
        core::mem::swap(&mut start, &mut next);

        let mut file = tokio::fs::File::from_std(file);
        match FileMetadata::validate(&config, &str_path, &path, &mut data, &mut file, &metadata, &hash).await {
            Ok(_) => {},
            Err(err) => {
                tracing::error!("Failed to get File Metadata for {str_path}: {err:#?}");
            }
        }
        next = Instant::now();
        timing.push_iter_nodelim([r#"resolveImplLocalFileMetadataValidate;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#"esc="Resolve Impl: Local: Validate or Create File Metadata""#]);
        core::mem::swap(&mut start, &mut next);

        Ok(StoredRepoPath::Mmap{
            metadata,
            data,
            hash,
            timing,
        })
    }
}

async fn serve_repository_stored_dir(path: &PathBuf) -> Result<HashMap<String, FileType>, Vec<GetRepoFileError>> {
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
            let mut out = HashMap::new();
            loop {
                let entry = match v.next_entry().await {
                    Err(err) => {
                        tracing::warn!("Error reading directory entry: {err}");
                        return Err(vec![GetRepoFileError::ReadDirectoryEntry]);
                    }
                    Ok(None) => break,
                    Ok(Some(v)) => v,
                };
                let file_name = match entry.file_name().into_string() {
                    Err(_) => {
                        tracing::warn!("Error: directory contains entries with non UTF-8 names");
                        return Err(vec![GetRepoFileError::ReadDirectoryEntryNonUTF8Name]);
                    }
                    Ok(v) => v,
                };
                if file_name.starts_with(".") && file_name.ends_with(".json") {
                    continue;
                }
                let file_type = match entry.file_type().await {
                    Err(_) => {
                        tracing::warn!("Error: failed to get the file-type of the directory entry");
                        return Err(vec![GetRepoFileError::ReadDirectoryEntryFileType]);
                    }
                    Ok(v) => v,
                };
                out.insert(file_name, file_type);
            }
            Ok(out)
        }
    }
}