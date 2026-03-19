use std::path::Path;
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
    request_headers: &RequestHeaders,
    hash: blake3::Hash,
    metadata: &Vec<std::fs::Metadata>,
    mut header_map: actix_web::http::header::HeaderMap,
    start: &mut Instant,
    next: &mut Instant,
) -> Return {
    let mut status = actix_web::http::StatusCode::OK;
    let mut content_type = if dir_listing {
        actix_web::http::header::ContentType::html()
    } else {
        actix_web::http::header::ContentType::octet_stream()
    };

    let etag = format!(r#""blake3-{}""#,::data_encoding::BASE64.encode(hash.as_bytes()));
    match actix_web::http::header::HeaderValue::from_str(etag.as_str()) {
        Ok(v) => {
            header_map.append(actix_web::http::header::ETAG, v);
        }
        Err(err) => {
            tracing::warn!("Cannot make a header value from '{etag}': {err}")
        }
    }
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
        let modification_datetime = modification_datetime.to_rfc2822();
        match actix_web::http::header::HeaderValue::from_str(modification_datetime.as_str()){
            Ok(v) => { header_map.append(actix_web::http::header::LAST_MODIFIED, v); }
            Err(err) => {
                tracing::warn!("Cannot convert '{modification_datetime}' to a header-value: {err}");
            }
        }
    }

    if dir_listing {
        for i in &config.cache_control_dir_listings {
            i.add_to_map(&mut header_map);
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
                i.add_to_map(&mut header_map);
                if filename.is_empty() {
                    content_type = actix_web::http::header::ContentType::xml();
                } else {
                    content_type = actix_web::http::header::ContentType::plaintext();
                }
            }
        } else {
            for i in &config.cache_control_file {
                i.add_to_map(&mut header_map);
            }
        }
    }

    // Check for If-None-Match header
    let mut contains_none_match = false;
    for i in request_headers.headers.get_all("If-None-Match") {
        let i = match i.to_str(){
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("Couldn't create string from a header-value '{}': {err}", String::from_utf8_lossy(i.as_bytes()));
                continue;
            }
        };
        contains_none_match = true;
        let v = match ETagValidator::parse(i) {
            Some(ETagValidator::Any) => {
                content = Content::None;
                status = actix_web::http::StatusCode::NOT_MODIFIED;
                break
            },
            Some(ETagValidator::Tags(v)) => v,
            None => {
                *next = Instant::now();
                timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                core::mem::swap(start, next);

                header_map.clear();

                match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                    Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                    Err(err) => {
                        tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                    }
                }

                return Return {
                    status: actix_web::http::StatusCode::BAD_REQUEST,
                    content: Content::String(format!("Bad If-None-Match header: {i}")),
                    content_type: actix_web::http::header::ContentType::plaintext(),
                    header_map: Some(header_map),
                }
            }
        };
        //This is strict checking, which is against spec, but we have 0 clue what the files actually contain
        // (and additionally this implementation disallows re-deploys via PUT [you'd have to DELETE and then PUT, once implemented])
        for tag in v {
            if tag.matches(&hash).await.unwrap_or(false) {
                content = Content::None;
                status = actix_web::http::StatusCode::NOT_MODIFIED;
                break
            }
        }
    }
    // Check for If-Match header
    if request_headers.headers.get(actix_web::http::header::IF_MATCH).is_some() {
        let mut any_match = false;
        for i in request_headers.headers.get_all("If-Match") {
            let i = match i.to_str(){
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!("Couldn't create string from a header-value '{}': {err}", String::from_utf8_lossy(i.as_bytes()));
                    continue;
                }
            };
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

                    header_map.clear();

                    match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                        Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                        Err(err) => {
                            tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                        }
                    }

                    return Return {
                        status: actix_web::http::StatusCode::BAD_REQUEST,
                        content: Content::String(format!("Bad If-Match header: {i}")),
                        content_type: actix_web::http::header::ContentType::plaintext(),
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
            header_map.clear();

            match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                Err(err) => {
                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                }
            }

            return Return{
                status: actix_web::http::StatusCode::PRECONDITION_FAILED,
                content: Content::None,
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: Some(header_map),
            }
        }

    }

    // Check for If-Unmodified-Since and If-Modified-Since
    if
        request_headers.headers.get(actix_web::http::header::IF_UNMODIFIED_SINCE).is_some() ||
        //When used in combination with If-None-Match, it is ignored - https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/If-Modified-Since
        (!contains_none_match && request_headers.headers.get(actix_web::http::header::IF_MODIFIED_SINCE).is_some())
    {
        if let Some(modification_datetime) = modification_datetime {
            let modification_datetime = chrono::DateTime::<chrono::Utc>::from(modification_datetime);
            if !contains_none_match {
                for i in request_headers.headers.get_all("If-Modified-Since") {
                    let i = match i.to_str(){
                        Ok(v) => v,
                        Err(err) => {
                            tracing::warn!("Couldn't create string from a header-value '{}': {err}", String::from_utf8_lossy(i.as_bytes()));
                            continue;
                        }
                    };
                    match chrono::DateTime::parse_from_rfc2822(i) {
                        Ok(http_time) => {
                            if http_time > modification_datetime {
                                status = actix_web::http::StatusCode::NOT_MODIFIED;

                                *next = Instant::now();
                                timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                                tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                                core::mem::swap(start, next);


                                match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                                    Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                                    Err(err) => {
                                        tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                                    }
                                }

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

                            header_map.clear();

                            match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                                Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                                Err(err) => {
                                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                                }
                            }

                            return Return{
                                status: actix_web::http::StatusCode::BAD_REQUEST,
                                content: Content::String(format!("Invalid value '{i}' in If-Modified-Since header: {err}")),
                                content_type: actix_web::http::header::ContentType::plaintext(),
                                header_map: Some(header_map),
                            }
                        }
                    }
                }
            }
            for i in request_headers.headers.get_all("If-Unmodified-Since") {
                let i = match i.to_str(){
                    Ok(v) => v,
                    Err(err) => {
                        tracing::warn!("Couldn't create string from a header-value '{}': {err}", String::from_utf8_lossy(i.as_bytes()));
                        continue;
                    }
                };
                match chrono::DateTime::parse_from_rfc2822(i) {
                    Ok(http_time) => {
                        if http_time <= modification_datetime {
                            status = actix_web::http::StatusCode::PRECONDITION_FAILED;

                            *next = Instant::now();
                            timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
                            tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
                            core::mem::swap(start, next);

                            match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                                Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                                Err(err) => {
                                    tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                                }
                            }

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
                        header_map.clear();

                        match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
                            Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
                            Err(err) => {
                                tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
                            }
                        }

                        return Return{
                            status: actix_web::http::StatusCode::BAD_REQUEST,
                            content: Content::String(format!("Invalid value '{i}' in If-Modified-Since header: {err}")),
                            content_type: actix_web::http::header::ContentType::plaintext(),
                            header_map: Some(header_map),
                        }
                    }
                }
            }
        } else {
            return Return{
                status: actix_web::http::StatusCode::BAD_REQUEST,
                content: Content::String(modification_err.iter().map(|v|format!("Could not get Modification time: {v}")).collect::<Vec<_>>().join("\n")),
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: Some(header_map),
            }
        }
    }
    *next = Instant::now();
    timings.push_iter_nodelim([r#"condHeader;dur="#, (*next-*start).as_server_timing_duration().to_string().as_str(), r#";desc="Parsing,Validation and Evaluation of conditional request Headers""#]);
    tracing::info!("get_repo_file: {repo}: header checks took {}µs", (*next-*start).as_micros());
    core::mem::swap(start, next);

    match actix_web::http::header::HeaderValue::from_str(timings.value.as_str()) {
        Ok(v) => {header_map.append(crate::SERVER_TIMINGS, v);}
        Err(err) => {
            tracing::warn!("Cannot convert '{}' to a header-value: {err}", timings.value);
        }
    }

    Return {
        status,
        content,
        content_type,
        header_map: Some(header_map)
    }
}