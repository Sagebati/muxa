//! Build-time side channels: router registry, task registry, shutdown token,
//! and the serve-function slot the web plugin fills.

use core::future::Future;
use core::pin::Pin;

use tokio::task::JoinHandle;
use tracing::Instrument as _;

use crate::error::Result;

/// Cancellation token used to coordinate graceful shutdown across the app.
pub type ShutdownToken = tokio_util::sync::CancellationToken;

/// Type-erased async return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The serve loop the web plugin schedules. Invoked by [`crate::App::run`]
/// with the final composed router.
pub type ServeFn = Box<dyn FnOnce(axum::Router) -> BoxFuture<'static, Result<()>> + Send>;

/// A registered background task: spawns its (concrete, unboxed) future on the
/// runtime when handed its [`ShutdownToken`].
type SpawnTask = Box<dyn FnOnce(ShutdownToken) -> JoinHandle<()> + Send + 'static>;

/// Per-application build context — passed to every plugin's `build()`. Holds
/// the router and task registries, the shutdown token, the telemetry kernel,
/// the run mode, and a slot for the serving plugin's serve closure.
///
/// The configuration is deliberately **not** reachable from here: a plugin
/// gets its own section as the `cfg` argument of `build` (see
/// [`Plugin::read_config`](crate::Plugin::read_config)), and only the
/// application reads the whole thing, via
/// [`AppBuilder::figment`](crate::AppBuilder::figment).
pub struct BuildCtx {
    figment: figment::Figment,
    /// Coarse run mode (development vs production), resolved once from the
    /// figment's `env` key (or the build profile). Plugins branch on this for
    /// environment-aware defaults instead of each re-deriving it.
    pub mode: crate::RunMode,
    /// Routes and middleware queued for the final axum router.
    pub router: RouterRegistry,
    /// Background tasks to spawn at [`crate::App::run`].
    pub tasks: TaskRegistry,
    /// Cancellation token signaling graceful shutdown.
    pub shutdown: ShutdownToken,
    /// Tracing-subscriber registry. Plugins push their tracing layers
    /// (`tracing-opentelemetry`, `sentry-tracing`, …) here; they're attached
    /// to the global subscriber immediately via a reload handle.
    pub telemetry: muxa_telemetry::TelemetryRegistry,
    pub(crate) serve_fn: Option<ServeFn>,
}

impl BuildCtx {
    pub(crate) fn new(figment: figment::Figment) -> Self {
        let mode = crate::RunMode::from_figment(&figment);
        Self {
            figment,
            mode,
            router: RouterRegistry::default(),
            tasks: TaskRegistry::default(),
            shutdown: ShutdownToken::new(),
            telemetry: muxa_telemetry::TelemetryRegistry::install(),
            serve_fn: None,
        }
    }

    /// The merged configuration. Crate-private on purpose — see the type docs.
    pub(crate) fn figment(&self) -> &figment::Figment {
        &self.figment
    }

    /// Set the serve function. Called by the web plugin during `build`.
    /// Returns an error if a serve function is already registered.
    pub fn set_serve_fn(&mut self, serve_fn: ServeFn) -> Result<()> {
        if self.serve_fn.is_some() {
            return Err(crate::Error::other(
                "another plugin already registered a serve_fn",
            ));
        }
        self.serve_fn = Some(serve_fn);
        Ok(())
    }
}

/// How a plugin's router is composed into the final app router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    /// Automatically merged at compose-time. Default for plugin-owned routes.
    Auto,
    /// Skipped at compose-time — the user must opt in by calling
    /// [`RouterRegistry::take_manual`] from their own router builder.
    Manual,
}

/// Registry of plugin-contributed tower services (axum routers) and middleware.
#[derive(Default)]
pub struct RouterRegistry {
    routes: Vec<MountEntry>,
    fallback: Option<axum::Router>,
    layer_fns: Vec<Box<dyn FnOnce(axum::Router) -> axum::Router + Send + 'static>>,
}

struct MountEntry {
    prefix: String,
    router: axum::Router,
    mount: Mount,
}

impl RouterRegistry {
    /// Mount a tower service (axum router) under `prefix`. The router must have
    /// its state already baked in (call `.with_state(...)` if needed before
    /// passing it here). Defaults to [`Mount::Auto`].
    pub fn mount(&mut self, prefix: impl Into<String>, router: axum::Router) {
        self.routes.push(MountEntry {
            prefix: prefix.into(),
            router,
            mount: Mount::Auto,
        });
    }

