//! End-to-end demo of the muxa framework — **SQLite flavour, sessionless**.
//!
//! Runs a server with `OtelPlugin` → `SqlitePlugin` → `WebPlugin`. SQLite
//! needs no external infrastructure; by default the plugin opens an
//! in-memory database. Override with `MUXA_MUXA__SQLITE__URL` to use a file.
//!
//! ```text
//! cargo run --example hello --features sqlite
//! curl localhost:3000/
//! curl localhost:3000/health
//! ```
//!
//! The example pins `app.toml` to the crate's manifest directory so it
//! loads correctly regardless of where `cargo run` is invoked from.
//!
//! `app.toml` is a single file owned by this example "app" — its own
//! `[app]` section sits alongside a `[muxa]` table holding every plugin's
//! config (`[muxa.web]`, `[muxa.otel]`, `[muxa.sqlite]`). No dedicated
//! `muxa.toml` is needed: [`App::with_figment_at`] scopes muxa's plugins to
//! the `muxa` subtree while this example reads its own `[app]` section from
//! the very same figment.

use axum::Router;
use axum::routing::get;
use muxa::prelude::*;
use serde::Deserialize;

/// This example's own config — lives at `[app]` in `app.toml`, next to (not
/// mixed into) muxa's `[muxa]` table.
#[derive(Deserialize)]
struct AppConfig {
    greeting: String,
}

fn routes<S>(_state: &S) -> Router {
    Router::new()
        .route("/", get(|| async { "hello, muxa (sqlite)" }))
        .route("/health", get(|| async { "ok" }))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    let figment =
        muxa::load_figment_from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/app.toml"));
    let app_cfg: AppConfig = figment.extract_inner("app")?;

    App::with_figment_at(figment, "muxa")
        // Observability first — installs a tracing-subscriber so later
        // plugins' info logs go somewhere.
        .with_plugin(OtelPlugin)
        .await?
        // SQLite pool — defaults to in-memory. Override via
        // MUXA_MUXA__SQLITE__URL or [muxa.sqlite] in app.toml.
        .with_plugin(SqlitePlugin)
        .await
        .inspect(|_| tracing::info!(greeting = %app_cfg.greeting, "loaded app config"))?
        // WebPlugin must be added last so its `routes` callback sees the
        // fully-composed state HList.
        .with_plugin(WebPlugin::new(routes))
        .await?
        .run()
        .await
}
