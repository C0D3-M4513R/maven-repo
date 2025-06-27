use std::borrow::Cow;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use rocket::http::{ContentType, Status};
use rocket::State;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
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

    let set = Arc::new(Mutex::new(HashSet::new()));
    match get_repo_file_impl((*client).clone(), repo.clone(), Arc::from(path), str_path.clone(), set).await {
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
    RecursionLoop,
    RecursionTooMany,
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
            Self::RecursionLoop => "Loop in Repository groups",
            Self::RecursionTooMany => "Repository upstream delegates to too many repos",
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

async fn get_repo_config(repo: &str) -> Result<Repository, GetRepoFileError> {
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
fn get_repo_file_impl(client: reqwest::Client, repo: String, path: Arc<Path>, str_path: Arc<str>, visited_repos: Arc<Mutex<HashSet<String>>>) -> Pin<Box<dyn Future<Output = Result<StoredRepoPath, Vec<GetRepoFileError>>> + Send>> {
    Box::pin(async move{
        check_already_visited(&repo, &visited_repos).await?;

        let config = get_repo_config(&repo).await.map_err(|v|vec![v])?;

        if config.upstreams.is_empty() {
            let path = Path::new(&repo).join(path);
             serve_repository_stored_path(path, true).await
        } else {
            let mut js = JoinSet::new();
            let mut local_upstream = Vec::new();
            let mut remote_upstream = Vec::new();

            for upstream in config.upstreams {
                match upstream {
                    Upstream::Local(upstream) => local_upstream.push(upstream),
                    Upstream::Remote(remote) => remote_upstream.push(remote),
                }
            }
            
            let mut errors = Vec::new();
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

            if config.stores_remote_upstream {
                let path = Path::new(&repo).join(&path);
                js.spawn(serve_repository_stored_path(path, true));
            }
            for upstream in local_upstream {
                js.spawn(get_repo_file_impl(client.clone(), upstream.path, path.clone(), str_path.clone(), visited_repos.clone()));
            }
            if let Some(v) = check_result(&mut js).await {
                return Ok(v);
            }

            for remote in remote_upstream {
                js.spawn(serve_remote_repository(client.clone(), remote, str_path.clone(), repo.clone(), path.clone(), config.stores_remote_upstream, visited_repos.clone()));
            }
            if let Some(v) = check_result(&mut js).await {
                return Ok(v);
            }

            Err(errors)
        }
    })
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
async fn check_already_visited(repo: &String, visited_repos: &Arc<Mutex<HashSet<String>>>) -> Result<(), Vec<GetRepoFileError>> {
    let mut visited_repos = visited_repos.lock().await;
    if !visited_repos.insert(repo.clone()) {
        return Err(vec![GetRepoFileError::RecursionLoop]);
    }
    if visited_repos.len() > 1024 {
        return Err(vec![GetRepoFileError::RecursionTooMany]);
    }
    Ok(())
}
async fn serve_remote_repository(client: reqwest::Client, remote: RemoteUpstream, str_path: Arc<str>, repo: String, path: Arc<Path>, stores_remote_upstream: bool, visited_repos: Arc<Mutex<HashSet<String>>>) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    check_already_visited(&remote.url, &visited_repos).await?;
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