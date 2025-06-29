use std::borrow::Cow;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use reqwest::StatusCode;
use rocket::http::{ContentType, Status};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use tokio::time::Instant;
use crate::repository::{RemoteUpstream, Repository, Upstream};
use crate::status::{Content, Return};

#[rocket::get("/<repo>/<path..>")]
pub async fn get_repo_file(repo: String, path: PathBuf) -> Return {
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
        Some(v) => Arc::<str>::from(v),
    };

    match get_repo_file_impl(repo.clone(), Arc::from(path), str_path.clone()).await {
        Ok(v) => v.to_return(str_path.as_ref(), &repo),
        Err(v) => {
            let mut out = String::new();
            if v.is_empty() {
                out.push_str("No error reported, despite being in an error state.");
                out.push('\n');
            }
            let mut can_404 = true;
            for err in v {
                can_404 &= err.can_404();
                out.push_str(err.get_err().as_ref());
                out.push('\n');
            }
            Return{
                status: if can_404 { Status::NotFound } else { Status::InternalServerError },
                content: Content::String(out),
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
    UpstreamRequestError,
    UpstreamBodyReadError,
    UpstreamStatus(StatusCode),
    FileCreateFailed,
    FileWriteFailed,
    FileContainsNoDot,
}
impl GetRepoFileError {
    fn can_404(&self) -> bool {
        match self {
            Self::NotFound => true,
            Self::UpstreamStatus(code) if code.is_client_error() => true,
            _ => false,
        }
    }
    fn get_err_content(self) -> Content {
        match self.get_err(){
            Cow::Borrowed(v) => Content::Str(v),
            Cow::Owned(v) => Content::String(v),
        }
    }
    fn get_err(self) -> Cow<'static,str> {
        match self {
            Self::ReadConfig => "Error reading repo config".into(),
            Self::ParseConfig => "Error parsing repo config".into(),
            Self::NotFound => "File or Directory could not be found".into(),
            Self::ReadFile => "Error whilst reading file".into(),
            Self::ReadDirectory => "Error whist reading directory".into(),
            Self::ReadDirectoryEntry => "Error whist reading directory entries".into(),
            Self::ReadDirectoryEntryNonUTF8Name => "Error: directory contains entries with non UTF-8 names".into(),
            Self::Panicked => "Error: implementation panicked".into(),
            Self::InvalidUTF8 => "Error: request path included invalid utf-8 characters".into(),
            Self::UpstreamRequestError => "Error: Failed to send a request to the Upstream".into(),
            Self::UpstreamBodyReadError => "Error: Failed to read the response of the Upstream".into(),
            Self::FileCreateFailed => "Error: Failed to create a file to write the upstream's response into".into(),
            Self::FileWriteFailed => "Error: Failed to write to a local file to contain the upstream's response".into(),
            Self::UpstreamStatus(status) => format!("Upstream repo responded with a non 200 status code: {status}").into(),
            Self::FileContainsNoDot => "Error: Refusing to contact upstream about files, which don't contain a '.' in them".into(),
        }
    }
}

async fn get_repo_config(repo: Cow<'_, str>) -> Result<Repository, GetRepoFileError> {
    match crate::REPOSITORIES.read_async(repo.as_ref(), |_, value|value.clone()).await {
        Some(v) => {
            tracing::info!("Using cached repo config");
            return Ok(v)
        },
        None => {},
    }
    tracing::info!("Getting repo config");
    let config = match tokio::fs::read_to_string(format!(".{repo}.json")).await {
        Err(err) => {
            tracing::error!("Error reading repo config: {err}");
            return Err(GetRepoFileError::ReadConfig);
        }
        Ok(v) => v,
    };
    let config:Repository = match serde_json::from_str(&config) {
        Err(err) => {
            tracing::error!("Error parsing repo config: {err}");
            return Err(GetRepoFileError::ParseConfig);
        }
        Ok(v) => v,
    };
    match crate::REPOSITORIES.insert_async(repo.into_owned(), config.clone()).await {
        Ok(()) => {},
        Err((repo, _)) => {
            tracing::info!("A cached config already exists for {repo}.");
        }
    }
    Ok(config)
}
async fn get_repo_look_locations(repo: &str) -> (Vec<(String, Repository)>, Vec<GetRepoFileError>) {
    let mut start = Instant::now();
    let mut next;

    let mut errors = Vec::new();
    let mut out = Vec::new();

    let config = match get_repo_config(Cow::Borrowed(repo)).await {
        Ok(v) => v,
        Err(e) => return (Vec::new(), vec![e]),
    };
    out.push((repo.to_string(), config.clone()));
    next = Instant::now();
    tracing::info!("{repo}: get_repo_config took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let mut js = JoinSet::new();
    let mut visited = HashSet::new();

    async fn check_repo(js: &mut JoinSet<Result<(String, Repository), GetRepoFileError>>, repo: &str, config: Repository, visited: &mut HashSet<String>, out: &mut Vec<(String, Repository)>) {
        let mut configs = vec![(repo.to_string(), config)];
        while let Some((repo, config)) = configs.pop() {
            for upstream in config.upstreams{
                let upstream = match upstream {
                    Upstream::Local(upstream) => upstream,
                    Upstream::Remote(_) => continue,
                };
                if visited.insert(upstream.path.clone()) {
                    match crate::REPOSITORIES.read_async(&upstream.path, |_, value|value.clone()).await {
                        Some(repo) => {
                            out.push((upstream.path.clone(), repo.clone()));
                            configs.push((upstream.path.clone(), repo));
                        },
                        None => {
                            js.spawn(async move {
                                let path = upstream.path;
                                let out = get_repo_config(Cow::Borrowed(path.as_str())).await?;
                                Ok((path, out))
                            });
                        }
                    }
                } else {
                    tracing::info!("{repo}: Skipping duplicate local upstream: {}", &upstream.path)
                }
            };
        }
    }
    check_repo(&mut js, &repo, config, &mut visited, &mut out).await;
    while let Some(task) = js.join_next().await {
        match task {
            Ok(Ok((path, config))) => {
                check_repo(&mut js, &repo, config.clone(), &mut visited, &mut out).await;
                out.push((path, config));
            },
            Ok(Err(v)) => {
                errors.push(v);
            },
            Err(err) => {
                tracing::error!("{repo}: Panicked whilst trying to resolve repo config: {err}");
                errors.push(GetRepoFileError::Panicked);
            }
        }
    }
    next = Instant::now();
    tracing::info!("{repo}: collecting all configs took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    (out, errors)
}
async fn get_repo_file_impl(repo: String, path: Arc<Path>, str_path: Arc<str>) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;

    let (configs, mut errors) = get_repo_look_locations(repo.as_str()).await;
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

    for (repo, _) in &configs {
        js.spawn(serve_repository_stored_path(Path::new(&repo).join(&path), true));
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

    let mut upstreams = HashSet::new();
    for (repo, config) in configs {
        for upstream in config.upstreams {
            let upstream = match upstream {
                Upstream::Local(_) => continue,
                Upstream::Remote(v) => v,
            };
            if upstreams.insert(upstream.url.clone()) {
                js.spawn(serve_remote_repository(upstream, str_path.clone(), repo.clone(), path.clone(), config.stores_remote_upstream));
            }
        }
    }
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
                let mut out = r#"<!DOCTYPE HTML><html><head><meta charset="utf-8"><meta name="color-scheme" content="dark light"></head><body><ul>"#.to_string();
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
async fn serve_remote_repository(remote: RemoteUpstream, str_path: Arc<str>, repo: String, path: Arc<Path>, stores_remote_upstream: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
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

        let mut file = match tokio::fs::File::create_new(&path)
                .await
        {
            Ok(v) => v,
            Err(v) => {
                tracing::error!("Error Creating File: {v}");
                return Err(vec![GetRepoFileError::FileCreateFailed]);
            }
        };
        let mut response = response;
        loop {
            let body = match response.chunk().await {
                Err(err) => {
                    tracing::warn!("Error contacting Upstream repo: {err}");
                    return Err(vec![GetRepoFileError::UpstreamBodyReadError]);
                }
                Ok(Some(v)) => v,
                Ok(None) => break,
            };

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
        Ok(StoredRepoPath::File(file))
    } else {
        Ok(StoredRepoPath::Upstream(response))
    }
}
async fn serve_repository_stored_path(path: PathBuf, display_dir: bool) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    match tokio::fs::File::open(&path).await {
        Ok(v) => Ok(StoredRepoPath::File(v)),
        Err(err) => {
            match err.kind(){
                ErrorKind::IsADirectory if display_dir => {
                    serve_repository_stored_dir(&path).await
                }
                ErrorKind::NotFound => Err(vec![GetRepoFileError::NotFound]),
                _ => {
                    tracing::warn!("Error reading file: {err}");
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