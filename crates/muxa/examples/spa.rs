//! A single-page app served next to an API: `SpaPlugin` + `WebPlugin`.
//!
//! ```text
//! cargo run --example spa --features spa
//! curl -i localhost:3000/games/1               # index.html, no-cache
//! curl -i localhost:3000/assets/app-abc123.js  # immutable
//! curl -i localhost:3000/api/hello             # the API
//! curl -i localhost:3000/api/nope              # plain 404
//! ```
//!
//! A real app collects its frontend build in `build.rs`, with `memory-serve` as
//! a build-dependency, and passes `spa_assets!()` to the plugin:
//!
//! ```ignore
//! // build.rs
//! fn main() {
//!     let dist = std::path::Path::new("../frontend/dist");
//!     println!("cargo::rerun-if-changed={}", dist.display());
//!     if dist.is_dir() {
//!         memory_serve::load_directory(dist);
//!     } else {
//!         memory_serve::load_names_directories(std::iter::empty::<(&str, &str)>(), false);
//!     }
//! }
//!
//! // main.rs
//! .with_plugin(SpaPlugin::new(spa_assets!())).await?
//! ```
//!
//! The facade crate has no build script of its own, so this example writes out
//! by hand the asset list `build.rs` would generate for `examples/spa/dist`.

use axum::Router;
use axum::routing::get;
use muxa::prelude::*;
use muxa::web::spa::memory_serve::Asset;

/// One embedded file, as `memory_serve::load_directory` generates it.
const fn file(route: &'static str, content_type: &'static str, bytes: &'static [u8]) -> Asset {
    Asset {
        route,
        path: "",
        // memory-serve fills in the SHA-256 of the file; any stable tag works.
        etag: "example",
        content_type,
        bytes: Some(bytes),
        is_compressed: false,
        should_compress: false,
    }
}

static DIST: &[Asset] = &[
    file(
        "/index.html",
        "text/html",
        include_bytes!("spa/dist/index.html"),
    ),
    file(
        "/assets/app-abc123.js",
        "text/javascript",
        include_bytes!("spa/dist/assets/app-abc123.js"),
    ),
];

fn routes<S>(_state: &S) -> Router {
    Router::new()
        .nest(
            "/api",
            Router::new().route("/hello", get(|| async { "hello" })),
        )
        .route("/health", get(|| async { "ok" }))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::named("spa")
        .with_plugin(OtelPlugin)
        .await?
        // Before the web plugin, which goes last.
        .with_plugin(SpaPlugin::new(DIST))
        .await?
        .with_plugin(WebPlugin::new(routes))
        .await?
        .run()
        .await
}
