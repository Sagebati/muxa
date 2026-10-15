//! muxa-web — the `WebPlugin` for muxa.
//!
//! `WebPlugin` owns the axum `serve` loop and the graceful-shutdown handshake.
//! It is added **last** in the plugin chain by convention: its `build` method
//! invokes the user-supplied routes function against the current state
//! (which by that point holds every other plugin's resource), composes it
//! with each plugin's auto-mounted tower services and middleware, and
//! schedules the actual serve loop for `App::run`.
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
//!     muxa::app!()
//!         .with_plugin(WebPlugin::new(my_routes)).await?
//!         .run().await
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod banner;
#[cfg(feature = "ratelimit")]
pub mod ratelimit;

use std::net::SocketAddr;

use axum::Router;
use bon::Builder;
#[cfg(feature = "openapi")]
use muxa_core::Sections;
use muxa_core::{BoxFuture, BuildCtx, Plugin, Result, ServeFn, ShutdownToken, State};
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

/// Snapshot the banner data and register the serve loop on the build context.
/// Shared by [`WebPlugin`] and the aide-aware [`ApiPlugin`].
fn schedule_serve(cfg: WebConfig, ctx: &mut BuildCtx) -> Result<()> {
    // Render the banner's routes now: the serve closure runs after
    // `RouterRegistry` is consumed at `compose()` time.
    let banner_routes = cfg.banner.then(|| banner::routes(&ctx.router));

    let shutdown = ctx.shutdown.clone();
    let serve_fn: ServeFn = Box::new(move |router: Router| -> BoxFuture<'static, Result<()>> {
        Box::pin(serve_loop(router, cfg, banner_routes, shutdown))
    });
    ctx.set_serve_fn(serve_fn)
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
/// muxa::app!()
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

async fn serve_loop(
    router: Router,
    cfg: WebConfig,
    banner_routes: Option<String>,
    shutdown: ShutdownToken,
) -> Result<()> {
    // `(host, port)` goes through `ToSocketAddrs`: an IP literal is parsed
    // without a lookup, a hostname is resolved.
    let listener = tokio::net::TcpListener::bind((bind_host(&cfg.host), cfg.port)).await?;
    let bound = listener.local_addr()?;

    if let Some(routes) = banner_routes {
        banner::print(bound, &routes);
    }

    tracing::info!(%bound, "muxa-web: serving");

    // Bridge ctrl_c to the shutdown token (additive — anyone else can also
    // cancel it). Only compiled with the `graceful-shutdown` feature, which
    // also enables `tokio/signal`.
    #[cfg(feature = "graceful-shutdown")]
    {
        let signal_token = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("ctrl-c received; signaling shutdown");
                signal_token.cancel();
            }
        });
    }

    let graceful = shutdown.clone();
    // Serve with per-connection `ConnectInfo<SocketAddr>` so handlers and tower
    // layers (e.g. the per-IP rate limiter) can read the client's peer address.
    // Additive and cheap — handlers that don't need it are unaffected.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { graceful.cancelled().await })
    .await?;

    tracing::info!("muxa-web: serve loop ended");
    Ok(())
}

/// `host` without the brackets of a URL-style IPv6 literal (`[::1]` → `::1`),
/// which `ToSocketAddrs` doesn't take.
fn bind_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
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

    #[test]
    fn bind_host_strips_ipv6_brackets_only() {
        assert_eq!(bind_host("[::1]"), "::1");
        assert_eq!(bind_host("[fe80::1%2]"), "fe80::1%2");
        assert_eq!(bind_host("::1"), "::1");
        assert_eq!(bind_host("0.0.0.0"), "0.0.0.0");
        assert_eq!(bind_host("localhost"), "localhost");
    }

    /// The forms the old `"{host}:{port}"` parse took still bind, and hostnames
    /// now do too.
    #[tokio::test]
    async fn binds_ip_literals_and_hostnames() {
        for host in ["127.0.0.1", "localhost"] {
            let listener = tokio::net::TcpListener::bind((bind_host(host), 0)).await;
            assert!(listener.is_ok(), "{host}: {listener:?}");
        }
    }
}
