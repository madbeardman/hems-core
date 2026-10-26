mod api;
mod config;

use axum::{Router, routing::get};
use config::Config;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env();

    let app = Router::new()
        .route("/", get(|| async { "HEMS-Core is running!" }))
        .nest("/api", api::router());

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
