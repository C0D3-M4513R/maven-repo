use std::path::Path;
use base64::Engine;
use rocket::http::{ContentType, Status};
use tokio::time::Instant;
use crate::err::GetRepoFileError;
use crate::etag::ETagValidator;
use crate::get::{serve_repository_stored_path, StoredRepoPath};
use crate::repository::{Repository, Upstream};
use crate::RequestHeaders;
use crate::server_timings::AsServerTimingDuration;
use crate::status::{Content, Return};

pub async fn header_check(
    repo: &str,
    path: &Path,
    config: &Repository,
    str_path: &str,
    timings: &mut Vec<String>,
    map: memmap2::Mmap,
    request_headers: &RequestHeaders<'_>,
    hash: blake3::Hash,
    metadata: &std::fs::Metadata,
    start: &mut Instant,
    next: &mut Instant,
) -> Return {
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

    if str_path.ends_with("maven-metadata.xml") {
        for i in &config.cache_control_metadata {
            header_map.add(i.clone());
        }
    } else {
        for i in &config.cache_control {
            header_map.add(i.clone());
        }
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
                *next = Instant::now();
                timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
                tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                core::mem::swap(start, next);

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
            if tag.matches(&map, &hash, &old_hash, timings).await.unwrap_or(false) {
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
                    *next = Instant::now();
                    timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
                    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                    core::mem::swap(start, next);

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
                if tag.matches(&map, &hash, &old_hash, timings).await.unwrap_or(false) {
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

                            *next = Instant::now();
                            timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
                            tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                            core::mem::swap(start, next);

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
                        *next = Instant::now();
                        timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
                        tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                        core::mem::swap(start, next);

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

                        *next = Instant::now();
                        timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
                        tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                        core::mem::swap(start, next);
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
                    *next = Instant::now();
                    timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
                    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                    core::mem::swap(start, next);
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
    *next = Instant::now();
    timings.push(format!(r#"condHeader;dur={};desc="Parsing,Validation and Evaluation of conditional request Headers""#, (*next-*start).as_server_timing_duration()));
    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
    core::mem::swap(start, next);

    header_map.add(rocket::http::Header::new("Server-Timing", timings.join(",")));
    let content = content.unwrap_or(Content::Mmap(map));

    Return {
        status,
        content,
        content_type,
        header_map: Some(header_map)
    }
}