//! Serve a single-page app's static build (Vite, …) from memory, as the app's
//! fallback.
//!
//! The files are collected at compile time by [`memory_serve`]: the app's
//! `build.rs` calls [`memory_serve::load_directory`] on the frontend's `dist`
//! directory, [`spa_assets!`](crate::spa_assets) picks the result up, and
//! [`SpaPlugin`] registers it as the [fallback](muxa_core::RouterRegistry::fallback)
//! so every request no API route matched reaches it:
//!
//! - a file in the build is served as is (`/`, `/favicon.ico`, …);
//! - anything else, on `GET`/`HEAD`, gets `index.html`, so the client-side
//!   router can take the path (`/games/1`);
//! - paths under [`SpaConfig::exclude_prefixes`] (`/api`, …) and missing files
//!   under [`SpaConfig::assets_prefix`] get a plain 404, never `index.html`.
//!
//! In release builds the files are embedded in the binary; in debug builds
//! memory-serve reads them from disk on each request, so a rebuilt frontend
//! shows up without recompiling.
//!
//! ```ignore
//! // build.rs — memory-serve is a build-dependency
//! fn main() {
//!     let dist = std::path::Path::new("../frontend/dist");
//!     println!("cargo::rerun-if-changed={}", dist.display());
//!     if dist.is_dir() {
//!         memory_serve::load_directory(dist);
//!     } else {
//!         // No frontend build: an empty asset set, and SpaPlugin stays inert.
//!         memory_serve::load_names_directories(std::iter::empty::<(&str, &str)>(), false);
//!     }
//! }
//! ```
//!
//! ```ignore
//! // main.rs
//! muxa::app!()
//!     .with_plugin(SpaPlugin::new(spa_assets!())).await?
//!     .with_plugin(ApiPlugin::new(api, seed)).await?
//!     .run().await
//! ```

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse as _, Response};
use http::header::CACHE_CONTROL;
use http::{HeaderValue, Method, StatusCode, Uri};
pub use memory_serve;
use memory_serve::{Asset, CacheControl, MemoryServe};
use muxa_core::{BuildCtx, Plugin, Result, State};
use serde::Deserialize;
use tower::ServiceExt as _;

/// The route of the page served for client-side routes.
const INDEX: &str = "/index.html";

/// `Cache-Control` of fingerprinted files: their name changes with their
/// content, so a cached copy never goes stale.
const IMMUTABLE: HeaderValue = HeaderValue::from_static("max-age=31536000, immutable");

/// `Cache-Control` of the files that must be revalidated on every load.
const NO_CACHE: HeaderValue = HeaderValue::from_static("no-cache");

/// The asset set the app's `build.rs` collected with
/// [`memory_serve::load_directory`] (or `load_names_directories`), as a
/// `&'static [`[`Asset`]`]` for [`SpaPlugin::new`].
///
/// `spa_assets!()` takes the first directory, `spa_assets!("name")` the one of
/// that name. Unlike `memory_serve::load!`, an empty or absent set doesn't
/// panic: it is an empty slice, and the plugin logs a warning and stays inert.
/// The app needs no `memory-serve` dependency of its own besides the
/// build-dependency.
#[macro_export]
macro_rules! spa_assets {
    () => {{
        use $crate::spa::memory_serve;
        let sets: &'static [(&'static str, &'static [memory_serve::Asset])] =
            include!(concat!(env!("OUT_DIR"), "/memory_serve_assets.rs"));
        sets.first().map_or(&[][..], |(_, assets)| *assets)
    }};
    ($name:expr) => {{
        use $crate::spa::memory_serve;
        let sets: &'static [(&'static str, &'static [memory_serve::Asset])] =
            include!(concat!(env!("OUT_DIR"), "/memory_serve_assets.rs"));
        sets.iter()
            .find(|(name, _)| *name == $name)
            .map_or(&[][..], |(_, assets)| *assets)
    }};
}

/// Configuration for [`SpaPlugin`], read from the `[spa]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SpaConfig {
    /// Serve the frontend. With `false` the plugin registers nothing.
    /// Defaults to `true`.
    pub enabled: bool,
    /// Path prefixes that belong to the server, not the frontend: an unmatched
    /// request under one gets a plain 404 instead of `index.html`, so a typo in
    /// an API call fails loudly. Matched per path segment (`/api` covers
    /// `/api` and `/api/x`, not `/apiary`).
    pub exclude_prefixes: Vec<String>,
    /// Where the bundler puts its fingerprinted files (`assets` for Vite).
    /// They're served `immutable`, and a missing one is a 404 — a stale tab
    /// asking for an old chunk must not get HTML back as JavaScript.
    pub assets_prefix: String,
    /// Files that must never be cached, besides HTML (always `no-cache`).
    /// Defaults to the service worker, `/sw.js`.
    pub no_cache: Vec<String>,
}

