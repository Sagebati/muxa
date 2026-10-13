//! muxa-web — the `WebPlugin` for muxa.
//!
//! `WebPlugin` owns the axum `serve` loop and the graceful-shutdown handshake.
//! It is added **last** in the plugin chain by convention: its `build` method
//! invokes the user-supplied routes function against the current state
//! (which by that point holds every other plugin's resource), composes it
//! with each plugin's auto-mounted tower services and middleware, and
//! schedules the actual serve loop for `App::run`.
//!
//! On wasm (e.g. Cloudflare Workers) there is no serve loop: `WebPlugin` only
//! mounts the routes, and the host finishes the chain with
//! `AppBuilder::into_router` and calls the router itself. The same chain runs
//! on both targets.
//!
//! ```ignore
//! use muxa::prelude::*;
//! use muxa_web::WebPlugin;
//!
//! fn my_routes<S>(_state: &S) -> axum::Router {
//!     axum::Router::new().route("/", axum::routing::get(|| async { "hi" }))
//! }
//!
//! #[tokio::main]
//! async fn main() -> muxa::Result<()> {
//!     App::default()
//!         .with_plugin(WebPlugin::new(my_routes)).await?
//!         .run().await
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(not(target_family = "wasm"))]
mod banner;
#[cfg(feature = "ratelimit")]
pub mod ratelimit;
#[cfg(not(target_family = "wasm"))]
mod serve;

use axum::Router;
use bon::Builder;
#[cfg(feature = "openapi")]
use muxa_core::Sections;
use muxa_core::{BuildCtx, Plugin, Result, State};
use serde::Deserialize;

/// Default bind host.
fn default_host() -> String {
    "0.0.0.0".into()
}

/// Default bind port.
fn default_port() -> u16 {
    3000
}

/// Configuration for [`WebPlugin`].
///
/// Read from the `[web]` configuration section.
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct WebConfig {
    /// Bind host: an IPv4 or IPv6 literal (IPv6 may be bracketed, as in a
    /// URL: `[::1]`) or a hostname such as `localhost`. Defaults to `0.0.0.0`.
    #[builder(default = default_host())]
    pub host: String,
    /// Bind port. Defaults to `3000`.
    #[builder(default = default_port())]
    pub port: u16,
    /// Print a launch banner (bound URL, mount points) to stderr at
    /// startup. Defaults to `true`.
    #[serde(default = "default_true")]
    #[builder(default = true)]
    pub banner: bool,
}

fn default_true() -> bool {
    true
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            banner: true,
        }
    }
}

/// A no-op routes function. Useful when you want a `WebPlugin` with no
/// application-level routes (only plugin-contributed ones).
pub fn no_routes<S>(_state: &S) -> Router {
    Router::new()
}

/// The web plugin. Generic over a `routes` callback that receives the
/// application state HList and returns an `axum::Router`.
///
/// The plugin should be added **last** in the chain so the state passed
/// to `routes` contains every other plugin's resource.
///
/// Natively it registers the serve loop that `App::run` drives. On wasm it
/// only mounts the routes; finish with `AppBuilder::into_router`.
pub struct WebPlugin<R> {
    routes: R,
}

impl<R> WebPlugin<R> {
    /// Construct a `WebPlugin` with the given routes function.
    pub fn new(routes: R) -> Self {
        Self { routes }
    }
}

impl<S, R> Plugin<S> for WebPlugin<R>
where
    S: State,
    R: FnOnce(&S) -> Router + Send + 'static,
{
    type Output = ();
    type Config = WebConfig;
    const CONFIG_PREFIX: &'static str = "web";

    async fn build(self, cfg: WebConfig, state: &S, ctx: &mut BuildCtx) -> Result<()> {
        // Invoke the user routes callback against the current state and mount
        // the resulting router at the root for compose-time merging.
        let user_router = (self.routes)(state);
        ctx.router.mount("/", user_router);
        schedule_serve(cfg, ctx)
    }
}

