//! End-to-end demo of the muxa framework — **SQLite flavour, sessionless**.
//!
//! Runs a server with `OtelPlugin` → `SqlitePlugin` → `WebPlugin`. SQLite
//! needs no external infrastructure; by default the plugin opens an
//! in-memory database. Override with `MUXA_SQLITE__URL` to use a file.
//!
//! ```text
//! cargo run --example hello --features sqlite
//! curl localhost:3000/
//! curl localhost:3000/health
//! ```
//!
//! The example pins `muxa.toml` to the crate's manifest directory so it
//! loads correctly regardless of where `cargo run` is invoked from.

use axum::Router;
use axum::routing::get;
use muxa::prelude::*;

fn routes<S>(_state: &S) -> Router {
    Router::new()
        .route("/", get(|| async { "hello, muxa (sqlite)" }))
        .route("/health", get(|| async { "ok" }))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::with_config_file(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/muxa.toml"))
        // Observability first — installs a tracing-subscriber so later
        // plugins' info logs go somewhere.
        .with_plugin(OtelPlugin)
        .await?
        // SQLite pool — defaults to in-memory. Override via
        // MUXA_SQLITE__URL or [sqlite] in muxa.toml.
        .with_plugin(SqlitePlugin)
        .await?
        // WebPlugin must be added last so its `routes` callback sees the
        // fully-composed state HList.
        .with_plugin(WebPlugin::new(routes))
        .await?
        .run()
        .await
}
