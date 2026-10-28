//! Dashboard assets are built once, then embedded in the binary. No Node.js or
//! asset directory is needed on the Raspberry Pi at runtime.
use axum::{
    Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

const HTML: &str = include_str!("../../frontend/dist/index.html");
const JS: &str = include_str!("../../frontend/dist/app.js");
const CSS: &str = include_str!("../../frontend/dist/styles.css");

fn asset(content: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"),
        ],
        content,
    ).into_response()
}

pub fn router() -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { asset(HTML, "text/html; charset=utf-8") }),
        )
        .route(
            "/assets/app.js",
            get(|| async { asset(JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/assets/styles.css",
            get(|| async { asset(CSS, "text/css; charset=utf-8") }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn compiled_assets_have_correct_types_and_do_not_swallow_unknown_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router()).await.unwrap();
        });
        let client = reqwest::Client::new();
        for (path, mime) in [
            ("/", "text/html"),
            ("/assets/app.js", "text/javascript"),
            ("/assets/styles.css", "text/css"),
        ] {
            let response = client.get(format!("{url}{path}")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()[header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with(mime)
            );
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
            assert!(
                response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .contains("connect-src 'self'")
            );
            assert!(!response.text().await.unwrap().is_empty());
        }
        for path in [
            "/api/missing",
            "/assets/missing.js",
            "/.env",
            "/src/main.rs",
        ] {
            assert_eq!(
                client
                    .get(format!("{url}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        task.abort();
    }
}