impl Default for SpaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            exclude_prefixes: ["/api", "/health", "/openapi.json", "/docs"]
                .map(str::to_owned)
                .to_vec(),
            assets_prefix: "/assets".to_owned(),
            no_cache: vec!["/sw.js".to_owned()],
        }
    }
}

/// Serves a single-page app's build as the app's fallback. See the
/// [module docs](self).
///
/// Its routes are a fallback, not a mount, so the API's routes win over them
/// wherever the plugin sits in the chain. Add it **before** the web plugin
/// (`WebPlugin` or `ApiPlugin`), which goes last by convention and only lists
/// in its launch banner what was registered before it.
pub struct SpaPlugin {
    assets: &'static [Asset],
}

impl SpaPlugin {
    /// Serve `assets`, normally [`spa_assets!()`](crate::spa_assets).
    pub fn new(assets: &'static [Asset]) -> Self {
        Self { assets }
    }
}

impl<S: State> Plugin<S> for SpaPlugin {
    type Output = ();
    type Config = SpaConfig;
    const CONFIG_PREFIX: &'static str = "spa";

    async fn build(self, cfg: SpaConfig, _state: &S, ctx: &mut BuildCtx) -> Result<()> {
        if !cfg.enabled {
            tracing::info!("muxa-web: spa disabled");
            return Ok(());
        }
        if self.assets.is_empty() {
            tracing::warn!("muxa-web: no frontend assets were embedded; not serving the spa");
            return Ok(());
        }
        if !self.assets.iter().any(|asset| asset.route == INDEX) {
            tracing::warn!(
                "muxa-web: the frontend assets have no /index.html; not serving the spa"
            );
            return Ok(());
        }
        ctx.router.fallback(router(self.assets, cfg))
    }
}

/// The fallback router: memory-serve's file routes, the `index.html` fallback,
/// and the cache headers.
fn router(assets: &'static [Asset], cfg: SpaConfig) -> Router {
    // Embedded (release) files only change with the binary; files read from
    // disk (debug) change under a running server, so nothing caches them.
    let embedded = assets.iter().all(|asset| asset.bytes.is_some());
    let files = MemoryServe::new(assets)
        .index_file(Some(INDEX))
        .html_cache_control(CacheControl::NoCache)
        .cache_control(if embedded {
            CacheControl::Medium
        } else {
            CacheControl::NoCache
        })
        .into_router();

    let cfg: &'static SpaConfig = Box::leak(Box::new(cfg));
    let index = files.clone();
    files
        .fallback(move |request: Request| client_route(request, index, cfg))
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            cache_headers(request, next, cfg, embedded)
        }))
}

/// A path that is not a file of the build: `index.html` if it may be a
/// client-side route, else 404.
async fn client_route(request: Request, files: Router, cfg: &SpaConfig) -> Response {
    let path = request.uri().path();
    let excluded = cfg
        .exclude_prefixes
        .iter()
        .chain([&cfg.assets_prefix])
        .any(|prefix| under(path, prefix));
    if excluded || !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Keep the method and headers (`If-None-Match`, `Accept-Encoding`).
    let (mut parts, _) = request.into_parts();
    parts.uri = Uri::from_static(INDEX);
    match files
        .oneshot(Request::from_parts(parts, Body::empty()))
        .await
    {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    }
}

/// Override memory-serve's `Cache-Control` where the path says more than the
/// content type: fingerprinted files and the `no_cache` list.
async fn cache_headers(request: Request, next: Next, cfg: &SpaConfig, embedded: bool) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let value = if cfg.no_cache.contains(&path) {
        NO_CACHE
    } else if embedded && under(&path, &cfg.assets_prefix) {
        IMMUTABLE
    } else {
        return response;
    };
    response.headers_mut().insert(CACHE_CONTROL, value);
    response
}

