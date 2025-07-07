use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use base64::Engine;
use reqwest::StatusCode;
use rocket::http::{ContentType, Status};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tokio::time::Instant;
use crate::auth::BasicAuthentication;
use crate::repository::{get_repo_config, get_repo_look_locations, RemoteUpstream, Repository, Upstream};
use crate::status::{Content, Return};
use crate::err::GetRepoFileError;
use crate::etag::ETagValidator;
use crate::RequestHeaders;
use crate::server_timings::AsServerTimingDuration;

#[rocket::get("/<repo>/<path..>")]
pub async fn get_repo_file(repo: &str, path: PathBuf, auth: Option<Result<BasicAuthentication, Return>>, request_headers: RequestHeaders<'_>) -> Return {
    let mut timings = Vec::new();
    let mut start = Instant::now();
    let mut next;

    let auth = match auth {
        Some(Err(err)) => return err,
        Some(Ok(v)) => {
            timings.push(format!(r#"parseAuthenticationHeader;dur={};desc="Parseing HTTP Authentication Header""#, v.duration.as_server_timing_duration()));
            Some(v)
        },
        None => None,
    };
    if path.components().any(|v|
        match v {
            Component::ParentDir => true,
            Component::RootDir => true,
            Component::Prefix(_) => true,
            _ => false,
        }
    ) {
        return GetRepoFileError::BadRequestPath.to_return();
    }
    if path.has_root() {
        return GetRepoFileError::BadRequestPath.to_return();
    }
    let str_path = match path.to_str() {
        None => return GetRepoFileError::InvalidUTF8.to_return(),
        Some(v) => v,
    };
    let str_path = str_path.strip_prefix("/").unwrap_or(str_path);
    let str_path = str_path.strip_suffix("/").unwrap_or(str_path);

    next = Instant::now();
    timings.push(format!(r#"verifyPathValid;dur={};desc="Verify Path to not contain any malicious items""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: path checks took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let config = match get_repo_config(Cow::Borrowed(repo)).await {
        Ok(v) => v,
        Err(e) => {
            let mut ret = e.to_return();
            ret.header_map.get_or_insert_default().add(rocket::http::Header::new("Server-Timing", timings.join(",")));
            return ret;
        },
    };
    next = Instant::now();
    timings.push(format!(r#"getMainConfig;dur={};desc="Get Repo Config""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: get_repo_config took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    match config.check_auth(rocket::http::Method::Get, auth, str_path) {
        Err(err) => return err,
        Ok(_) => {},
    }
    next = Instant::now();
    timings.push(format!(r#"verifyAuth;dur={};desc="Verify Authentication Information""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: auth check took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let resolve_impl = get_repo_file_impl(repo, path.as_path(), str_path, config, &mut timings).await;
    next = Instant::now();
    timings.push(format!(r#"resolveImpl;dur={};desc="Total Resolve Implementation""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: get_repo_file_impl check took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let (metadata, map, hash, mut timing) = match resolve_impl {
        Ok(StoredRepoPath::Mmap{metadata, data, hash, timing}) => (metadata, data, hash, timing),
        Ok(v) => {
            let mut ret = v.to_return(str_path.as_ref(), &repo);
            ret.header_map.get_or_insert_default().add(rocket::http::Header::new("Server-Timing", timings.join(",")));
            return ret;
        },
        Err(v) => {
            let mut out = String::new();
            if v.is_empty() {
                out.push_str("No error reported, despite being in an error state.");
                out.push('\n');
            }
            let mut status_code = None;
            for err in v {
                match &mut status_code {
                    None => status_code = Some(err.allowed_status_codes()),
                    Some(v) => status_code = Some(v.intersection(&err.allowed_status_codes()).copied().collect()),
                }
                out.push_str(err.get_err().as_ref());
                out.push('\n');
            }
            let mut ret = Return{
                status: status_code.map(|codes|codes.into_iter().min()).unwrap_or(None).unwrap_or(Status::InternalServerError),
                content: Content::String(out),
                content_type: ContentType::Text,
                header_map: Default::default(),
            };
            ret.header_map.get_or_insert_default().add(rocket::http::Header::new("Server-Timing", timings.join(",")));
            return ret;
        }
    };
    timings.append(&mut timing);

    let mut status = Status::Ok;
    let old_hash = tokio::sync::OnceCell::new();
    let mut content = None;
    let content_type = ContentType::Binary;
    let mut header_map = rocket::http::HeaderMap::new();
    header_map.add(rocket::http::Header::new("ETag", format!(r#""blake3-{}""#,base64::engine::general_purpose::STANDARD.encode(hash.as_bytes()))));
    if let Ok(modification_datetime) = metadata.modified() {
        let modification_datetime = chrono::DateTime::<chrono::Utc>::from(modification_datetime);
        header_map.add(rocket::http::Header::new("Last-Modified", modification_datetime.to_rfc2822()));
    }

    // Check for If-None-Match header
    let mut contains_none_match = false;
    for i in request_headers.0.get("If-None-Match") {
        contains_none_match = true;
        let v = match ETagValidator::parse(i) {
            Some(ETagValidator::Any) => {
                content = Some(Content::None);
                status = Status::NotModified;
                break
            },
            Some(ETagValidator::Tags(v)) => v,
            None => {
                next = Instant::now();
                timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
                tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
                core::mem::swap(&mut start, &mut next);

                header_map.remove_all();
                header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));

                return Return {
                    status: Status::BadRequest,
                    content: Content::String(format!("Bad If-None-Match header: {i}")),
                    content_type: ContentType::Text,
                    header_map: Some(header_map),
                }
            }
        };
        //This is strict checking, which is against spec, but we have 0 clue what the files actually contain
        // (and additionally this implementation disallows re-deploys via PUT [you'd have to DELETE and then PUT, once implemented])
        for tag in v {
            if tag.matches(&map, &hash, &old_hash, &mut timings).await.unwrap_or(false) {
                content = Some(Content::None);
                status = Status::NotModified;
                break
            }
        }
    }
    // Check for If-Match header
    if request_headers.0.contains("If-Match") {
        let mut any_match = false;
        for i in request_headers.0.get("If-Match") {
            let v = match ETagValidator::parse(i) {
                Some(ETagValidator::Any) => {
                    any_match = true;
                    break;
                },
                Some(ETagValidator::Tags(v)) => v,
                None => {
                    next = Instant::now();
                    timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
                    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
                    core::mem::swap(&mut start, &mut next);

                    header_map.remove_all();
                    header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));

                    return Return {
                        status: Status::BadRequest,
                        content: Content::String(format!("Bad If-Match header: {i}")),
                        content_type: ContentType::Text,
                        header_map: Some(header_map),
                    }
                }
            };
            //This is strict checking, which is against spec, but we have 0 clue what the files actually contain
            // (and additionally this implementation disallows re-deploys via PUT [you'd have to DELETE and then PUT, once implemented])
            for tag in v {
                if tag.matches(&map, &hash, &old_hash, &mut timings).await.unwrap_or(false) {
                    any_match = true;
                    break
                }
            }
        }
        if !any_match {
            header_map.remove_all();
            header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));

            return Return{
                status: Status::PreconditionFailed,
                content: Content::None,
                content_type: ContentType::Text,
                header_map: Some(header_map),
            }
        }

    }

    // Check for If-Unmodified-Since and If-Modified-Since
    if
        request_headers.0.contains("If-Unmodified-Since") ||
        //When used in combination with If-None-Match, it is ignored - https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/If-Modified-Since
        (!contains_none_match && request_headers.0.contains("If-Modified-Since"))
    {
        let modification_datetime = match metadata.modified() {
            Ok(v) => v,
            Err(err) => {
                tracing::error!("Failed to get modification time for {}: {err}", path.display());
                return GetRepoFileError::NotSupportedByOs.to_return()
            },
        };
        let modification_datetime = chrono::DateTime::<chrono::Utc>::from(modification_datetime);
        if !contains_none_match {
            for i in request_headers.0.get("If-Modified-Since") {
                match chrono::DateTime::parse_from_rfc2822(i) {
                    Ok(http_time) => {
                        if http_time > modification_datetime {
                            status = Status::NotModified;

                            next = Instant::now();
                            timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
                            tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
                            core::mem::swap(&mut start, &mut next);

                            header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));

                            return Return{
                                status,
                                content: Content::None,
                                content_type,
                                header_map: Some(header_map),
                            };
                        }
                    },
                    Err(err) => {
                        next = Instant::now();
                        timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
                        tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
                        core::mem::swap(&mut start, &mut next);

                        header_map.remove_all();
                        header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));

                        return Return{
                            status: Status::BadRequest,
                            content: Content::String(format!("Invalid value '{i}' in If-Modified-Since header: {err}")),
                            content_type: ContentType::Text,
                            header_map: Some(header_map),
                        }
                    }
                }
            }
        }
        for i in request_headers.0.get("If-Unmodified-Since") {
            match chrono::DateTime::parse_from_rfc2822(i) {
                Ok(http_time) => {
                    if http_time <= modification_datetime {
                        status = Status::PreconditionFailed;

                        next = Instant::now();
                        timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
                        tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
                        core::mem::swap(&mut start, &mut next);
                        header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));

                        return Return{
                            status,
                            content: Content::None,
                            content_type,
                            header_map: Some(header_map),
                        };
                    }
                },
                Err(err) => {
                    next = Instant::now();
                    timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
                    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
                    core::mem::swap(&mut start, &mut next);
                    header_map.remove_all();
                    header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));


                    return Return{
                        status: Status::BadRequest,
                        content: Content::String(format!("Invalid value '{i}' in If-Modified-Since header: {err}")),
                        content_type: ContentType::Text,
                        header_map: Some(header_map),
                    }
                }
            }
        }
    }
    next = Instant::now();
    timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));
    let content = content.unwrap_or(Content::Mmap(map));

    Return {
        status,
        content,
        content_type,
        header_map: Some(header_map)
    }
}
async fn get_repo_file_impl(repo: &str, path: &Path, str_path: &str, config: Repository, timings: &mut Vec<String>) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;

    let (configs, mut errors) = get_repo_look_locations(repo, &config).await;
    next = Instant::now();
    timings.push(format!(r#"resolveImplGetLocalRepoConfigs;dur={};desc="Resolve Implementation: Fetch all local upstream repo configs""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file_impl: {repo}: get_repo_look_locations took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let mut js = JoinSet::new();

    let mut check_result = async |js:&mut JoinSet<_>|{
        let mut out = None; 
        while let Some(task) = js.join_next().await {
            match task {
                Ok(Ok(v)) => {
                    out = match (out, v) {
                        (Some(StoredRepoPath::DirListing(mut out)), StoredRepoPath::DirListing(v)) => {
                            out.extend(v);
                            Some(StoredRepoPath::DirListing(out))
                        }
                        (Some(out), _) => {
                            js.abort_all();
                            return Some(out)
                        },
                        (None, v) => {
                            Some(v)
                        }
                    };
                },
                Ok(Err(mut v)) => {
                    errors.append(&mut v);
                },
                Err(err) => {
                    tracing::error!("Panicked whilst trying to resolve repo file: {err}");
                    errors.push(GetRepoFileError::Panicked);
                }
            }
        };
        out
    };

    for (repo, repo_config) in &configs {
        let display_dir = !config.hide_directory_listings.unwrap_or(repo_config.hide_directory_listings.unwrap_or(false));
        js.spawn(serve_repository_stored_path(Path::new(&repo).join(&path), display_dir));
    }

    if let Some(v) = check_result(&mut js).await {
        next = Instant::now();
        timings.push(format!(r#"resolveImplQueryLocalRepositoriesFound;dur={};desc="Resolve Implementation: Query local repositories for File (HIT)""#, (next-start).as_server_timing_duration()));
        tracing::info!("get_repo_file_impl: {repo}: final resolve took took {}µs (skipped remotes, as the information could be locally sourced)", (next-start).as_micros());
        core::mem::swap(&mut start, &mut next);
        return Ok(v);
    }

    next = Instant::now();
    timings.push(format!(r#"resolveImplQueryLocalRepositoriesMiss;dur={};desc="Resolve Implementation: Query local repositories for File (MISS)""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file_impl: {repo}: local resolve took took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);
    if path.components().any(|v|match v {
        Component::Normal(v) => {
            //valid utf-8 should have been checked earlier
            v.to_string_lossy().starts_with(".")
        },
        _ => false,
    }) {
        errors.push(GetRepoFileError::FileStartsWithDot);
        return Err(errors);
    }

    //Start requests to upstreams
    {
        let mut upstreams = HashSet::new();
        let remote_str_path = LazyLock::new(||Arc::<str>::from(str_path));
        let remote_path = LazyLock::new(||Arc::<Path>::from(path));
        for (repo, config) in configs {
            for upstream in config.upstreams {
                let upstream = match upstream {
                    Upstream::Local(_) => continue,
                    Upstream::Remote(v) => v,
                };
                if upstreams.insert(upstream.url.clone()) {
                    js.spawn(serve_remote_repository(upstream, remote_str_path.clone(), repo.clone(), remote_path.clone(), config.stores_remote_upstream, config.max_file_size.unwrap_or(crate::DEFAULT_MAX_FILE_SIZE)));
                }
            }
        }
    }

    //Collect requests from upstreams
    if let Some(v) = check_result(&mut js).await {
        next = Instant::now();
        timings.push(format!(r#"resolveImplQueryRemoteRepositoriesHit;dur={};desc="Resolve Implementation: Query remote repositories for File (HIT)""#, (next-start).as_server_timing_duration()));
        tracing::info!("get_repo_file_impl: {repo}: final resolve took took {}µs (contacted remotes)", (next-start).as_micros());
        core::mem::swap(&mut start, &mut next);
        return Ok(v);
    }
    next = Instant::now();
    timings.push(format!(r#"resolveImplQueryRemoteRepositoriesMiss;dur={};desc="Resolve Implementation: Query remote repositories for File (MISS)""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file_impl: {repo}: final resolve took took {}µs (contacted remotes)", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    Err(errors)
}
enum StoredRepoPath{
    Mmap{
        metadata: std::fs::Metadata,
        data: memmap2::Mmap,
        hash: blake3::Hash,
        timing: Vec<String>,
    },
    Upstream(reqwest::Response),
    DirListing(HashSet<String>),
}
impl StoredRepoPath {
    pub fn to_return(self, path: &str, repo:&str) -> Return {
        match self {
            Self::DirListing(v) => {
                let mut out = r#"<!DOCTYPE HTML><html><head><meta charset="utf-8"><meta name="color-scheme" content="dark light"></head><body><ul>"#.to_owned();
                let mut v = v.into_iter().collect::<Vec<_>>();
                v.sort();
                for entry in v {
                    out.push_str(&format!(r#"<li><a href="/{repo}/{path}/{entry}">{entry}</a></li>"#));
                }
                out.push_str("</ul></body></html>");
                Return{
                    status: Status::Ok,
                    content: Content::String(out),
                    content_type: ContentType::HTML,
                    header_map: None,
                }
            },
            Self::Upstream(v) => Return{
                status: Status::Ok,
                content: Content::Response(v),
                content_type: ContentType::Binary,
                header_map: None,
            },
            Self::Mmap{data, hash, ..} => {
                let mut header_map = rocket::http::HeaderMap::new();
                let hash = base64::engine::general_purpose::STANDARD.encode(hash.as_bytes());
                header_map.add(rocket::http::Header::new("ETag", hash));

                Return{
                    status: Status::Ok,
                    content: Content::Mmap(data),
                    content_type: ContentType::Binary,
                    header_map: Some(header_map),
                }
            }
        }
    }
}
async fn serve_remote_repository(remote: RemoteUpstream, str_path: Arc<str>, repo: String, path: Arc<Path>, stores_remote_upstream: bool, limit: u64) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;
    let mut timings = Vec::new();

    let url = remote.url;
    let response = match crate::CLIENT
        .get(format!("{url}/{str_path}"))
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
async fn serve_repository_stored_path(path: PathBuf, display_dir: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;
    let mut errors = Vec::new();
    let mut timing = Vec::new();

    macro_rules! delegate {
        ($path:ident) => {
            if !display_dir {
                errors.push(GetRepoFileError::NotFound);
            } else {
                match serve_repository_stored_dir(&$path).await {
                    Ok(v) => return Ok(v),
                    Err(mut err) => errors.append(&mut err),
                }
            }
            return Err(errors);
        }
    }
    macro_rules! handle_err {
        ($err:ident, $path:ident) => {
            match $err.kind(){
                ErrorKind::IsADirectory if display_dir => {
                    delegate!($path);
                }
                ErrorKind::NotFound => errors.push(GetRepoFileError::NotFound),
                _ => {
                    tracing::warn!("Error opening file: {}", $err);
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
                delegate!(path);
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

async fn serve_repository_stored_dir(path: &PathBuf) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
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
            Ok(StoredRepoPath::DirListing(out))
        }
    }
}