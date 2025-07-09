use std::collections::HashMap;
use std::convert::Infallible;
use std::io::SeekFrom;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock};
use std::time::Instant;
use rocket::http::{ContentType, Status};
use rocket::request::Outcome;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
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

const UNAUTHORIZED: Return = Return{
    status: Status::Unauthorized,
    content: Content::Str("Unauthorized"),
    content_type: ContentType::Text,
    header_map: None,
};
const FORBIDDEN: Return = Return{
    status: Status::Forbidden,
    content: Content::Str("Forbidden"),
    content_type: ContentType::Text,
    header_map: None,
};
const DEFAULT_MAX_FILE_SIZE:u64 = 4*1024*1024*1024;

static CLIENT:LazyLock<reqwest::Client> = LazyLock::new(||{
    let mut map = reqwest::header::HeaderMap::new();
    map.insert("x-powered-by", reqwest::header::HeaderValue::from_static(env!("CARGO_PKG_REPOSITORY")));

    reqwest::ClientBuilder::new()
        .default_headers(map)
        .user_agent(reqwest::header::HeaderValue::from_static(const_format::formatcp!("{}/{} - {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"))))
        .build()
        .expect("Client to be initialized")

});
static REPOSITORIES:LazyLock<tokio::sync::RwLock<HashMap<String, (tokio::fs::File, Arc<Repository>)>>> = LazyLock::new(||tokio::sync::RwLock::new(HashMap::new()));
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

    rocket::execute(async_main())
}

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
    let  _ = rocket::build()
        .attach(AddSourceLink)
        .mount("/", rocket::routes![
            get::get_repo_file,
            put::put_repo_file,
        ])
        .launch()
        .await?;
    Ok(())
}

struct AddSourceLink;
#[rocket::async_trait]
impl rocket::fairing::Fairing for AddSourceLink {
    fn info(&self) -> rocket::fairing::Info {
        rocket::fairing::Info{
            name: "Add Source Link",
            kind: rocket::fairing::Kind::Response,
        }
    }

    async fn on_response<'r>(&self, _req: &'r rocket::Request<'_>, res: &mut rocket::Response<'r>) {
        res.set_header(rocket::http::Header::new("X-Powered-By", env!("CARGO_PKG_REPOSITORY")));
    }
}
struct RequestHeaders<'a> {
    pub headers: &'a rocket::http::HeaderMap<'a>,
    pub client_ip: Option<IpAddr>
}
#[rocket::async_trait]
impl<'a> rocket::request::FromRequest<'a> for RequestHeaders<'a> {
    type Error = Infallible;

    async fn from_request(request: &'a rocket::Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self{
            headers: request.headers(),
            client_ip: request.client_ip(),
        })
    }
}