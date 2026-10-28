mod api;
mod config;
mod frontend;
mod history;
mod home_assistant;

use axum::Router;
use config::Config;
use tokio::net::TcpListener;

use api::AppState;
use home_assistant::HomeAssistantClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let config = Config::from_env()?;

    let ha = HomeAssistantClient::new(config.ha_url, config.ha_token)?;

    let state = AppState {
        ha,
        collection: api::CollectionConfig::from_env()?,
    };

    let history =
        history::History::open(history::HistoryConfig::from_env().map_err(|e| e.to_string())?)
            .map_err(|_| "Unable to open/migrate history database")?;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let collector = history::start(history.clone(), state.clone(), stop_rx);
    let app = Router::new()
        .nest("/api/history", history::router(history))
        .merge(frontend::router())
        .nest("/api", api::router(state));

    let address = format!("{}:{}", config.host, config.port);

    let listener = TcpListener::bind(&address).await?;

    println!(
        "HEMS-Core v{} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        address
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = stop_tx.send(true);
        })
        .await?;
    collector.await?;

    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