/// Register the serve loop for `App::run`. Shared by [`WebPlugin`] and the
/// aide-aware [`ApiPlugin`].
#[cfg(not(target_family = "wasm"))]
fn schedule_serve(cfg: WebConfig, ctx: &mut BuildCtx) -> Result<()> {
    serve::schedule(cfg, ctx)
}

/// On wasm there is no listener to bind: the host (e.g. a Cloudflare Worker's
/// `fetch` handler) drives the router from `AppBuilder::into_router`, so only
/// the routes are mounted.
#[cfg(target_family = "wasm")]
fn schedule_serve(_cfg: WebConfig, _ctx: &mut BuildCtx) -> Result<()> {
    Ok(())
}

/// The aide-aware web plugin: like [`WebPlugin`], but its routes callback
/// returns an [`aide::axum::ApiRouter`] instead of a plain `axum::Router`.
///
/// At build it **finishes** the `ApiRouter` into routes + an
/// [`OpenApi`](aide::openapi::OpenApi) document (seeded by the value passed to
/// [`ApiPlugin::new`] — set `info.title`/`info.version` there), mounts the
/// routes, and serves the spec + Scalar docs at the paths from the `[openapi]`
/// config section. Owns the serve loop, so add it **last**, in place of
/// `WebPlugin` + a separate `OpenApiPlugin` — the two are alternatives, never
/// add both.
///
/// ```ignore
/// use muxa::prelude::*;
/// use muxa::aide::{axum::ApiRouter, openapi::OpenApi};
///
/// let openapi = OpenApi::default(); // set info.title / version as needed
/// App::default()
///     .with_plugin(ApiPlugin::new(move |_s| my_api_router(), openapi)).await?
///     .run().await
/// ```
#[cfg(feature = "openapi")]
pub struct ApiPlugin<R> {
    routes: R,
    api: aide::openapi::OpenApi,
}

#[cfg(feature = "openapi")]
impl<R> ApiPlugin<R> {
    /// Construct from an `ApiRouter`-producing callback and a seed OpenAPI
    /// document (carries `info.title`/`info.version`; the routes are filled in
    /// at build).
    pub fn new(routes: R, api: aide::openapi::OpenApi) -> Self {
        Self { routes, api }
    }
}

/// Configuration for [`ApiPlugin`]. It serves the app *and* its API docs, so
/// its configuration is two sections: `[web]` and `[openapi]`.
#[cfg(feature = "openapi")]
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    /// The `[web]` section: bind address, banner.
    pub web: WebConfig,
    /// The `[openapi]` section: where the spec and the docs are served.
    pub openapi: muxa_openapi::OpenApiConfig,
}

#[cfg(feature = "openapi")]
impl<S, R> Plugin<S> for ApiPlugin<R>
where
    S: State,
    R: FnOnce(&S) -> aide::axum::ApiRouter + Send + 'static,
{
    type Output = ();
    type Config = ApiConfig;
    const CONFIG_PREFIX: &'static str = "web";

    /// Two sections make up this plugin's config; an invalid value in either
    /// fails the build.
    fn read_config(sections: &Sections<'_>) -> Result<ApiConfig> {
        Ok(ApiConfig {
            web: sections.get("web")?,
            openapi: sections.get("openapi")?,
        })
    }

    async fn build(self, cfg: ApiConfig, state: &S, ctx: &mut BuildCtx) -> Result<()> {
        // Finish the aide router → axum Router, populating the OpenAPI document.
        let mut api = self.api;
        let router = (self.routes)(state).finish_api(&mut api);
        ctx.router.mount("/", router);

        // Serve the spec + Scalar docs (paths/title from the `[openapi]` table).
        ctx.router
            .mount("/", muxa_openapi::docs_router(&api, &cfg.openapi)?);

        schedule_serve(cfg.web, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_config_defaults() {
        let config = WebConfig::default();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn web_config_builder() {
        let config = WebConfig::builder()
            .host("127.0.0.1".into())
            .port(8080)
            .build();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8080);
    }
}
