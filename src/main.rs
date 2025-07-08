use std::collections::HashMap;
use std::convert::Infallible;
use std::io::SeekFrom;
use std::sync::LazyLock;
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

static CLIENT:LazyLock<reqwest::Client> = LazyLock::new(||reqwest::Client::new());
static REPOSITORIES:LazyLock<tokio::sync::RwLock<HashMap<String, (tokio::fs::File, Repository)>>> = LazyLock::new(||tokio::sync::RwLock::new(HashMap::new()));
fn main() -> anyhow::Result<()>{
    match dotenvy::dotenv() {
        Ok(_) => {},
        Err(err) => {
            eprintln!("Could not read .env: {err}");
        }
    }
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
        tokio::task::spawn(async move {
            signal.recv().await;
            let start = Instant::now();
            tracing::info!("Clearing Repository Cache");
            for (key, (file, repo)) in REPOSITORIES.write().await.iter_mut() {
                let mut content = String::new();
                match file.seek(SeekFrom::Start(0)).await {
                    Ok(_) => {},
                    Err(err) => {
                        tracing::error!("Could not seek file for {key}. Keeping old config. Error: {err}");
                        return;
                    }
                }
                match file.read_to_string(&mut content).await {
                    Ok(_) => {},
                    Err(err) => {
                        tracing::error!("Could not read file for {key}. Keeping old config. Error: {err}");
                        return;
                    }
                }
                match quick_xml::de::from_str(&content){
                    Ok(v) => *repo = v,
                    Err(err) => {
                        tracing::error!("Failed Deserializing config for {key}. Keeping old Config. Error: {err}");
                    }
                }
            }
            let time = start.elapsed();
            tracing::info!("Cleared Repository Cache in {}ns", time.as_nanos());
        });
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
struct RequestHeaders<'a>(pub &'a rocket::http::HeaderMap<'a>);
#[rocket::async_trait]
impl<'a> rocket::request::FromRequest<'a> for RequestHeaders<'a> {
    type Error = Infallible;

    async fn from_request(request: &'a rocket::Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self(request.headers()))
    }
}