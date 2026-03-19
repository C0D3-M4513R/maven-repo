use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;
use serde_derive::{Deserialize, Serialize};
use tokio::time::Instant;
use crate::auth::BasicAuthentication;
use crate::err::GetRepoFileError;
use crate::status::{Return};

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
        if let Some(headers) = self.cache_control_status_code.get(&ret.status.as_u16()) {
            for header in headers {
                let name = match actix_web::http::header::HeaderName::from_str(header.name.as_str()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                
                let value = match actix_web::http::header::HeaderValue::from_str(header.value.as_str()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                header_map.insert(name, value);
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
    pub path: Box<str>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Header{
    pub name: String,
    pub value: String,
}
impl Header {
    pub fn add_to_map(&self, header_map: &mut actix_web::http::header::HeaderMap) {
        let name = match actix_web::http::header::HeaderName::from_bytes(self.name.as_str().as_bytes()){
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("Cannot convert '{}' to a header-name: {err}", self.name);
                return;
            }
        };
        let value = match actix_web::http::header::HeaderValue::from_bytes(self.value.as_str().as_bytes()){
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("Cannot convert '{}' to a header-value: {err}", self.value);
                return;
            }
        };
        header_map.append(name, value);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteUpstream{
    pub url: Box<str>,
    pub timeout: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_fresh: Option<Duration>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Token{
    pub hash: Box<str>,
    pub paths: HashMap<Box<str>, PathAuthorization>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PathAuthorization{
    pub read: bool,
    pub put: bool,
    pub delete: bool,
}

impl Repository {
    pub fn check_auth(
        &self,
        method: actix_web::http::Method,
        #[cfg_attr(not(feature = "token-auth"), allow(unused_variables))]
        auth: Option<BasicAuthentication>,
        #[cfg_attr(not(feature = "token-auth"), allow(unused_variables))]
        path: &str
    ) -> Result<bool, Return> {
        let needs_auth = match method {
            actix_web::http::Method::GET => !self.publicly_readable.unwrap_or(true),
            _ => true,
        };

        #[cfg(not(feature = "token-auth"))]
        if needs_auth { Err(crate::UNAUTHORIZED()) } else { Ok(false) }
        #[cfg(feature = "token-auth")]
        if needs_auth {
            let auth = match auth {
                None => return Err(crate::UNAUTHORIZED()),
                Some(v) => v,
            };
            let token = match self.tokens.get(&auth.username) {
                Some(v) => v,
                None => return Err(crate::UNAUTHORIZED()),
            };
            //Todo: this won't work with subdirs
            let path = match token.paths.get(path) {
                None => return Err(crate::UNAUTHORIZED()),
                Some(v) => v,
            };
            match bcrypt::verify(&auth.password, &token.hash) {
                Ok(true) => {},
                Ok(false) => return Err(crate::UNAUTHORIZED()),
                Err(err) => {
                    tracing::error!("Failed to verify password '{}' against hash '{}': {err}", &auth.password, &token.hash);
                    return Err(Return{
                        status: actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                        content: crate::status::Content::Str("Error validating password"),
                        content_type: actix_web::http::header::ContentType::plaintext(),
                        header_map: Default::default(),
                    });
                }
            }
            if !match method {
                actix_web::http::Method::GET => path.read,
                actix_web::http::Method::PUT => path.put,
                actix_web::http::Method::DELETE => path.delete,
                _ => false,
            } {
                return Err(crate::FORBIDDEN());
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }
}


pub const OUT_VEC_STACKSIZE:usize = 32;
pub fn get_repo_look_locations(repo: &'static str, config: &'static Repository) -> (smallvec::SmallVec<[(&'static str, &'static Repository); OUT_VEC_STACKSIZE]>, Vec<GetRepoFileError>) {
    let mut start = Instant::now();
    let mut next;

    let mut errors = Vec::new();
    let mut to_visit = smallvec::SmallVec::<[(&str, &Repository); OUT_VEC_STACKSIZE]>::new();
    let mut out = smallvec::SmallVec::new();

    out.push((repo, config));

    let mut visited = HashSet::new();

    to_visit.push((repo, config));

    while let Some((repo, config)) = to_visit.pop() {
        for upstream in &config.upstreams{
            let upstream = match upstream {
                Upstream::Local(upstream) => upstream,
                Upstream::Remote(_) => continue,
            };
            if visited.insert(upstream.path.clone()) {
                match crate::REPOSITORIES.get(upstream.path.as_ref()) {
                    Some(repo) => {
                        out.push((&*upstream.path, repo));
                        to_visit.push((&*upstream.path, repo));
                    },
                    None => {
                        errors.push(GetRepoFileError::NotFound);
                    }
                }
            } else {
                tracing::info!("{repo}: Skipping duplicate local upstream: {}", &upstream.path)
            }
        };
    }
    next = Instant::now();
    tracing::info!("{repo}: collecting all configs took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    (out, errors)
}