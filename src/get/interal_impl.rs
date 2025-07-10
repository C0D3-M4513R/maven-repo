use std::collections::HashSet;
use std::path::{Component, Path};
use std::sync::{Arc, LazyLock};
use tokio::task::JoinSet;
use tokio::time::Instant;
use crate::err::GetRepoFileError;
use crate::get::{serve_remote_repository, serve_repository_stored_path, StoredRepoPath};
use crate::repository::{get_repo_look_locations, Repository, Upstream};
use crate::RequestHeaders;
use crate::server_timings::AsServerTimingDuration;

pub async fn resolve_impl(repo: &str, path: &Path, str_path: &str, config: &Arc<Repository>, timings: &mut Vec<String>, request_headers: &RequestHeaders<'_>, rocket_config: &rocket::Config) -> Result<StoredRepoPath, Vec<GetRepoFileError>> {
    let mut start = Instant::now();
    let mut next;

    let (configs, mut errors) = get_repo_look_locations(repo, &config).await;
    next = Instant::now();
    timings.push(format!(r#"resolveImplGetLocalRepoConfigs;dur={};desc="Resolve Implementation: Fetch all local upstream repo configs""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file_impl: {repo}: get_repo_look_locations took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    let mut js = JoinSet::new();

    let mut check_result = async |js:&mut JoinSet<_>|{
        let mut out = None;
        while let Some(task) = js.join_next().await {
            match task {
                Ok(Ok(v)) => {
                    out = match (out, v) {
                        (_, StoredRepoPath::IsADir) =>  {
                            js.abort_all();
                            return Some(StoredRepoPath::IsADir);
                        }
                        (Some(StoredRepoPath::DirListing{mut metadata, mut entries}), StoredRepoPath::DirListing{metadata: mut metadata_1, entries: entries_1}) => {
                            entries.extend(entries_1);
                            metadata.append(&mut metadata_1);
                            Some(StoredRepoPath::DirListing{metadata, entries})
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

    let str_path = Arc::<str>::from(str_path);
    for (repo, repo_config) in &configs {
        let display_dir = !config.hide_directory_listings.unwrap_or(repo_config.hide_directory_listings.unwrap_or(false));
        js.spawn(serve_repository_stored_path(Path::new(&repo).join(&path), display_dir, request_headers.has_trailing_slash, repo_config.clone(), str_path.clone()));
    }

    if let Some(v) = check_result(&mut js).await {
        next = Instant::now();
        timings.push(format!(r#"resolveImplQueryLocalRepositoriesFound;dur={};desc="Resolve Implementation: Query local repositories for File (HIT)""#, (next-start).as_server_timing_duration()));
        tracing::info!("get_repo_file_impl: {repo}: final resolve took took {}µs (skipped remotes, as the information could be locally sourced)", (next-start).as_micros());
        core::mem::swap(&mut start, &mut next);
        return Ok(v);
    }

    next = Instant::now();
    timings.push(format!(r#"resolveImplQueryLocalRepositoriesMiss;dur={};desc="Resolve Implementation: Query local repositories for File (MISS)""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file_impl: {repo}: local resolve took took {}µs", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);
    if path.components().any(|v|match v {
        Component::Normal(v) => {
            //valid utf-8 should have been checked earlier
            v.to_string_lossy().starts_with(".")
        },
        _ => false,
    }) {
        errors.push(GetRepoFileError::FileStartsWithDot);
        return Err(errors);
    }
    if request_headers.has_trailing_slash {
        errors.push(GetRepoFileError::NotFound);
        return Err(errors);
    }

    //Start requests to upstreams
    {
        let mut upstreams = HashSet::new();
        let remote_path = LazyLock::new(||Arc::<Path>::from(path));
        let request_url = LazyLock::new(||Arc::<str>::from({
            let mut domain = String::new();
            match request_headers.headers.get_one("X-Forwarded-Proto") {
                Some(v) => {
                    domain.push_str(v);
                    domain.push_str("://");
                },
                None => {
                    if rocket_config.tls_enabled() {
                        domain.push_str("https://");
                    } else {
                        domain.push_str("http://");
                    }
                },
            };
            match request_headers.headers.get("Host")
                .chain(request_headers.headers.get("X-Forwarded-Host"))
                .chain(request_headers.headers.get("X-Forwarded-Server"))
                .next()
            {
                Some(v) => {
                    domain.push_str(v);
                }
                None => {
                    domain.push_str("unknown-host");
                }
            }
            if !str_path.starts_with("/") {
                domain.push('/');
            }
            domain.push_str(&str_path);

            domain
        }));
        for (repo, config) in configs {
            for upstream in &config.upstreams {
                let upstream = match upstream {
                    Upstream::Local(_) => continue,
                    Upstream::Remote(v) => v,
                };
                if upstreams.insert(upstream.url.clone()) {
                    js.spawn(serve_remote_repository(
                        upstream.clone(),
                        str_path.clone(),
                        repo.clone(),
                        remote_path.clone(),
                        config.clone(),
                        request_url.clone(),
                        request_headers.client_ip
                    ));
                }
            }
        }
    }

    //Collect requests from upstreams
    if let Some(v) = check_result(&mut js).await {
        next = Instant::now();
        timings.push(format!(r#"resolveImplQueryRemoteRepositoriesHit;dur={};desc="Resolve Implementation: Query remote repositories for File (HIT)""#, (next-start).as_server_timing_duration()));
        tracing::info!("get_repo_file_impl: {repo}: final resolve took took {}µs (contacted remotes)", (next-start).as_micros());
        core::mem::swap(&mut start, &mut next);
        return Ok(v);
    }
    next = Instant::now();
    timings.push(format!(r#"resolveImplQueryRemoteRepositoriesMiss;dur={};desc="Resolve Implementation: Query remote repositories for File (MISS)""#, (next-start).as_server_timing_duration()));
    tracing::info!("get_repo_file_impl: {repo}: final resolve took took {}µs (contacted remotes)", (next-start).as_micros());
    core::mem::swap(&mut start, &mut next);

    Err(errors)
}