    /// Mount a router under `prefix` with [`Mount::Manual`] — it won't be
    /// composed automatically; the user must opt in.
    pub fn mount_manual(&mut self, prefix: impl Into<String>, router: axum::Router) {
        self.routes.push(MountEntry {
            prefix: prefix.into(),
            router,
            mount: Mount::Manual,
        });
    }

    /// Set the app-wide fallback: the router that handles every request no
    /// mounted route matched (a single-page app's `index.html`, a custom 404).
    ///
    /// It sees the full, unstripped request URI. Nested routers without a
    /// fallback of their own fall through to it too, so `/api/nope` reaches it
    /// when `/api` is mounted — a fallback that must not answer for such paths
    /// has to check the prefix itself. Applied at [`compose`](Self::compose)
    /// after every mount and before the layers, so registration order doesn't
    /// matter and middleware wraps it like any route.
    ///
    /// An app has one fallback: setting it a second time is an error.
    pub fn fallback(&mut self, router: axum::Router) -> Result<()> {
        if self.fallback.is_some() {
            return Err(crate::Error::other(
                "another plugin already registered a fallback router",
            ));
        }
        self.fallback = Some(router);
        Ok(())
    }

    /// `true` once a [`fallback`](Self::fallback) is registered.
    pub fn has_fallback(&self) -> bool {
        self.fallback.is_some()
    }

    /// Register a middleware/layer that wraps the final composed router.
    ///
    /// Layers are applied in registration order, each one wrapping everything
    /// registered before it. So the **last** registered layer is the outermost:
    /// it sees a request first and the response last. Since plugins register
    /// during `build`, middleware order follows plugin order — a plugin added
    /// later in the chain wraps the ones added earlier.
    pub fn layer<F>(&mut self, func: F)
    where
        F: FnOnce(axum::Router) -> axum::Router + Send + 'static,
    {
        self.layer_fns.push(Box::new(func));
    }

    /// Iterate over the registered mount points (prefix + mount mode), in
    /// registration order, without consuming the registry. Used by the web
    /// plugin to print a launch banner.
    pub fn mounts(&self) -> impl Iterator<Item = (&str, Mount)> {
        self.routes
            .iter()
            .map(|entry| (entry.prefix.as_str(), entry.mount))
    }

    /// Remove all `Manual` entries matching `prefix`, returning their routers
    /// for the user to nest themselves. Useful from a custom routes function.
    pub fn take_manual(&mut self, prefix: &str) -> Vec<axum::Router> {
        self.routes
            .extract_if(.., |entry| {
                entry.mount == Mount::Manual && entry.prefix == prefix
            })
            .map(|entry| entry.router)
            .collect()
    }

    /// Compose all `Auto`-mounted routers, then the [`fallback`](Self::fallback)
    /// if any, then apply middleware in registration order (see
    /// [`layer`](Self::layer): last registered is outermost).
    pub fn compose(self) -> axum::Router {
        let mut out = axum::Router::new();
        for entry in self.routes {
            if entry.mount == Mount::Auto {
                if entry.prefix.is_empty() || entry.prefix == "/" {
                    out = out.merge(entry.router);
                } else {
                    out = out.nest(&entry.prefix, entry.router);
                }
            }
        }
        if let Some(fallback) = self.fallback {
            out = out.fallback_service(fallback);
        }
        for layer in self.layer_fns {
            out = layer(out);
        }
        out
    }
}

/// Registry of background tasks; spawned on the runtime at
/// [`crate::App::run`]. Each task gets a [`ShutdownToken`]; once serving ends
/// the token is cancelled and `run` waits (for a bounded time) for the tasks
/// to return, so a task can use it to flush or drain before the process exits.
#[derive(Default)]
pub struct TaskRegistry {
    tasks: Vec<(&'static str, SpawnTask)>,
}

impl TaskRegistry {
    /// Schedule a background task. `name` is used for tracing spans only.
    pub fn spawn<F, Fut>(&mut self, name: &'static str, func: F)
    where
        F: FnOnce(ShutdownToken) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Only the closure is boxed: the future itself goes straight into the
        // runtime's task, unboxed.
        let spawn: SpawnTask = Box::new(move |shutdown| {
            tokio::spawn(
                async move {
                    tracing::info!(task = name, "spawning background task");
                    func(shutdown).await;
                }
                .instrument(tracing::info_span!("muxa.task", name = name)),
            )
        });
        self.tasks.push((name, spawn));
    }

    /// Number of scheduled tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// `true` if no task is scheduled.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Names of the scheduled tasks, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        self.tasks.iter().map(|(name, _)| *name)
    }

