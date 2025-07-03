use std::sync::LazyLock;
use std::time::Instant;
use rocket::http::{ContentType, Status};
use crate::repository::Repository;
use crate::status::{Content, Return};

mod get;
mod repository;
mod status;
mod auth;
mod err;
mod put;
mod maven_metadata;

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
static REPOSITORIES:LazyLock<scc::HashMap<String, (tokio::fs::File, Repository)>> = LazyLock::new(||scc::HashMap::new());
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
            REPOSITORIES.clear_async().await;
            let time = start.elapsed();
            tracing::info!("Cleared Repository Cache in {}ns", time.as_nanos());
        });
    }
    let  _ = rocket::build()
        .attach(AddSourceLink)
        .mount("/", rocket::routes![
            get::get_repo_file
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