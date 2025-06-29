use std::sync::LazyLock;
use std::time::Instant;
use crate::repository::Repository;

mod get;
mod repository;
mod status;
static CLIENT:LazyLock<reqwest::Client> = LazyLock::new(||reqwest::Client::new());
static REPOSITORIES:LazyLock<scc::HashMap<String, Repository>> = LazyLock::new(||scc::HashMap::new());
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
        log::info!("Initialized logging");
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
            log::info!("Clearing Repository Cache");
            REPOSITORIES.clear_async().await;
            let time = start.elapsed();
            log::info!("Cleared Repository Cache in {}ns", time.as_nanos());
        });
    }
    let  _ = rocket::build()
        .mount("/", rocket::routes![
            get::get_repo_file
        ])
        .launch()
        .await?;
    Ok(())
}
