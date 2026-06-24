//! Build-time side channels: router registry, task registry, shutdown token,
//! and the serve-function slot the web plugin fills.

use core::future::Future;
use core::pin::Pin;

use crate::error::Result;

/// Cancellation token used to coordinate graceful shutdown across the app.
pub type ShutdownToken = tokio_util::sync::CancellationToken;

/// Type-erased async return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The serve loop the web plugin schedules. Invoked by [`crate::App::run`]
/// with the final composed router.
pub type ServeFn = Box<dyn FnOnce(axum::Router) -> BoxFuture<'static, Result<()>> + Send>;

/// A type-erased background task — gets handed a [`ShutdownToken`] and returns
/// a future the runtime drives on `tokio::spawn`.
pub type BoxTask = Box<dyn FnOnce(ShutdownToken) -> BoxFuture<'static, ()> + Send + 'static>;

/// Per-application build context — passed to every plugin's `build()`. Holds
/// the figment (read-only after `App::default()`), the router and task registries,
/// the shutdown token, the telemetry kernel, and a slot for the web plugin's
/// serve closure.
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

    /// Borrow the configured figment.
    pub fn figment(&self) -> &figment::Figment {
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
#[derive(Debug, Clone)]
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

    /// Register a middleware/layer that wraps the final composed router.
    /// Pushed-in-order; outermost layer first.
    pub fn layer<F>(&mut self, func: F)
    where
        F: FnOnce(axum::Router) -> axum::Router + Send + 'static,
    {
        self.layer_fns.push(Box::new(func));
    }

    /// Snapshot the registered mount points (prefix + mount mode) without
    /// consuming the registry. Used by the web plugin to print a launch
    /// banner.
    pub fn mounts(&self) -> Vec<(String, Mount)> {
        self.routes
            .iter()
            .map(|entry| (entry.prefix.clone(), entry.mount.clone()))
            .collect()
    }

    /// Drain all `Manual` entries matching `prefix`, returning their routers
    /// for the user to nest themselves. Useful from a custom routes function.
    pub fn take_manual(&mut self, prefix: &str) -> Vec<axum::Router> {
        let mut taken = Vec::new();
        let mut keep = Vec::with_capacity(self.routes.len());
        for entry in std::mem::take(&mut self.routes) {
            if matches!(entry.mount, Mount::Manual) && entry.prefix == prefix {
                taken.push(entry.router);
            } else {
                keep.push(entry);
            }
        }
        self.routes = keep;
        taken
    }

    /// Compose all `Auto`-mounted routers, then apply middleware in
    /// registration order.
    pub fn compose(self) -> axum::Router {
        let mut out = axum::Router::new();
        for entry in self.routes {
            if matches!(entry.mount, Mount::Auto) {
                if entry.prefix.is_empty() || entry.prefix == "/" {
                    out = out.merge(entry.router);
                } else {
                    out = out.nest(&entry.prefix, entry.router);
                }
            }
        }
        for layer in self.layer_fns {
            out = layer(out);
        }
        out
    }
}

/// Registry of background tasks; drained at [`crate::App::run`] into
/// `tokio::spawn` calls.
#[derive(Default)]
pub struct TaskRegistry {
    tasks: Vec<(&'static str, BoxTask)>,
}

impl TaskRegistry {
    /// Schedule a background task. `name` is used for tracing spans only.
    pub fn spawn<F, Fut>(&mut self, name: &'static str, func: F)
    where
        F: FnOnce(ShutdownToken) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.tasks
            .push((name, Box::new(|shutdown| Box::pin(func(shutdown)))));
    }

    /// Drain all scheduled tasks for spawning.
    pub fn drain(&mut self) -> Vec<(&'static str, BoxTask)> {
        std::mem::take(&mut self.tasks)
    }
}
