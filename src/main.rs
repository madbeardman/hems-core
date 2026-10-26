use axum::{Router, routing::get};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new().route("/", get(|| async { "HEMS-Core is running!" }));

    let listener = TcpListener::bind("127.0.0.1:3001").await?;

    println!("HEMS-Core listening on http://127.0.0.1:3001");

    axum::serve(listener, app).await?;

    Ok(())
}
