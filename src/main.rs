extern crate core;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use actix_web::dev::Payload;
use actix_web::HttpRequest;
use crate::auth::BasicAuthentication;
use crate::err::GetRepoFileError;
use crate::repository::{Repository};
use crate::status::{Content, Return};

mod get;
mod repository;
mod status;
mod auth;
mod err;
mod put;
mod maven_metadata;
mod path_info;
mod etag;
mod server_timings;
mod file_metadata;
mod remote;
mod file_ext;
mod timings;

static UNAUTHORIZED: fn() -> Return = ||Return{
    status: actix_web::http::StatusCode::UNAUTHORIZED,
    content: Content::Str("Unauthorized"),
    content_type: actix_web::http::header::ContentType::plaintext(),
    header_map: None,
};
static FORBIDDEN: fn() -> Return = ||Return{
    status: actix_web::http::StatusCode::FORBIDDEN,
    content: Content::Str("Forbidden"),
    content_type: actix_web::http::header::ContentType::plaintext(),
    header_map: None,
};
const DEFAULT_MAX_FILE_SIZE:u64 = 4*1024*1024*1024;
const DEFAULT_FRESH:Duration = Duration::from_secs(6*60*60); //6 hours
const SERVER_TIMINGS: actix_web::http::header::HeaderName = actix_web::http::header::HeaderName::from_static("server-timing");

static CLIENT:LazyLock<reqwest::Client> = LazyLock::new(||{
    let mut map = reqwest::header::HeaderMap::new();
    map.insert("x-powered-by", reqwest::header::HeaderValue::from_static(env!("CARGO_PKG_REPOSITORY")));

    reqwest::ClientBuilder::new()
        .default_headers(map)
        .user_agent(reqwest::header::HeaderValue::from_static(const_format::formatcp!("{}/{} - {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"))))
        .build()
        .expect("Client to be initialized")

});
static MAIN_CONFIG:LazyLock<arc_swap::ArcSwap<Repository>> = LazyLock::new(||{
    let config = private::read_main_config().expect("Failed to read main configuration");
    arc_swap::ArcSwap::new(config)
});
type RepositoryStore = HashMap<Box<str>, Arc<Repository>>;
static REPOSITORIES:LazyLock<arc_swap::ArcSwap<RepositoryStore>> = LazyLock::new(|| {
    let hm = gather_repos(MAIN_CONFIG.load_full()).expect("Failed to read repo configurations");
    arc_swap::ArcSwap::new(Arc::new(hm))
});
mod private {
    use std::io::SeekFrom;
    use std::sync::Arc;
    use crate::repository::Repository;

    pub fn read_main_config() -> anyhow::Result<Arc<Repository>> {
        let mut file = std::fs::File::open("..main.json")?;
        let config = read_main_config_file(&mut file, false)?;
        Ok(Arc::new(config))
    }
    fn read_main_config_file(file: &mut std::fs::File, seek: bool) -> anyhow::Result<Repository> {
        use std::io::{Read, Seek};
        if seek{
            file.seek(SeekFrom::Start(0))?;
        }

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let config:Repository = serde_json::from_str(&contents)?;
        Ok(config)
    }
    #[inline]
    fn set_new_config(config: Arc<Repository>) {
        super::MAIN_CONFIG.swap(config);
    }
    pub fn refresh_config() -> anyhow::Result<Arc<Repository>> {
        let config = read_main_config()?;
        set_new_config(config.clone());

        Ok(config)
    }
}
fn main() -> anyhow::Result<()>{
    match dotenvy::dotenv() {
        Ok(_) => {},
        Err(err) => {
            eprintln!("Could not read .env: {err}");
        }
    }
    LazyLock::force(&CLIENT);
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::Layer;
        let registry = tracing_subscriber::registry();
        #[cfg(tokio_unstable)]
        let registry = registry.with(console_subscriber::spawn());
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
            )
            .init();
        tracing::info!("Initialized logging");
    }
    {
        let _ = MAIN_CONFIG.load();
        let _ = REPOSITORIES.load();
    }

    let rt = ||::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .build()
        .expect("Failed to build tokio runtime");

    ::actix_web::rt::System::with_tokio_rt(rt)
        .block_on(async_main())
}

