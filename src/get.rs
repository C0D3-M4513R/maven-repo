use std::borrow::Cow;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use rocket::http::{ContentType, Status};
use rocket::State;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use tokio::time::Instant;
use crate::repository::{RemoteUpstream, Repository, Upstream};
use crate::status::Return;

#[rocket::get("/<repo>/<path..>")]
pub async fn get_repo_file(client: &State<reqwest::Client>, repo: String, path: PathBuf) -> Return {
    if path.iter().any(|v|v == "..") {
        return Return::Content{
            status: Status::BadRequest,
            content: Cow::Borrowed("`..` is not allowed in the path".as_bytes()),
            content_type: ContentType::Text,
            header_map: Default::default(),
        }
    }
    let str_path = match path.to_str() {
        None => return Return::Content{
            status: Status::InternalServerError,
            content: GetRepoFileError::InvalidUTF8.get_err().as_bytes().into(),
            content_type: ContentType::Text,
            header_map: Default::default(),
        },
        Some(v) => Arc::<str>::from(v),
    };

    match get_repo_file_impl((*client).clone(), repo.clone(), Arc::from(path), str_path.clone()).await {
        Ok(v) => v.to_return(str_path.as_ref(), &repo),
        Err(v) => {
            let mut out = String::new();
            if v.is_empty() {
                out.push_str("No error reported, despite being in an error state.");
                out.push('\n');
            }
            for err in v {
                out.push_str(err.get_err());
                out.push('\n');
            }
            Return::Content{
                status: Status::BadRequest,
                content: out.into_bytes().into(),
                content_type: ContentType::Text,
                header_map: Default::default(),
            }
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum GetRepoFileError{
    ReadConfig,
    ParseConfig,
    NotFound,
    ReadFile,
    ReadDirectory,
    ReadDirectoryEntry,
    ReadDirectoryEntryNonUTF8Name,
    Panicked,
    InvalidUTF8,
    UpstreamError,
}
impl GetRepoFileError {
    const fn get_err(self) -> &'static str {
        match self {
            Self::ReadConfig => "Error reading repo config",
            Self::ParseConfig => "Error parsing repo config",
            Self::NotFound => "File or Directory could not be found",
            Self::ReadFile => "Error whilst reading file",
            Self::ReadDirectory => "Error whist reading directory",
            Self::ReadDirectoryEntry => "Error whist reading directory entries",
            Self::ReadDirectoryEntryNonUTF8Name => "Error: directory contains entries with non UTF-8 names",
            Self::Panicked => "Error: implementation panicked",
            Self::InvalidUTF8 => "Error: request path included invalid utf-8 characters",
            Self::UpstreamError => "Upstream did not deliver a file",
        }
    }
}

async fn get_repo_config(repo: Cow<'_, str>) -> Result<Repository, GetRepoFileError> {
    let config = match tokio::fs::read_to_string(format!(".{repo}.json")).await {
        Err(err) => {
            log::error!("Error reading repo config: {err}");
            return Err(GetRepoFileError::ReadConfig);
        }
        Ok(v) => v,
    };
    let config:Repository = match serde_json::from_str(&config) {
        Err(err) => {
            log::error!("Error parsing repo config: {err}");
            return Err(GetRepoFileError::ParseConfig);
        }
        Ok(v) => v,
    };
    Ok(config)
}
async fn get_repo_look_locations(repo: &str) -> (Vec<(String, Repository)>, Vec<GetRepoFileError>) {
    let mut start = Instant::now();
    let mut next;
    next = Instant::now();
    core::mem::swap(&mut start, &mut next);
    log::info!("{repo}: check_already_visited took {}ns", (next-start).as_nanos());

    let mut errors = Vec::new();
    let mut out = Vec::new();

    let config = match get_repo_config(Cow::Borrowed(repo)).await {
        Ok(v) => v,
        Err(e) => return (Vec::new(), vec![e]),
    };
    out.push((repo.to_string(), config.clone()));
    next = Instant::now();
    core::mem::swap(&mut start, &mut next);
    log::info!("{repo}: get_repo_config took {}ns", (next-start).as_nanos());

    let mut js = JoinSet::new();
    let mut visited = HashSet::new();

    fn check_repo(js: &mut JoinSet<Result<(String, Repository), GetRepoFileError>>, repo: &str, config: Repository, visited: &mut HashSet<String>) {
        for upstream in config.upstreams{
            let upstream = match upstream {
                Upstream::Local(upstream) => upstream,
                Upstream::Remote(_) => continue,
            };
            if visited.insert(upstream.path.clone()) {
                js.spawn(async move {
                    let path = upstream.path;
                    let out = get_repo_config(Cow::Borrowed(path.as_str())).await?;
                    Ok((path, out))
                });
            } else {
                log::info!("{repo}: Skipping duplicate local upstream: {}", &upstream.path)
            }
        };
    }
    check_repo(&mut js, &repo, config, &mut visited);
    while let Some(task) = js.join_next().await {
        match task {
            Ok(Ok((path, config))) => {
                check_repo(&mut js, &repo, config.clone(), &mut visited);
                out.push((path, config));
            },
            Ok(Err(v)) => {
                errors.push(v);
            },
            Err(err) => {
                log::error!("{repo}: Panicked whilst trying to resolve repo config: {err}");
                errors.push(GetRepoFileError::Panicked);
            }
        }
    }

    (out, errors)
}
async fn get_repo_file_impl(client: reqwest::Client, repo: String, path: Arc<Path>, str_path: Arc<str>) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;

    let (configs, mut errors) = get_repo_look_locations(repo.as_str()).await;
    next = Instant::now();
    core::mem::swap(&mut start, &mut next);
    log::info!("{repo}: get_repo_look_locations took {}ns", (next-start).as_nanos());

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
                    log::error!("Panicked whilst trying to resolve repo file: {err}");
                    errors.push(GetRepoFileError::Panicked);
                }
            }
        };
        out
    };

    for (repo, _) in &configs {
        js.spawn(serve_repository_stored_path(Path::new(&repo).join(&path), true));
    }

    if let Some(v) = check_result(&mut js).await {
        next = Instant::now();
        core::mem::swap(&mut start, &mut next);
        log::info!("{repo}: final resolve took took {}ns (skipped remotes, as the information could be locally sourced)", (next-start).as_nanos());
        return Ok(v);
    }

    next = Instant::now();
    core::mem::swap(&mut start, &mut next);
    log::info!("{repo}: local resolve took took {}ns", (next-start).as_nanos());

    let mut upstreams = HashSet::new();
    for (repo, config) in configs {
        for upstream in config.upstreams {
            let upstream = match upstream {
                Upstream::Local(_) => continue,
                Upstream::Remote(v) => v,
            };
            if upstreams.insert(upstream.url.clone()) {
                js.spawn(serve_remote_repository(client.clone(), upstream, str_path.clone(), repo.clone(), path.clone(), config.stores_remote_upstream));
            }
        }
    }
    if let Some(v) = check_result(&mut js).await {
        next = Instant::now();
        core::mem::swap(&mut start, &mut next);
        log::info!("{repo}: final resolve took took {}ns (contacted remotes)", (next-start).as_nanos());
        return Ok(v);
    }

    Err(errors)
}
enum StoredRepoPath{
    File(Vec<u8>),
    UpstreamDirListing(String),
    DirListing(HashSet<String>),
}
impl StoredRepoPath {
    pub fn to_return(self, path: &str, repo:&str) -> Return {
        match self {
            Self::UpstreamDirListing(v) => Return::Content{
                status: Status::Ok,
                content: v.into_bytes().into(),
                content_type: ContentType::HTML,
                header_map: Default::default(),
            },
            Self::DirListing(v) => {
                let mut out = r#"<!DOCTYPE HTML><html><head><meta charset="utf-8"><meta name="color-scheme" content="dark light"></head><body><ul>"#.to_string();
                for entry in v {
                    out.push_str(&format!(r#"<li><a href="/{repo}/{path}/{entry}">{entry}</a></li>"#));
                }
                out.push_str("</ul></body></html>");
                Return::Content{
                    status: Status::Ok,
                    content: out.into_bytes().into(),
                    content_type: ContentType::HTML,
                    header_map: Default::default(),
                }
            },
            Self::File(v) => Return::Content{
                status: Status::Ok,
                content: v.into(),
                content_type: ContentType::Binary,
                header_map: Default::default(),
            }
        }
    }
}
async fn serve_remote_repository(client: reqwest::Client, remote: RemoteUpstream, str_path: Arc<str>, repo: String, path: Arc<Path>, stores_remote_upstream: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let url = remote.url;
    let response = match client
        .get(format!("{url}/{str_path}"))
        .timeout(remote.timeout)
        .send()
        .await {
        Err(err) => {
            log::warn!("Error contacting Upstream repo: {err}");
            return Err(vec![GetRepoFileError::UpstreamError])
        },
        Ok(v) => v,
    };
    if response.status() != reqwest::StatusCode::OK {
        log::warn!("Error contacting Upstream repo didn't respond with Ok: {response:?}");
        return Err(vec![GetRepoFileError::UpstreamError]);
    }
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).cloned();
    let body = match response.bytes().await {
        Err(err) => {
            log::warn!("Error contacting Upstream repo: {err}");
            return Err(vec![GetRepoFileError::UpstreamError]);
        }
        Ok(v) => v,
    };

    match content_type {
        Some(v) if v.as_bytes().starts_with(b"text/html") => {
            let body = match core::str::from_utf8(&*body) {
                Ok(v) => v,
                Err(err) => {
                    log::warn!("Repo sent html, but response isn't utf-8: {err}");
                    return Err(vec![GetRepoFileError::UpstreamError]);
                }
            };
            Ok(StoredRepoPath::UpstreamDirListing(body.to_string()))
        }
        _ => {
            if stores_remote_upstream {
                let path = Path::new(&repo).join(path);
                if let Some(parent) = path.parent() {
                    if let Err(err) = tokio::fs::create_dir_all(parent).await {
                        log::error!("Error creating directories to {}: {err}", path.display());
                    }
                }
                match tokio::fs::File::create_new(&path)
                        .await
                {
                    Ok(mut v) => {
                        match v.write_all(&*body).await {
                            Ok(()) => {},
                            Err(err) => {
                                log::error!("Error writing to File {}: {err}", path.display());
                                match tokio::fs::remove_file(&path).await {
                                    Ok(()) => {},
                                    Err(err) => {
                                        log::error!("Error deleting File after error writing to File {}: {err}", path.display());
                                    }
                                }
                            }
                        }
                    },
                    Err(v) => {
                        log::error!("Error Creating File: {v}");
                    }
                }
            }
            Ok(StoredRepoPath::File(Vec::from(&*body)))
        }
    }
}
async fn serve_repository_stored_path(path: PathBuf, display_dir: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    match tokio::fs::read(&path).await {
        Ok(v) => Ok(StoredRepoPath::File(v)),
        Err(err) => {
            match err.kind(){
                ErrorKind::IsADirectory if display_dir => {
                    serve_repository_stored_dir(&path).await
                }
                ErrorKind::NotFound => Err(vec![GetRepoFileError::NotFound]),
                _ => {
                    log::warn!("Error reading file: {err}");
                    Err(vec![GetRepoFileError::ReadFile])
                },
            }
        }
    }
}

async fn serve_repository_stored_dir(path: &PathBuf) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    match tokio::fs::read_dir(&path).await {
        Err(err) => {
            match err.kind() {
                ErrorKind::NotFound => Err(vec![GetRepoFileError::NotFound]),
                _ => {
                    log::warn!("Error reading directory: {err}");
                    Err(vec![GetRepoFileError::ReadDirectory])
                }
            }
        }
        Ok(mut v) => {
            let mut out = HashSet::new();
            loop {
                let entry = match v.next_entry().await {
                    Err(err) => {
                        log::warn!("Error reading directory entry: {err}");
                        return Err(vec![GetRepoFileError::ReadDirectoryEntry]);
                    }
                    Ok(None) => break,
                    Ok(Some(v)) => v,
                };
                let entry = match entry.file_name().into_string() {
                    Err(_) => {
                        log::warn!("Error: directory contains entries with non UTF-8 names");
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