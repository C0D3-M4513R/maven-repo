use std::borrow::Cow;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use reqwest::StatusCode;
use rocket::http::{ContentType, Status};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use tokio::time::Instant;
use crate::auth::BasicAuthentication;
use crate::repository::{get_repo_config, get_repo_look_locations, RemoteUpstream, Repository, Upstream};
use crate::status::{Content, Return};
use crate::err::GetRepoFileError;

#[rocket::get("/<repo>/<path..>")]
pub async fn get_repo_file(repo: &str, path: PathBuf, auth: Option<Result<BasicAuthentication, Return>>) -> Return {
    let auth = match auth {
        Some(Err(err)) => return err,
        Some(Ok(v)) => Some(v),
        None => None,
    };
    if path.iter().any(|v|v == "..") {
        return Return{
            status: Status::BadRequest,
            content: Content::Str("`..` is not allowed in the path"),
            content_type: ContentType::Text,
            header_map: Default::default(),
        }
    }
    let str_path = match path.to_str() {
        None => return Return{
            status: Status::InternalServerError,
            content: GetRepoFileError::InvalidUTF8.get_err_content(),
            content_type: ContentType::Text,
            header_map: Default::default(),
        },
        Some(v) => v,
    };
    let str_path = str_path.strip_prefix("/").unwrap_or(str_path);
    let str_path = str_path.strip_suffix("/").unwrap_or(str_path);

    let config = match get_repo_config(Cow::Borrowed(repo)).await {
        Ok(v) => v,
        Err(e) => return Return{
            status: e.get_status_code(),
            content: e.get_err_content(),
            content_type: ContentType::Text,
            header_map: Default::default(),
        }
    };

    match config.check_auth(rocket::http::Method::Get, auth, str_path) {
        Err(err) => return err,
        Ok(_) => {},
    }

    match get_repo_file_impl(repo, path.as_path(), str_path, config).await {
        Ok(v) => v.to_return(str_path.as_ref(), &repo),
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
            Return{
                status: status_code.map(|codes|codes.into_iter().next()).unwrap_or(None).unwrap_or(Status::InternalServerError),
                content: Content::String(out),
                content_type: ContentType::Text,
                header_map: Default::default(),
            }
        }
    }
}
async fn get_repo_file_impl(repo: &str, path: &Path, str_path: &str, config: Repository) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;

    let (configs, mut errors) = get_repo_look_locations(repo, &config).await;
    next = Instant::now();
    tracing::info!("{repo}: get_repo_look_locations took {}µs", (next-start).as_micros());
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
        tracing::info!("{repo}: final resolve took took {}µs (skipped remotes, as the information could be locally sourced)", (next-start).as_micros());
        core::mem::swap(&mut start, &mut next);
        return Ok(v);
    }

    next = Instant::now();
    tracing::info!("{repo}: local resolve took took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);
    if !path.file_name().map_or(false, |v|v.to_str().map_or(false, |v|v.contains("."))) {
        errors.push(GetRepoFileError::FileContainsNoDot);
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
        tracing::info!("{repo}: final resolve took took {}µs (contacted remotes)", (next-start).as_micros());
        core::mem::swap(&mut start, &mut next);
        return Ok(v);
    }
    next = Instant::now();
    tracing::info!("{repo}: final resolve took took {}µs (contacted remotes)", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    Err(errors)
}
enum StoredRepoPath{
    File(tokio::fs::File),
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
                    header_map: Default::default(),
                }
            },
            Self::Upstream(v) => Return{
                status: Status::Ok,
                content: Content::Response(v),
                content_type: ContentType::Binary,
                header_map: Default::default(),
            },
            Self::File(v) => Return{
                status: Status::Ok,
                content: Content::File(v),
                content_type: ContentType::Binary,
                header_map: Default::default(),
            }
        }
    }
}
async fn serve_remote_repository(remote: RemoteUpstream, str_path: Arc<str>, repo: String, path: Arc<Path>, stores_remote_upstream: bool, limit: u64) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
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
            return Err(vec![GetRepoFileError::UpstreamStatus(code)]);
        }
    }

    if stores_remote_upstream {
        let path = Path::new(&repo).join(path);
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::error!("Error creating directories to {}: {err}", path.display());
            }
        }

        let file = match tokio::fs::File::create_new(&path)
                .await
        {
            Ok(v) => v,
            Err(v) => {
                tracing::error!("Error Creating File: {v}");
                return Err(vec![GetRepoFileError::FileCreateFailed]);
            }
        };
        let mut response = response;
        let mut file = tokio::io::BufWriter::new(file);
        let mut current_size = 0u64;
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
                return Err(vec![GetRepoFileError::UpstreamFileTooLarge { limit}])
            }

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
        Ok(StoredRepoPath::File(file.into_inner()))
    } else {
        Ok(StoredRepoPath::Upstream(response))
    }
}
async fn serve_repository_stored_path(path: PathBuf, display_dir: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut errors = Vec::new();
    macro_rules! delegate {
        () => {
            if !display_dir {
                errors.push(GetRepoFileError::NotFound);
            } else {
                match serve_repository_stored_dir(&path).await {
                    Ok(v) => return Ok(v),
                    Err(mut err) => errors.append(&mut err),
                }
            }
            return Err(errors);
        }
    }
    macro_rules! handle_err {
        ($err:ident) => {
            match $err.kind(){
                ErrorKind::IsADirectory if display_dir => {
                    delegate!();
                }
                ErrorKind::NotFound => errors.push(GetRepoFileError::NotFound),
                _ => {
                    tracing::warn!("Error reading file: {}", $err);
                    errors.push(GetRepoFileError::ReadFile)
                },
            }
            return Err(errors);
        };
    }
    let file = match tokio::fs::File::open(&path).await {
        Ok(v) => v,
        Err(err) => {
            handle_err!(err);
        }
    };
    match file.metadata().await {
        Ok(v) => {
            //We only check, if the metadata says, that this is a dir, because non-files might also be able to be read (e.g. unix sockets).
            //Theoretically everything inside the maven repos should be a directory or file.
            if v.is_dir() {
                delegate!();
            }
            
            Ok(StoredRepoPath::File(file))
        },
        Err(err) => {
            handle_err!(err);
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