fn gather_repos(main_config: Arc<Repository>) -> anyhow::Result<HashMap<Box<str>, Arc<Repository>>> {
    let mut hm = HashMap::new();

    let cur_dir = std::env::current_dir()?;
    for i in std::fs::read_dir(cur_dir.as_path())? {
        let res = i?;
        let full_name = res.file_name();
        let name = match full_name.to_str() {
            Some(name) => name,
            None => {
                tracing::warn!("Could not convert file name of {} to string. Skipping entry.", full_name.display());
                continue;
            }
        };

        let ftype = match res.file_type(){
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("Failed to read file_type for '{name}': {err}");
                continue;
            }
        };
        if !ftype.is_file() {
            if !ftype.is_dir() {
                tracing::warn!("'{name}' is not a file or directory. Ignoring.");
            }
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if ftype.is_fifo() || ftype.is_block_device() || ftype.is_char_device() || ftype.is_socket() {
                tracing::warn!("'{name}' is a fifo, block_device, char_device or socket. Ignoring.");
            }
        }

        let name = match name.strip_suffix(".json"){
            Some(name) => name,
            None => {
                tracing::warn!("'{name}' does not end with '.json'. Skipping file");
                continue;
            }
        };

        let name = match name.strip_prefix(".") {
            Some(name) => name,
            None => {
                tracing::warn!("'{name}' does not start with '.'. Skipping file");
                continue;
            }
        };
        if name == ".main" {
            continue;
        }

        let config = match std::fs::read_to_string(cur_dir.join(&full_name)) {
            Err(err) => {
                tracing::error!("Error reading repo config file, file:'{name}' : {err}");
                anyhow::bail!("{}, file: '{name}': {err}", GetRepoFileError::ReadConfig.get_err())
            }
            Ok(v) => v,
        };
        let mut config:Repository = match serde_json::from_str(&config) {
            Err(err) => {
                tracing::error!("Error parsing repo config, file:'{name}' : {err}");
                anyhow::bail!("{}, file: '{name}': {err}", GetRepoFileError::ParseConfig.get_err());
            }
            Ok(v) => v,
        };
        config.merge(&*main_config);
        hm.insert(Box::from(name), Arc::new(config));
    }

    Ok(hm)
}

#[actix_web::main]
async fn async_main() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        tokio::task::spawn(async move {loop {
            signal.recv().await;
            match tokio::task::spawn_blocking(||{
                let start = Instant::now();
                tracing::info!("Refreshing Repository Cache");
                let main_config = match private::refresh_config() {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::error!("There was an error, whilst reading the main config: {err}");
                        return;
                    }
                };
                let hm = gather_repos(main_config).expect("Failed to read repo configurations");
                REPOSITORIES.swap(Arc::new(hm));
                let time = start.elapsed();
                tracing::info!("Cleared Repository Cache in {}ns", time.as_nanos());
            }).await {
                Ok(()) => {},
                Err(err) => {
                    tracing::error!("Panicked whilst refreshing config: {err}");
                }
            }
        }});
    }
    let server = actix_web::HttpServer::new(||
        actix_web::App::new()
            .wrap(actix_web::middleware::Logger::default())
            .wrap(actix_web::middleware::NormalizePath::new(actix_web::middleware::TrailingSlash::MergeOnly))
            .default_service(actix_web::web::route().to(repo_file))
    );

    #[cfg(feature = "socket")]
    let server = server.bind_uds("server.sock")?;
    #[cfg(not(feature = "socket"))]
    let server = server.bind((core::net::IpAddr::V4(core::net::Ipv4Addr::LOCALHOST), 8080))?;

    server.run().await?;
    Ok(())
}
struct RequestHeaders {
    pub headers: actix_web::http::header::HeaderMap,
    pub client_ip: Option<core::net::IpAddr>,
    pub has_trailing_slash: bool,
    pub path: actix_web::http::Uri,
}
impl actix_web::FromRequest for RequestHeaders {
    type Error = Infallible;
    type Future = core::future::Ready<Result<Self, Self::Error>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        let client_ip:Option<core::net::IpAddr> = request.connection_info().realip_remote_addr().map(|v|v.parse().ok()).flatten();

        core::future::ready(Ok(Self{
            headers: request.headers().clone(),
            client_ip,
            has_trailing_slash: request.uri().path().ends_with("/"),
            path: request.uri().clone(),
        }))
    }
}

async fn repo_file(req: actix_web::HttpRequest, auth: Result<BasicAuthentication, Return>, request_headers: RequestHeaders, data: actix_web::web::Payload, method: actix_web::http::Method) -> Return {
    match method {
        actix_web::http::Method::PUT => put::put_repo_file(req, auth, data).await,
        actix_web::http::Method::GET |
        actix_web::http::Method::HEAD
            => get::get_repo_file(req, auth, request_headers).await,
        _ => Return {
            status: actix_web::http::StatusCode::METHOD_NOT_ALLOWED,
            content: Content::None,
            content_type: actix_web::http::header::ContentType::plaintext(),
            header_map: None,
        }
    }
}