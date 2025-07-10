use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;
use rocket::http::{ContentType, Status};
use serde_derive::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::task::JoinSet;
use tokio::time::Instant;
use crate::auth::BasicAuthentication;
use crate::err::GetRepoFileError;
use crate::status::{Content, Return};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repository{
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stores_remote_upstream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publicly_readable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_directory_listings: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infer_content_type_on_file_extension: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_fresh: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    #[serde(alias="cache_control", default, skip_serializing_if = "Vec::is_empty")]
    pub cache_control_file: Vec<Header>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_control_metadata: Vec<Header>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_control_dir_listings: Vec<Header>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub cache_control_status_code: HashMap<u16, Vec<Header>>,
    #[serde(default)]
    pub upstreams: Vec<Upstream>,
    #[serde(default)]
    pub tokens: HashMap<String, Token>
}
impl Default for Repository{
    fn default() -> Self {
        Self{
            stores_remote_upstream: None,
            publicly_readable: None,
            hide_directory_listings: None,
            infer_content_type_on_file_extension: None,
            time_fresh: None,
            max_file_size: None,
            cache_control_file: Vec::new(),
            cache_control_metadata: Vec::new(),
            cache_control_dir_listings: Vec::new(),
            cache_control_status_code: Default::default(),
            upstreams: Vec::new(),
            tokens: Default::default(),
        }
    }
}
impl Repository {
    pub fn merge(&mut self, other: &Repository) {
        self.stores_remote_upstream = self.stores_remote_upstream.or(other.stores_remote_upstream);
        self.publicly_readable = self.publicly_readable.or(other.publicly_readable);
        self.hide_directory_listings = self.hide_directory_listings.or(other.hide_directory_listings);
        self.infer_content_type_on_file_extension = self.infer_content_type_on_file_extension.or(other.infer_content_type_on_file_extension);
        self.max_file_size = self.max_file_size.or(other.max_file_size);
        self.cache_control_file.extend(other.cache_control_file.clone());
        self.cache_control_metadata.extend(other.cache_control_metadata.clone());
        self.cache_control_dir_listings.extend(other.cache_control_dir_listings.clone());
        self.cache_control_status_code.extend(other.cache_control_status_code.clone());
        self.tokens.extend(other.tokens.clone());
    }
    pub fn apply_cache_control(&self, ret: &mut Return) {
        let header_map = ret.header_map.get_or_insert_default();
        if let Some(headers) = self.cache_control_status_code.get(&ret.status.code) {
            for header in headers {
                header_map.add_raw(header.name.clone(), header.value.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Upstream{
    Local(LocalUpstream),
    Remote(RemoteUpstream),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalUpstream{
    pub path: String, 
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Header{
    pub name: String,
    pub value: String,
}
impl From<Header> for rocket::http::Header<'static> {
    fn from(value: Header) -> Self {
        Self::new(value.name, value.value)
    }
}
impl From<rocket::http::Header<'static>> for Header {
    fn from(value: rocket::http::Header<'static>) -> Self {
        Self{
            name: value.name.into_string(),
            value: value.value.into_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteUpstream{
    pub url: String, 
    pub timeout: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_fresh: Option<Duration>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Token{
    pub hash: String,
    pub paths: HashMap<String, PathAuthorization>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PathAuthorization{
    pub read: bool,
    pub put: bool,
    pub delete: bool,
}

impl Repository {
    pub fn check_auth(&self, method: rocket::http::Method, auth: Option<BasicAuthentication>, path: &str) -> Result<bool, Return> {
        let needs_auth = match method {
            rocket::http::Method::Get => !self.publicly_readable.unwrap_or(true),
            _ => true,
        };
        if needs_auth {
            let auth = match auth {
                None => return Err(crate::UNAUTHORIZED),
                Some(v) => v,
            };
            let token = match self.tokens.get(&auth.username) {
                Some(v) => v,
                None => return Err(crate::UNAUTHORIZED),
            };
            //Todo: this won't work with subdirs
            let path = match token.paths.get(path) {
                None => return Err(crate::UNAUTHORIZED),
                Some(v) => v,
            };
            match bcrypt::verify(&auth.password, &token.hash) {
                Ok(true) => {},
                Ok(false) => return Err(crate::UNAUTHORIZED),
                Err(err) => {
                    tracing::error!("Failed to verify password '{}' against hash '{}': {err}", &auth.password, &token.hash);
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Error validating password"),
                        content_type: ContentType::Text,
                        header_map: Default::default(),
                    });
                }
            }
            if !match method {
                rocket::http::Method::Get => path.read,
                rocket::http::Method::Put => path.put,
                rocket::http::Method::Delete => path.delete,
                _ => false,
            } {
                return Err(crate::FORBIDDEN);
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }
}


pub async fn get_repo_config(repo: Cow<'_, str>) -> Result<Arc<Repository>, GetRepoFileError> {
    match crate::REPOSITORIES.read().await.get(repo.as_ref()) {
        Some((_, v)) => {
            tracing::info!("Using cached repo config");
            return Ok(v.clone())
        },
        None => {},
    }
    let main_config = match crate::private::get_main_config().await{
        Ok(v) => v,
        Err(err) => {
            tracing::error!("Error getting main repo config file: {err}");
            return Err(GetRepoFileError::MainConfigError);
        }
    };
    tracing::info!("Getting repo config");
    let mut file = match tokio::fs::File::open(format!(".{repo}.json")).await {
        Err(err) => {
            return match err.kind() {
                ErrorKind::NotFound => Err(GetRepoFileError::NotFound),
                err => {
                    tracing::error!("Error opening repo config file: {err}");
                    Err(GetRepoFileError::OpenConfig)
                },
            }
        }
        Ok(v) => v,
    };
    let mut config = String::new();
    match file.read_to_string(&mut config).await {
        Err(err) => {
            return match err.kind() {
                ErrorKind::NotFound => Err(GetRepoFileError::NotFound),
                err => {
                    tracing::error!("Error reading repo config file: {err}");
                    Err(GetRepoFileError::ReadConfig)
                },
            };
        }
        Ok(v) => v,
    };
    let config = config;
    let mut config:Repository = match serde_json::from_str(&config) {
        Err(err) => {
            tracing::error!("Error parsing repo config: {err}");
            return Err(GetRepoFileError::ParseConfig);
        }
        Ok(v) => v,
    };
    config.merge(&main_config);
    let config = Arc::new(config);
    match crate::REPOSITORIES.write().await.insert(repo.clone().into_owned(), (file, config.clone())) {
        None => {},
        Some(_) => {
            tracing::info!("A cached config already exists for {repo}.");
        }
    }
    Ok(config)
}
pub async fn get_repo_look_locations(repo: &str, config: &Arc<Repository>) -> (Vec<(String, Arc<Repository>)>, Vec<GetRepoFileError>) {
    let mut start = Instant::now();
    let mut next;

    let mut errors = Vec::new();
    let mut out = Vec::new();

    out.push((repo.to_owned(), config.clone()));
    next = Instant::now();
    tracing::info!("{repo}: get_repo_config took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let mut js = JoinSet::new();
    let mut visited = HashSet::new();

    async fn check_repo(
        js: &mut JoinSet<Result<(String, Arc<Repository>), GetRepoFileError>>,
        repo: &str,
        config: Arc<Repository>,
        visited: &mut HashSet<String>,
        out: &mut Vec<(String, Arc<Repository>)>
    ) {
        let mut configs = vec![(repo.to_owned(), config)];
        let repository_cache = crate::REPOSITORIES.read().await;
        while let Some((repo, config)) = configs.pop() {
            for upstream in &config.upstreams{
                let upstream = match upstream {
                    Upstream::Local(upstream) => upstream,
                    Upstream::Remote(_) => continue,
                };
                if visited.insert(upstream.path.clone()) {
                    match repository_cache.get(&upstream.path) {
                        Some((_, repo)) => {
                            out.push((upstream.path.clone(), repo.clone()));
                            configs.push((upstream.path.clone(), repo.clone()));
                        },
                        None => {
                            let path = upstream.path.clone();
                            js.spawn(async move {
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
    check_repo(&mut js, &repo, config.clone(), &mut visited, &mut out).await;
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