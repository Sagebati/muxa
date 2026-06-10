//! Minimal muxa demo: WebPlugin only, plus OtelPlugin for log output.
//!
//! Runs without any external infrastructure. Use this binary to sanity-check
//! the serve loop in isolation from the database-dependent plugins.
//!
//! ```text
//! cargo run -p hello --bin web_only
//! curl localhost:3000/
//! ```

use axum::Router;
use axum::routing::get;
use muxa::prelude::*;

fn routes<S>(_state: &S) -> Router {
    Router::new()
        .route("/", get(|| async { "hello from muxa-web" }))
        .route("/health", get(|| async { "ok" }))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::default()
        .with_plugin(OtelPlugin)
        .await?
        .with_plugin(WebPlugin::new(routes))
        .await?
        .run()
        .await
}
