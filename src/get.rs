mod local;
mod remote;
mod interal_impl;
mod header;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::FileType;
use std::path::{Component, PathBuf};
use std::sync::Arc;
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
use interal_impl::resolve_impl;
use crate::timings::ServerTimings;

pub async fn get_repo_file(req: actix_web::HttpRequest, auth: Result<BasicAuthentication, Return>, request_headers: RequestHeaders) -> Return {
    let uri = req.full_url();
    let path = uri.path();
    let path = path.strip_prefix("/").unwrap_or(path);
    let path = PathBuf::from(path);
    let repo;
    let path = {
        let mut iter = path.components();
        loop {
            match iter.next() {
                Some(Component::Normal(v)) => {
                    match v.to_str() {
                        Some(v) => {
                            repo = Arc::from(v);
                            break;
                        },
                        None => return Return{
                            status: actix_web::http::StatusCode::BAD_REQUEST,
                            content: Content::Str("A part of the request was not valid UTF-8"),
                            content_type: actix_web::http::header::ContentType::plaintext(),
                            header_map: None,
                        },
                    }
                }
                Some(_) => continue,
                None => return Return{
                    status: actix_web::http::StatusCode::NOT_FOUND,
                    content: Content::None,
                    content_type: actix_web::http::header::ContentType::plaintext(),
                    header_map: None,
                },
            }
        }
        PathBuf::from_iter(iter)
    };
    let mut timings = ServerTimings::new();
    let mut start = Instant::now();
    let mut next;
    let mut header_map = actix_web::http::header::HeaderMap::new();
    {
        match path.to_str() {
            Some(v) => {
                let canonical = format!(
                    r#"<{}://{}/{repo}/{v}>; rel="canonical""#,
                    uri.scheme(),
                    uri.authority(),
                );
                match actix_web::http::header::HeaderValue::from_str(canonical.as_str()){
                    Ok(v) => { header_map.append(actix_web::http::header::LINK, v);}
                    Err(err) => {
                        tracing::warn!("Failed to create a header-value from '{canonical}': {err}");
                    }
                };
            },
            None => {
                tracing::warn!("Cannot convert path to str: {}", path.display());
            }
        }
    }

    let auth = match auth {
        Err(err) => {
            if err.status == actix_web::http::StatusCode::FORBIDDEN {
                None
            } else {
                return err
            }
        },
        Ok(v) => {
            timings.push_iter_nodelim([r#"parseAuthenticationHeader;dur="#, v.duration.as_server_timing_duration().to_string().as_str(), r#";desc="Parseing HTTP Authentication Header""#]);
            Some(v)
        },
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
    timings.push_iter_nodelim([r#"verifyPathValid;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Verify Path to not contain any malicious items""#]);
    tracing::info!("get_repo_file: {repo}: path checks took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let config = match get_repo_config(&repo).await {
        Ok(v) => v,
        Err(e) => {
            let mut ret = e.to_return();

            match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                Ok(v) => {ret.header_map.get_or_insert_default().append(crate::SERVER_TIMINGS, v);}
                Err(err) => {
                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                }
            }
            return ret;
        },
    };
    next = Instant::now();
    timings.push_iter_nodelim([r#"getMainConfig;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Get Repo Config""#]);
    tracing::info!("get_repo_file: {repo}: get_repo_config took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    match config.check_auth(actix_web::http::Method::GET, auth, str_path) {
        Err(mut err) => {
            err.header_map.get_or_insert_default().append(actix_web::http::header::VARY, actix_web::http::header::HeaderValue::from_static("Authorization"));
            config.apply_cache_control(&mut err);
            return err
        },
        Ok(true) => {
            header_map.append(actix_web::http::header::VARY, actix_web::http::header::HeaderValue::from_static("Authorization"))
        },
        Ok(false) => {},
    }
    next = Instant::now();
    timings.push_iter_nodelim([r#"verifyAuth;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Verify Authentication Information""#]);
    tracing::info!("get_repo_file: {repo}: auth check took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let resolve_impl = resolve_impl(&repo, path.as_path(), str_path, &config, &mut timings, &request_headers).await;
    next = Instant::now();
    timings.push_iter_nodelim([r#"resolveImpl;dur="#, (next-start).as_server_timing_duration().to_string().as_str(), r#";desc="Total Resolve Implementation""#]);
    tracing::info!("get_repo_file: {repo}: get_repo_file_impl check took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let (metadata, content, hash, mut timing, dir_listing) = match resolve_impl {
        Ok(StoredRepoPath::Mmap{metadata, data, hash, timing}) => (vec![metadata], Content::Mmap(data), hash, timing, false),
        Ok(StoredRepoPath::IsADir) => {
            let mut ret = Return {
                status: actix_web::http::StatusCode::PERMANENT_REDIRECT,
                content: Content::Empty,
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: None,
            };
            let header_map = ret.header_map.get_or_insert_default();
            let mut location = request_headers.path.to_string();
            if !location.ends_with("/") {
                location.push('/');
            }
            match actix_web::http::header::HeaderValue::from_str(location.as_str()) {
                Ok(v) => {header_map.append(actix_web::http::header::LOCATION, v);}
                Err(err) => {
                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", location);
                }
            }
            return ret;
        },
        Ok(StoredRepoPath::DirListing{metadata, entries}) => {
            let out = entries_to_content(&entries);
            let hash = blake3::Hasher::new().update(out.as_bytes()).finalize();
            (metadata, Content::String(out), hash, ServerTimings::new(), true)
        },
        Ok(StoredRepoPath::Upstream(upstream)) => {
            let mut ret = Return{
                status: actix_web::http::StatusCode::OK,
                content: Content::Response(upstream),
                content_type: actix_web::http::header::ContentType::octet_stream(),
                header_map: None,
            };
            let header_map  = ret.header_map.get_or_insert_default();
            match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                Err(err) => {
                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                }
            }
            header_map.append(actix_web::http::header::CACHE_CONTROL, actix_web::http::header::HeaderValue::from_static("no-store"));
            config.apply_cache_control(&mut ret);
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
                status: status_code.map(|codes|codes.into_iter().min()).unwrap_or(None).unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
                content: Content::String(out),
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: Default::default(),
            };
            match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                Ok(v) => {ret.header_map.get_or_insert_default().append(crate::SERVER_TIMINGS, v);}
                Err(err) => {
                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                }
            }
            config.apply_cache_control(&mut ret);
            return ret;
        }
    };
    timings.append(&mut timing);

    let mut ret = header_check(&repo, &path, &config, str_path, timings, content, dir_listing, &request_headers, hash, &metadata, header_map, &mut start, &mut next).await;
    config.apply_cache_control(&mut ret);
    ret
}
enum StoredRepoPath{
    Mmap{
        metadata: std::fs::Metadata,
        data: memmap2::Mmap,
        hash: blake3::Hash,
        timing: ServerTimings,
    },
    IsADir,
    Upstream(reqwest::Response),
    DirListing{
        metadata: Vec<std::fs::Metadata>,
        entries: HashMap<String, FileType>,
    }
}
fn entries_to_content(entries: &HashMap<String, FileType>) -> String {
    let mut out = r#"<!DOCTYPE HTML><html><head><meta charset="utf-8"><meta name="color-scheme" content="dark light"></head><body><ul>"#.to_owned();
    let mut v = entries.iter().map(|(key, value)|{
        if value.is_dir() {
            let mut key = key.clone();
            key.push('/');
            Cow::Owned(key)
        } else {
            Cow::Borrowed(key.as_str())
        }
    }).collect::<Vec<_>>();
    v.sort();
    for entry in v {
        out.push_str(r#"<li><a href=""#);
        out.push_str(entry.as_ref());
        out.push_str(r#"">"#);
        out.push_str(entry.as_ref());
        out.push_str("</a></li>");
    }
    out.push_str("</ul></body></html>");

    out
}