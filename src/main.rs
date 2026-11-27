mod api;
mod config;
mod costs;
mod export_calibration;
mod frontend;
mod history;
mod home_assistant;
mod modelling;

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
    let calibration_mode = export_calibration::Mode::from_env()?;
    let calibration_state = state.clone();
    let costs = costs::CostsState::from_env(state.ha.clone())?;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let cost_collector = costs::start(costs.clone(), history.clone(), stop_rx.clone());
    let collector = history::start(history.clone(), state.clone(), stop_rx.clone());
    let app = Router::new()
        .nest(
            "/api/modelling",
            modelling::router(
                history.clone(),
                modelling::Settings::from_env().map_err(|e| e.to_string())?,
            ),
        )
        .nest("/api/history", history::router(history))
        .merge(frontend::router())
        .nest("/api", api::router(state).merge(costs::router(costs)));

    let address = format!("{}:{}", config.host, config.port);

    let listener = TcpListener::bind(&address).await?;

    let calibration = export_calibration::start(calibration_mode, calibration_state, stop_rx);

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
    cost_collector.await?;
    if let Some(calibration) = calibration {
        calibration.await?;
    }

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
