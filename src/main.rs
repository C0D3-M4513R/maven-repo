mod get;
mod repository;
mod status;

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
    let  _ = rocket::build()
        .manage(reqwest::Client::new())
        .mount("/", rocket::routes![
            get::get_repo_file
        ])
        .launch()
        .await?;
    Ok(())
}