/// `path` is `prefix` or below it, segment-wise.
fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use axum::routing::get;
    use figment::Figment;
    use muxa_core::App;

    use super::*;

    /// What `memory_serve::load_directory` generates in a release build.
    const fn embedded(
        route: &'static str,
        content_type: &'static str,
        body: &'static str,
    ) -> Asset {
        Asset {
            route,
            path: "",
            etag: "0123456789abcdef",
            content_type,
            bytes: Some(body.as_bytes()),
            is_compressed: false,
            should_compress: false,
        }
    }

    static DIST: &[Asset] = &[
        embedded(
            "/index.html",
            "text/html",
            "<!doctype html><title>spa</title>",
        ),
        embedded("/assets/app-abc123.js", "text/javascript", "console.log(1)"),
        embedded("/sw.js", "text/javascript", "self.skipWaiting()"),
        embedded("/favicon.ico", "image/x-icon", "ico"),
    ];

    /// The API as `ApiPlugin` would mount it: nested routes and a root route.
    fn mount_api(ctx: &mut BuildCtx) {
        ctx.router.mount(
            "/api",
            Router::new().route("/games/{id}", get(|| async { "game" })),
        );
        ctx.router
            .mount("/", Router::new().route("/health", get(|| async { "ok" })));
    }

    /// The app router as `App::run` composes it, with the spa plugin before
    /// the API's routes, or after them.
    async fn app(assets: &'static [Asset], spa_first: bool) -> Router {
        let mut builder = App::with_figment(Figment::new());
        if !spa_first {
            mount_api(builder.ctx_mut());
        }
        let mut builder = builder.with_plugin(SpaPlugin::new(assets)).await.unwrap();
        if spa_first {
            mount_api(builder.ctx_mut());
        }
        core::mem::take(&mut builder.ctx_mut().router).compose()
    }

    async fn get_path(router: Router, method: Method, path: &str) -> (StatusCode, String, String) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let cache = response
            .headers()
            .get(CACHE_CONTROL)
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, cache, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn client_routes_get_index_uncached() {
        let router = app(DIST, true).await;
        for path in ["/", "/games/1", "/index.html"] {
            let (status, cache, body) = get_path(router.clone(), Method::GET, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(cache, "no-cache", "{path}");
            assert!(body.contains("<title>spa</title>"), "{path}");
        }
    }

    #[tokio::test]
    async fn fingerprinted_assets_are_immutable() {
        let (status, cache, body) =
            get_path(app(DIST, true).await, Method::GET, "/assets/app-abc123.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache, "max-age=31536000, immutable");
        assert_eq!(body, "console.log(1)");
    }

    #[tokio::test]
    async fn service_worker_is_not_cached_and_other_files_are_briefly() {
        let router = app(DIST, true).await;
        let (_, cache, _) = get_path(router.clone(), Method::GET, "/sw.js").await;
        assert_eq!(cache, "no-cache");
        let (_, cache, _) = get_path(router, Method::GET, "/favicon.ico").await;
        assert!(cache.starts_with("max-age=604800"), "{cache}");
    }

    #[tokio::test]
    async fn server_paths_and_missing_assets_are_plain_404s() {
        let router = app(DIST, true).await;
        for path in ["/api/nope", "/api", "/docs/x", "/assets/gone-123.js"] {
            let (status, _, body) = get_path(router.clone(), Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(body.is_empty(), "{path}");
        }
        // Segment-wise: `/apiary` is a client route.
        let (status, _, _) = get_path(router.clone(), Method::GET, "/apiary").await;
        assert_eq!(status, StatusCode::OK);
        // Only reads fall back to the page.
        let (status, _, _) = get_path(router, Method::POST, "/games/1").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_routes_win_over_the_fallback() {
        let router = app(DIST, true).await;
        let (status, _, body) = get_path(router.clone(), Method::GET, "/api/games/1").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "game"));
        let (status, _, body) = get_path(router, Method::GET, "/health").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
    }

    #[tokio::test]
    async fn no_assets_leaves_the_plugin_inert() {
        let (status, _, _) = get_path(app(&[], true).await, Method::GET, "/games/1").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn plugin_order_does_not_matter() {
        let router = app(DIST, false).await;
        let (_, _, body) = get_path(router.clone(), Method::GET, "/api/games/1").await;
        assert_eq!(body, "game");
        let (status, _, _) = get_path(router.clone(), Method::GET, "/api/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, cache, body) = get_path(router, Method::GET, "/games/1").await;
        assert_eq!(cache, "no-cache");
        assert!(body.contains("<title>spa</title>"));
    }

    #[test]
    fn under_matches_whole_segments() {
        assert!(under("/api", "/api"));
        assert!(under("/api/x", "/api"));
        assert!(under("/api/x", "/api/"));
        assert!(!under("/apiary", "/api"));
        assert!(under("/openapi.json", "/openapi.json"));
        assert!(!under("/", "/api"));
    }
}
