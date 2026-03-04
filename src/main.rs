extern crate core;

use std::collections::HashMap;
use std::convert::Infallible;
use std::io::SeekFrom;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use actix_web::dev::Payload;
use actix_web::HttpRequest;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use crate::auth::BasicAuthentication;
use crate::repository::Repository;
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
static REPOSITORIES:LazyLock<tokio::sync::RwLock<HashMap<Arc<str>, (tokio::fs::File, Arc<Repository>)>>> = LazyLock::new(||tokio::sync::RwLock::new(HashMap::new()));
mod private {
    use std::io::SeekFrom;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    use crate::repository::Repository;

    static MAIN_CONFIG:tokio::sync::RwLock<Option<(tokio::fs::File, Arc<Repository>)>> = tokio::sync::RwLock::const_new(None);
    async fn read_main_config() -> anyhow::Result<(tokio::fs::File, Arc<Repository>)> {
        let mut file = tokio::fs::File::open("..main.json").await?;
        let config = read_main_config_file(&mut file, false).await?;
        Ok((file, Arc::new(config)))
    }
    async fn read_main_config_file(file: &mut tokio::fs::File, seek: bool) -> anyhow::Result<Repository> {
        if seek{
            file.seek(SeekFrom::Start(0)).await?;
        }

        let mut contents = String::new();
        file.read_to_string(&mut contents).await?;

        let config:Repository = serde_json::from_str(&contents)?;
        Ok(config)
    }
    pub async fn get_main_config() -> anyhow::Result<Arc<Repository>> {
        //fast path
        if let Some((_, v)) = &*MAIN_CONFIG.read().await{
            return Ok(v.clone());
        }
        //we might have initialized the config in another thread in the meantime
        let mut config_mut = MAIN_CONFIG.write().await;
        if let Some((_, v)) = &*config_mut{
            return Ok(v.clone());
        }

        let (file, config) = read_main_config().await?;
        *config_mut = Some((file, config.clone()));

        Ok(config)
    }
    pub async fn refresh_config() -> anyhow::Result<Arc<Repository>> {
        let mut config_mut = MAIN_CONFIG.write().await;
        if let Some((file, v)) = &mut *config_mut{
            *Arc::make_mut(v) = read_main_config_file(file, true).await?;
            Ok(v.clone())
        } else {
            let (file, config) = read_main_config().await?;
            *config_mut = Some((file, config.clone()));
            Ok(config)
        }
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
    async_main()
}

#[actix_web::main]
async fn async_main() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        tokio::task::spawn(async move {loop {
            signal.recv().await;
            let start = Instant::now();
            tracing::info!("Clearing Repository Cache");
            let main_config = match private::refresh_config().await {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!("There was an error, whilst reading the main config: {err}");
                    continue;
                }
            };
            for (key, (file, repo)) in REPOSITORIES.write().await.iter_mut() {
                let mut content = String::new();
                match file.seek(SeekFrom::Start(0)).await {
                    Ok(_) => {},
                    Err(err) => {
                        tracing::error!("Could not seek file for {key}. Keeping old config. Error: {err}");
                        continue;
                    }
                }
                match file.read_to_string(&mut content).await {
                    Ok(_) => {},
                    Err(err) => {
                        tracing::error!("Could not read file for {key}. Keeping old config. Error: {err}");
                        continue;
                    }
                }
                let mut config = match serde_json::from_str::<Repository>(&content){
                    Ok(v) => v,
                    Err(err) => {
                        tracing::error!("Failed Deserializing config for {key}. Keeping old Config. Error: {err}");
                        continue;
                    }
                };
                config.merge(&main_config);
                *Arc::make_mut(repo) = config;
            }
            let time = start.elapsed();
            tracing::info!("Cleared Repository Cache in {}ns", time.as_nanos());
        }});
    }
    let server = actix_web::HttpServer::new(||
        actix_web::App::new()
            .wrap(actix_web::middleware::Logger::default())
            .wrap(actix_web::middleware::NormalizePath::new(actix_web::middleware::TrailingSlash::MergeOnly))
            .default_service(actix_web::web::route().to(repo_file))
    );

    #[cfg(feature = "socket")]
    let server = server.bind_uds("server.socket")?;
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