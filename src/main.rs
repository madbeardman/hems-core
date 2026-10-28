mod api;
mod config;
mod frontend;
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

    let app = Router::new()
        .merge(frontend::router())
        .nest("/api", api::router(state));

    let address = format!("{}:{}", config.host, config.port);

    let listener = TcpListener::bind(&address).await?;

    println!(
        "HEMS-Core v{} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        address
    );

    axum::serve(listener, app).await?;

    Ok(())
}
