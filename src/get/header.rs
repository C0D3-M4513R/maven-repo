use std::ffi::OsStr;
use std::path::Path;
use base64::Engine;
use rocket::http::{ContentType, HeaderMap, Status};
use tokio::time::Instant;
use crate::etag::ETagValidator;
use crate::repository::Repository;
use crate::RequestHeaders;
use crate::server_timings::AsServerTimingDuration;
use crate::status::{Content, Return};
use crate::timings::ServerTimings;

pub async fn header_check(
    repo: &str,
    path: &Path,
    config: &Repository,
    str_path: &str,
    mut timings: ServerTimings,
    mut content: Content,
    dir_listing: bool,
    request_headers: &RequestHeaders<'_>,
    hash: blake3::Hash,
    metadata: &Vec<std::fs::Metadata>,
    mut header_map: HeaderMap<'static>,
    start: &mut Instant,
    next: &mut Instant,
) -> Return {
    let mut status = Status::Ok;
    let mut content_type = if dir_listing {
        ContentType::HTML
    } else {
        if config.infer_content_type_on_file_extension.unwrap_or(true) {
            path.extension()
                .and_then(OsStr::to_str)
                .and_then(ContentType::from_extension)
                .unwrap_or(ContentType::Binary)
        } else {
            ContentType::Binary
        }
    };

    header_map.add(rocket::http::Header::new("ETag", format!(r#""blake3-{}""#,base64::engine::general_purpose::STANDARD.encode(hash.as_bytes()))));
    let (modification_datetime, modification_err) = {
        let (modification_datetime, errors) = metadata.iter().map(|v|v.modified()).fold((None, Vec::new()), |(mut time, mut errors), res|{
            match res {
                Ok(v) => match &time {
                    None => {time = Some(v);},
                    Some(cmp_time) => {
                        if *cmp_time < v {
                            time = Some(v);
                        }
                    }
                },
                Err(err) => {
                    tracing::error!("Failed to get modification time for {}: {err}", path.display());
                    errors.push(err);
                }
            }
            (time, errors)
        });

        (modification_datetime.map(chrono::DateTime::<chrono::Utc>::from), errors)
    };
    if let Some(modification_datetime) = modification_datetime {
        header_map.add(rocket::http::Header::new("Last-Modified", modification_datetime.to_rfc2822()));
    }

    if dir_listing {
        for i in &config.cache_control_dir_listings {
            header_map.add_raw(i.name.clone(), i.value.clone());
        }
    } else {
        let filename = str_path.rsplit_once("/").map(|(_, v)|v).unwrap_or(str_path);
        let filename = filename.strip_prefix("maven-metadata.xml").unwrap_or(filename);
        let is_metadata = match filename {
            "" | ".md5" | ".sha1" | ".sha256" | ".sha512" => true,
            _ => false,
        };
        if is_metadata {
            for i in &config.cache_control_metadata {
                header_map.add(i.clone());
                if filename.is_empty() {
                    content_type = ContentType::XML;
                } else {
                    content_type = ContentType::Text;
                }
            }
        } else {
            for i in &config.cache_control_file {
                header_map.add(i.clone());
            }
        }
    }

    // Check for If-None-Match header
    let mut contains_none_match = false;
    for i in request_headers.headers.get("If-None-Match") {
        contains_none_match = true;
        let v = match ETagValidator::parse(i) {
            Some(ETagValidator::Any) => {
                content = Content::None;
                status = Status::NotModified;
                break
            },
            Some(ETagValidator::Tags(v)) => v,
            None => {
                *next = Instant::now();
                timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                core::mem::swap(start, next);

                header_map.remove_all();
                header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

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
            if tag.matches(&hash).await.unwrap_or(false) {
                content = Content::None;
                status = Status::NotModified;
                break
            }
        }
    }
    // Check for If-Match header
    if request_headers.headers.contains("If-Match") {
        let mut any_match = false;
        for i in request_headers.headers.get("If-Match") {
            let v = match ETagValidator::parse(i) {
                Some(ETagValidator::Any) => {
                    any_match = true;
                    break;
                },
                Some(ETagValidator::Tags(v)) => v,
                None => {
                    *next = Instant::now();
                    timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                    core::mem::swap(start, next);

                    header_map.remove_all();
                    header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

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
                if tag.matches(&hash).await.unwrap_or(false) {
                    any_match = true;
                    break
                }
            }
        }
        if !any_match {
            header_map.remove_all();
            header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

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
    request_headers.headers.contains("If-Unmodified-Since") ||
        //When used in combination with If-None-Match, it is ignored - https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/If-Modified-Since
        (!contains_none_match && request_headers.headers.contains("If-Modified-Since"))
    {
        if let Some(modification_datetime) = modification_datetime {
            let modification_datetime = chrono::DateTime::<chrono::Utc>::from(modification_datetime);
            if !contains_none_match {
                for i in request_headers.headers.get("If-Modified-Since") {
                    match chrono::DateTime::parse_from_rfc2822(i) {
                        Ok(http_time) => {
                            if http_time > modification_datetime {
                                status = Status::NotModified;

                                *next = Instant::now();
                                timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                                tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                                core::mem::swap(start, next);

                                header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

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
                            timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                            tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                            core::mem::swap(start, next);

                            header_map.remove_all();
                            header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

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
            for i in request_headers.headers.get("If-Unmodified-Since") {
                match chrono::DateTime::parse_from_rfc2822(i) {
                    Ok(http_time) => {
                        if http_time <= modification_datetime {
                            status = Status::PreconditionFailed;

                            *next = Instant::now();
                            timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                            tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                            core::mem::swap(start, next);
                            header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

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
                        timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                        tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                        core::mem::swap(start, next);
                        header_map.remove_all();
                        header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

                        return Return{
                            status: Status::BadRequest,
                            content: Content::String(format!("Invalid value '{i}' in If-Modified-Since header: {err}")),
                            content_type: ContentType::Text,
                            header_map: Some(header_map),
                        }
                    }
                }
            }
        } else {
            return Return{
                status: Status::BadRequest,
                content: Content::String(modification_err.iter().map(|v|format!("Could not get Modification time: {v}")).collect::<Vec<_>>().join("\n")),
                content_type: ContentType::Text,
                header_map: Some(header_map),
            }
        }
    }
    *next = Instant::now();
    timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
    core::mem::swap(start, next);

    header_map.add(rocket::http::Header::new("Server-Timing", timings.value));

    Return {
        status,
        content,
        content_type,
        header_map: Some(header_map)
    }
}