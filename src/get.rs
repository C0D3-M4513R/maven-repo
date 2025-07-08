mod local;
mod remote;
mod interal_impl;
mod header;

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Component, PathBuf};
use base64::Engine;
use rocket::http::{ContentType, HeaderMap, Status};
use tokio::time::Instant;
use crate::auth::BasicAuthentication;
use crate::repository::get_repo_config;
use crate::status::{Content, Return};
use crate::err::GetRepoFileError;
use crate::RequestHeaders;
use crate::server_timings::AsServerTimingDuration;

use local::serve_repository_stored_path;
use remote::serve_remote_repository;
use header::header_check;
use interal_impl::get_repo_file_impl;

#[rocket::get("/<repo>/<path..>")]
pub async fn get_repo_file(repo: &str, path: PathBuf, auth: Option<Result<BasicAuthentication, Return>>, request_headers: RequestHeaders<'_>) -> Return {
    let mut timings = Vec::new();
    let mut start = Instant::now();
    let mut next;
    let mut header_map = HeaderMap::new();

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
        Err(mut err) => {
            err.header_map.get_or_insert_default().add_raw("Vary", "Authorization");
            return err
        },
        Ok(true) => {
            header_map.add_raw("Vary", "Authorization");
        },
        Ok(false) => {},
    }
    next = Instant::now();
    timings.push(format!(r#"verifyAuth;dur={};desc="Verify Authentication Information""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: auth check took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let resolve_impl = get_repo_file_impl(repo, path.as_path(), str_path, config.clone(), &mut timings).await;
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

    header_check(repo, &path, &config, str_path, &mut timings, map, &request_headers, hash, &metadata, header_map, &mut start, &mut next).await
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