    /// Spawn every scheduled task on the current Tokio runtime, each with a
    /// child of `shutdown`, and return their handles.
    ///
    /// [`App::run`](crate::App::run) does this. To drive the tasks yourself
    /// instead, take them out of the context first:
    /// `std::mem::take(&mut ctx.tasks).spawn_all(&token)`.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, like `tokio::spawn`.
    pub fn spawn_all(self, shutdown: &ShutdownToken) -> Vec<(&'static str, JoinHandle<()>)> {
        self.tasks
            .into_iter()
            .map(|(name, spawn)| (name, spawn(shutdown.child_token())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::Request;
    use axum::middleware::{self, Next};
    use tower::ServiceExt as _;

    use super::*;

    /// A layer that records `name` when a request passes through it.
    fn recording(
        name: &'static str,
        seen: &Arc<Mutex<Vec<&'static str>>>,
    ) -> impl FnOnce(axum::Router) -> axum::Router + Send + 'static {
        let seen = Arc::clone(seen);
        move |router| {
            router.layer(middleware::from_fn(move |request: Request, next: Next| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.lock().unwrap().push(name);
                    next.run(request).await
                }
            }))
        }
    }

    #[tokio::test]
    async fn last_registered_layer_is_outermost() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut registry = RouterRegistry::default();
        registry.mount(
            "/",
            axum::Router::new().route("/", axum::routing::get(|| async { "ok" })),
        );
        registry.layer(recording("first", &seen));
        registry.layer(recording("second", &seen));

        let request = http::Request::get("/")
            .body(axum::body::Body::empty())
            .unwrap();
        registry.compose().oneshot(request).await.unwrap();

        // The request meets the last-registered layer first.
        assert_eq!(*seen.lock().unwrap(), ["second", "first"]);
    }

    async fn body_of(router: axum::Router, uri: &str) -> String {
        let request = http::Request::get(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn fallback_catches_what_no_mount_matched() {
        let mut registry = RouterRegistry::default();
        // Registered before the mounts: compose applies it after them anyway.
        registry
            .fallback(
                axum::Router::new()
                    .fallback(|uri: http::Uri| async move { format!("fallback {uri}") }),
            )
            .unwrap();
        registry.mount(
            "/api",
            axum::Router::new().route("/thing", axum::routing::get(|| async { "thing" })),
        );
        assert!(registry.has_fallback());
        let router = registry.compose();

        assert_eq!(body_of(router.clone(), "/api/thing").await, "thing");
        // The nested router has no fallback of its own: the app's sees the
        // full URI.
        assert_eq!(
            body_of(router.clone(), "/api/nope").await,
            "fallback /api/nope"
        );
        assert_eq!(body_of(router, "/games/1").await, "fallback /games/1");
    }

    #[test]
    fn a_second_fallback_is_an_error() {
        let mut registry = RouterRegistry::default();
        assert!(!registry.has_fallback());
        registry.fallback(axum::Router::new()).unwrap();
        assert!(registry.fallback(axum::Router::new()).is_err());
    }

    #[tokio::test]
    async fn scheduled_tasks_can_be_inspected_and_driven() {
        let mut tasks = TaskRegistry::default();
        assert!(tasks.is_empty());
        tasks.spawn(
            "first",
            |shutdown| async move { shutdown.cancelled().await },
        );
        tasks.spawn("second", |_shutdown| async {});
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks.names().collect::<Vec<_>>(), ["first", "second"]);

        let shutdown = ShutdownToken::new();
        let handles = tasks.spawn_all(&shutdown);
        shutdown.cancel();
        for (_, handle) in handles {
            handle.await.unwrap();
        }
    }

    #[test]
    fn take_manual_removes_only_matching_manual_mounts() {
        let mut registry = RouterRegistry::default();
        registry.mount("/api", axum::Router::new());
        registry.mount_manual("/admin", axum::Router::new());
        registry.mount_manual("/other", axum::Router::new());
        registry.mount_manual("/admin", axum::Router::new());
        // Same prefix, but auto-mounted: stays.
        registry.mount("/admin", axum::Router::new());

        assert_eq!(registry.take_manual("/admin").len(), 2);
        assert!(registry.take_manual("/admin").is_empty());

        let remaining: Vec<_> = registry.mounts().collect();
        assert_eq!(
            remaining,
            [
                ("/api", Mount::Auto),
                ("/other", Mount::Manual),
                ("/admin", Mount::Auto),
            ]
        );
    }
}
