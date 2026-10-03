//! muxa-worker — run a muxa app on Cloudflare Workers.
//!
//! A Worker has no listener to bind and no long-lived runtime: the platform
//! hands each request to a `fetch` event. So instead of `WebPlugin` +
//! `App::run`, a Worker app ends its chain with [`WorkerPlugin`] and
//! finishes with [`AppBuilder::into_router`](muxa_core::AppBuilder::into_router);
//! [`dispatch`] then drives each request through the composed router, which
//! is built once per isolate and cached.
//!
//! ```ignore
//! use muxa::prelude::*;
//! use muxa::worker::{WorkerVars, dispatch};
//! use worker::{Context, Env, HttpRequest, event};
//!
//! async fn build(env: Env) -> muxa::Result<axum::Router> {
//!     let figment = figment::Figment::from(WorkerVars::from_env(&env).prefixed("MUXA_"));
//!     let (router, _state) = App::with_figment(figment)
//!         .with_plugin(WorkerPlugin::new(routes)).await?
//!         .into_router()?;
//!     Ok(router)
//! }
//!
//! #[event(fetch)]
//! async fn fetch(
//!     req: HttpRequest,
//!     env: Env,
//!     _ctx: Context,
//! ) -> worker::Result<axum::http::Response<axum::body::Body>> {
//!     dispatch(req, env, build).await
//! }
//! ```
//!
//! Handlers reach the per-request [`worker::Env`] (D1 bindings, vars,
//! secrets) with the [`WorkerEnv`] extractor. Bindings are `!Send` JS
//! handles while axum handlers must return `Send` futures — mark handlers
//! that await a binding with `#[worker::send]`.
//!
//! Background tasks don't exist on Workers: a plugin that registers one makes
//! `into_router` fail. Use Cron Triggers (`#[event(scheduled)]`) instead.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod console;
mod vars;

use core::cell::RefCell;
use core::future::Future;
use core::ops::Deref;
use std::error::Error as StdError;

use axum::Router;
use axum::extract::FromRequestParts;
use http::StatusCode;
use http::request::Parts;
use muxa_core::{BuildCtx, Plugin, Result, State};
use tower::Service as _;
use worker::{Env, HttpRequest};

pub use crate::console::ConsoleLayer;
pub use crate::vars::WorkerVars;

/// The Workers equivalent of `WebPlugin`: mounts the application routes but
/// registers **no serve function** — the platform delivers requests, and
/// [`dispatch`] feeds them to the router.
///
/// Generic over a `routes` callback that receives the application state HList
/// and returns an `axum::Router`. Add it **last** in the chain so the state
/// passed to `routes` contains every other plugin's resource, then finish
/// with [`AppBuilder::into_router`](muxa_core::AppBuilder::into_router).
///
/// It also attaches a [`ConsoleLayer`] to the shared tracing subscriber, so
/// `tracing` events show up in `wrangler tail` and the Workers dashboard.
pub struct WorkerPlugin<R> {
    routes: R,
}

impl<R> WorkerPlugin<R> {
    /// Construct a `WorkerPlugin` with the given routes function.
    pub fn new(routes: R) -> Self {
        Self { routes }
    }
}

impl<S, R> Plugin<S> for WorkerPlugin<R>
where
    S: State,
    R: FnOnce(&S) -> Router + Send + 'static,
{
    type Output = ();
    type Config = ();
    const CONFIG_PREFIX: &'static str = "";

    async fn build(self, _cfg: (), state: &S, ctx: &mut BuildCtx) -> Result<()> {
        ctx.telemetry.add_layer(ConsoleLayer);
        ctx.router.mount("/", (self.routes)(state));
        Ok(())
    }
}

/// The per-request Worker [`Env`] — D1/KV/R2 bindings, vars and secrets.
///
/// [`dispatch`] puts it into the request extensions; handlers take it as an
/// extractor. It derefs to [`Env`], so `env.d1("DB")`, `env.var("NAME")` and
/// `env.secret("NAME")` are available directly.
///
/// ```ignore
/// #[worker::send]
/// async fn handler(env: WorkerEnv) -> Result<String, StatusCode> {
///     let db = env.d1("DB").map_err(|_err| StatusCode::INTERNAL_SERVER_ERROR)?;
///     // ...
/// }
/// ```
#[derive(Debug, Clone)]
pub struct WorkerEnv(pub Env);

impl Deref for WorkerEnv {
    type Target = Env;

    fn deref(&self) -> &Env {
        &self.0
    }
}

impl<S> FromRequestParts<S> for WorkerEnv
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "no Worker Env in request extensions — was the request routed through muxa_worker::dispatch?",
        ))
    }
}

thread_local! {
    /// The composed router, built by the first request an isolate serves and
    /// reused by every later one. Workers run each isolate on one thread.
    static ROUTER: RefCell<Option<Router>> = const { RefCell::new(None) };
}

/// Dispatch a Worker `fetch` request through the muxa app's router.
///
/// `build` runs the plugin chain and returns the composed router (see the
/// [crate docs](crate) for the usual shape). It is called by the first
/// request an isolate serves; the router is then cached for the isolate's
/// lifetime, so plugin builds and config parsing are not repeated per
/// request. Vars and secrets are fixed per deployment, so reading config
/// from the first request's `Env` is sound.
///
/// The request's [`Env`] is inserted into its extensions as [`WorkerEnv`]
/// before routing.
///
/// # Errors
///
/// Returns the error of `build` (as `worker::Error::RustError`, with its
/// source chain) when the app fails to build; nothing is cached in that case,
/// so the next request retries.
pub async fn dispatch<B, Fut>(
    mut req: HttpRequest,
    env: Env,
    build: B,
) -> worker::Result<http::Response<axum::body::Body>>
where
    B: FnOnce(Env) -> Fut,
    Fut: Future<Output = Result<Router>>,
{
    let cached = ROUTER.with(|slot| slot.borrow().clone());
    let mut router = match cached {
        Some(router) => router,
        None => {
            let router = build(env.clone())
                .await
                .map_err(|err| worker::Error::RustError(error_chain(&err)))?;
            ROUTER.with(|slot| *slot.borrow_mut() = Some(router.clone()));
            router
        }
    };

    req.extensions_mut().insert(WorkerEnv(env));
    match router.call(req).await {
        Ok(response) => Ok(response),
        Err(never) => match never {},
    }
}

/// Render an error with its full source chain (`a: b: c`).
fn error_chain(err: &dyn StdError) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_chain_joins_sources() {
        let err = muxa_core::Error::plugin_build::<(), _>("inner cause");
        let chain = error_chain(&err);
        assert!(
            chain.contains("failed during build: inner cause"),
            "{chain}"
        );
    }

    #[tokio::test]
    async fn worker_plugin_mounts_routes_without_serve_fn() {
        fn routes<S>(_state: &S) -> Router {
            Router::new().route("/", axum::routing::get(|| async { "hi" }))
        }

        let app = muxa_core::App::with_figment(figment::Figment::new())
            .with_plugin(WorkerPlugin::new(routes))
            .await
            .unwrap();
        // No serve fn registered: into_router succeeds.
        assert!(app.into_router().is_ok());
    }
}
