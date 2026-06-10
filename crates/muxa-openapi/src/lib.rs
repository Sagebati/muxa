//! muxa-openapi — the `OpenApiPlugin` for muxa.
//!
//! Serves a finished [`aide::openapi::OpenApi`] document as JSON plus an
//! interactive [Scalar](https://scalar.com) docs page. The application builds
//! the spec with aide's [`ApiRouter`](https://docs.rs/aide) (so routes and
//! schemas are derived from the real handlers) and hands the finished document
//! to this plugin:
//!
//! ```ignore
//! use muxa::prelude::*;
//! use aide::axum::ApiRouter;
//! use aide::openapi::OpenApi;
//!
//! let mut api = OpenApi::default();
//! let router = ApiRouter::new()
//!     .api_route("/thing", aide::axum::routing::get(handler))
//!     .finish_api(&mut api);
//!
//! App::default()
//!     .with_plugin(OpenApiPlugin::new(api)).await?   // mounts /openapi.json + /docs
//!     .with_plugin(WebPlugin::new(move |_s| router)).await?
//!     .run().await
//! ```
//!
//! Added **before** `WebPlugin` (like every route-contributing plugin), its
//! routes are merged into the final router at compose time.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aide::openapi::OpenApi;
use axum::body::Bytes;
use axum::http::header;
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use muxa_core::{BuildCtx, Error, Plugin, Result, State};
use serde::Deserialize;

// Re-exported so applications (and `muxa-web`'s aide-aware plugin) use the exact
// aide/schemars versions this crate is built against — the `OpenApi` type then
// unifies, and apps can `use muxa::aide::…` / `use muxa::schemars::JsonSchema`
// without pinning these themselves. (Apps that use the `OperationIo` derive must
// still keep a direct `aide` dep — that macro hardcodes `::aide_macros`.)
pub use aide;
pub use schemars;

fn default_json_path() -> String {
    "/openapi.json".into()
}

fn default_docs_path() -> String {
    "/docs".into()
}

fn default_title() -> String {
    "muxa API".into()
}

/// Configuration for [`OpenApiPlugin`], read from the `[openapi]` figment table
/// (and `MUXA_OPENAPI__*` env vars).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OpenApiConfig {
    /// Path the raw OpenAPI JSON document is served at. Default `/openapi.json`.
    pub json_path: String,
    /// Path the interactive Scalar docs page is served at. Default `/docs`.
    pub docs_path: String,
    /// Title shown on the docs page. Default `muxa API`.
    pub title: String,
}

impl Default for OpenApiConfig {
    fn default() -> Self {
        Self {
            json_path: default_json_path(),
            docs_path: default_docs_path(),
            title: default_title(),
        }
    }
}

/// Plugin that serves a pre-built [`OpenApi`] document and a Scalar docs UI.
pub struct OpenApiPlugin {
    api: OpenApi,
}

impl OpenApiPlugin {
    /// Wrap a finished OpenAPI document (typically the `&mut OpenApi` populated
    /// by aide's `ApiRouter::finish_api`).
    pub fn new(api: OpenApi) -> Self {
        Self { api }
    }
}

/// The standard Scalar standalone embed — loads the viewer from a CDN and
/// points it at our spec URL. Kept tiny and dependency-free so `/docs` works
/// without bundling JS assets.
fn scalar_html(json_path: &str, title: &str) -> String {
    format!(
        "<!doctype html>\n\
         <html>\n  <head>\n    <title>{title}</title>\n\
         \x20   <meta charset=\"utf-8\" />\n\
         \x20   <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
         \x20 </head>\n  <body>\n\
         \x20   <script id=\"api-reference\" data-url=\"{json_path}\"></script>\n\
         \x20   <script src=\"https://cdn.jsdelivr.net/npm/@scalar/api-reference\"></script>\n\
         \x20 </body>\n</html>\n"
    )
}

/// Build the router that serves `api` as JSON at `cfg.json_path` plus a Scalar
/// docs page at `cfg.docs_path`. The document is serialized once here (responses
/// clone the cheap `Bytes`). Shared by [`OpenApiPlugin`] and `muxa-web`'s
/// aide-aware web plugin, so both serve the spec identically.
pub fn docs_router(api: &OpenApi, cfg: &OpenApiConfig) -> Result<Router> {
    let json = Bytes::from(serde_json::to_vec(api).map_err(Error::other)?);
    let docs = scalar_html(&cfg.json_path, &cfg.title);

    Ok(Router::new()
        .route(
            &cfg.json_path,
            get(move || {
                let json = json.clone();
                async move { ([(header::CONTENT_TYPE, "application/json")], json) }
            }),
        )
        .route(
            &cfg.docs_path,
            get(move || {
                let docs = docs.clone();
                async move { Html(docs) }
            }),
        ))
}

impl<S: State> Plugin<S> for OpenApiPlugin {
    type Output = ();
    type Config = OpenApiConfig;
    const CONFIG_PREFIX: &'static str = "openapi";

    async fn build(self, cfg: OpenApiConfig, _state: &S, ctx: &mut BuildCtx) -> Result<()> {
        let router = docs_router(&self.api, &cfg)?;
        tracing::info!(json = %cfg.json_path, docs = %cfg.docs_path, "muxa-openapi: mounting docs");
        ctx.router.mount("/", router);
        Ok(())
    }
}
