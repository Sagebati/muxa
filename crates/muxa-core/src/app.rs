//! `App` and `AppBuilder` — the entry point and plugin-chain accumulator.

use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use figment::Figment;
#[cfg(not(target_family = "wasm"))]
use tokio::task::JoinHandle;

use crate::config::Sections;
use crate::ctx::BuildCtx;
use crate::error::{Error, Result};
use crate::plugin::Plugin;
use crate::state::{HNil, State};

/// How long [`AppBuilder::run`] waits for background tasks to return once
/// serving has ended and the shutdown token is cancelled. Long enough for a
/// telemetry flush or a queue drain; a task still running after that is
/// abandoned (and logged) so the process can exit.
#[cfg(not(target_family = "wasm"))]
const TASK_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Entry point — a friendly alias for an empty [`AppBuilder`].
///
/// Construct with `App::default()` (uses the default figment lookup),
/// `App::with_config_file(path)`, or `App::with_figment(fig)`.
pub type App = AppBuilder<HNil>;

/// Application builder.
///
/// The type parameter `S` is the HList of plugin outputs accumulated so far.
/// Each [`AppBuilder::with_plugin`] call returns an `AppBuilder` whose `S`
/// grows by one entry.
pub struct AppBuilder<S: State> {
    state: S,
    ctx: BuildCtx,
}

impl AppBuilder<HNil> {
    /// Create an empty builder from a figment.
    pub fn with_figment(figment: Figment) -> Self {
        Self {
            state: HNil,
            ctx: BuildCtx::new(figment),
        }
    }

    /// Create an empty builder loading config from an explicit file path.
    /// Useful when you want to ship `muxa.toml` next to a binary, in a
    /// system path, etc.
    pub fn with_config_file<P: AsRef<Path>>(path: P) -> Self {
        Self::with_figment(crate::config::load_figment_from(path))
    }

    /// Like [`AppBuilder::default`], but reads env vars with a custom prefix
    /// instead of `MUXA_`. The bootstrap config-path var becomes
    /// `{prefix}CONFIG` (e.g. prefix `MYAPP_` → `$MYAPP_CONFIG`). The prefix
    /// should include its trailing separator.
    pub fn with_env_prefix(prefix: &str) -> Self {
        Self::with_figment(crate::config::load_figment_with_prefix(prefix))
    }

    /// Like [`AppBuilder::with_config_file`], but reads env vars with a custom
    /// prefix instead of `MUXA_`. The prefix should include its trailing
    /// separator.
    pub fn with_config_file_and_env_prefix<P: AsRef<Path>>(path: P, prefix: &str) -> Self {
        Self::with_figment(crate::config::load_figment_from_with_prefix(path, prefix))
    }
}

impl Default for AppBuilder<HNil> {
    /// Equivalent to `AppBuilder::with_figment(load_figment())` — uses the
    /// default figment lookup (`./muxa.toml`, overridable via
    /// `MUXA_CONFIG=path/to/file.toml`, merged with `MUXA_*` env vars).
    fn default() -> Self {
        Self::with_figment(crate::config::load_figment())
    }
}

impl<S: State> AppBuilder<S> {
    /// Borrow the current state HList. Useful for ad-hoc inspection in tests.
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Borrow the merged configuration.
    ///
    /// The application owns the configuration and may read any of it — e.g.
    /// `app.figment().extract_inner::<MyConfig>("my_section")` for settings
    /// that don't belong to a plugin. Plugins can't: they only receive their
    /// own section (see [`Plugin::read_config`]).
    pub fn figment(&self) -> &Figment {
        self.ctx.figment()
    }

    /// Borrow the build context.
    pub fn ctx(&self) -> &BuildCtx {
        &self.ctx
    }

    /// Mutably borrow the build context.
    pub fn ctx_mut(&mut self) -> &mut BuildCtx {
        &mut self.ctx
    }

    /// Add a plugin to the chain. Returns a new builder whose state has been
    /// extended with the plugin's output.
    pub async fn with_plugin<P>(mut self, plugin: P) -> Result<AppBuilder<S::Push<P::Output>>>
    where
        P: Plugin<S>,
    {
        let cfg = P::read_config(&Sections::new(self.ctx.figment()))?;
        let out = plugin
            .build(cfg, &self.state, &mut self.ctx)
            .await
            .map_err(|err| match err {
                // Wrap raw errors as PluginBuild for context, but pass through
                // pre-tagged ones (so nested plugin failures bubble cleanly).
                Error::PluginBuild { .. } => err,
                other => Error::PluginBuild {
                    plugin: std::any::type_name::<P>(),
                    source: Box::new(other),
                },
            })?;
        Ok(AppBuilder {
            state: self.state.push(out),
            ctx: self.ctx,
        })
    }

    /// Finish the app **without serving**: compose the router and hand it
    /// back together with the final state HList.
    ///
    /// This is the terminal step for hosts that drive the router themselves
    /// instead of running a serve loop — Cloudflare Workers (see
    /// `muxa-worker`), serverless adapters, or tests calling the router as a
    /// `tower::Service`.
    ///
    /// Such hosts have nowhere to run background tasks, so tasks registered
    /// via [`TaskRegistry::spawn`](crate::TaskRegistry::spawn) are an
    /// **error** here rather than being silently dropped. A registered serve
    /// function (a `WebPlugin` in the chain) is an error for the same reason:
    /// it would never be invoked. So is a chain that mounted no routes — every
    /// request would be a 404 — which usually means the routing plugin (e.g.
    /// `WorkerPlugin`) is missing.
    ///
    /// The shutdown token goes away with the build context, uncancelled. A
    /// caller that can run the tasks itself keeps a handle to the token,
    /// takes the tasks, and cancels the token when its host stops — that
    /// signals the tasks and anything else a plugin hooked on shutdown:
    ///
    /// ```ignore
    /// let shutdown = app.ctx().shutdown.clone();
    /// let tasks = std::mem::take(&mut app.ctx_mut().tasks).spawn_all(&shutdown);
    /// let (router, state) = app.into_router()?;
    /// // ... serve with `router` ...
    /// shutdown.cancel();
    /// ```
    pub fn into_router(self) -> Result<(axum::Router, S)> {
        if self.ctx.serve_fn.is_some() {
            return Err(Error::other(
                "into_router() called with a serve function registered — \
                 a serving plugin (WebPlugin) can't be combined with into_router()",
            ));
        }
        if !self.ctx.tasks.is_empty() {
            let registered = match self.ctx.tasks.len() {
                1 => "a background task is",
                _ => "background tasks are",
            };
            return Err(Error::other(format!(
                "into_router() can't run background tasks, but {registered} registered: {} — \
                 take them with `std::mem::take(&mut ctx_mut().tasks)` and run them yourself",
                self.ctx.tasks.names().collect::<Vec<_>>().join(", "),
            )));
        }
        if self.ctx.router.mounts().next().is_none() {
            return Err(Error::other(
                "into_router() found no routes — add a routing plugin (e.g. WorkerPlugin) \
                 as the last plugin in the chain",
            ));
        }
        Ok((self.ctx.router.compose(), self.state))
    }

    /// Freeze the state, spawn background tasks, compose the router, and run
    /// the registered serve function until shutdown.
    ///
    /// When the serve function returns, the shutdown token is cancelled and
    /// `run` waits up to ten seconds for the background tasks to return, so a
    /// task that flushes or drains on shutdown gets to finish.
    ///
    /// Not available on wasm targets — there is no tokio runtime to spawn
    /// onto; use [`AppBuilder::into_router`] there.
    #[cfg(not(target_family = "wasm"))]
    pub async fn run(mut self) -> Result<()> {
        let serve = self.ctx.serve_fn.take().ok_or_else(|| {
            Error::other(
                "no serve function registered — add a serving plugin (e.g. a web plugin) \
                 as the last plugin in the chain",
            )
        })?;

        let shutdown = self.ctx.shutdown.clone();
        let tasks = self.ctx.tasks.spawn_all(&shutdown);

        // The frozen state is reserved for future use (e.g. `Plugin::shutdown`
        // hooks) — for v0 we just keep it alive until run() returns.
        let _state = self.state;

        let router = self.ctx.router.compose();
        let served = serve(router).await;

        // Serving is over, whatever the reason: tell every task, then give
        // them a bounded window to return before the runtime is torn down.
        shutdown.cancel();
        join_tasks(tasks).await;

        served
    }
}

/// What became of the background tasks at shutdown, by name.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Default)]
struct TaskOutcomes {
    /// Tasks that panicked (or were cancelled).
    failed: Vec<&'static str>,
    /// Tasks still running when the grace period ran out.
    abandoned: Vec<&'static str>,
}

/// Wait for the background tasks to return, for at most
/// [`TASK_SHUTDOWN_GRACE`] in all. Logs tasks that panicked or didn't stop in
/// time.
#[cfg(not(target_family = "wasm"))]
async fn join_tasks(tasks: Vec<(&'static str, JoinHandle<()>)>) -> TaskOutcomes {
    // One deadline for all: the tasks run concurrently, so awaiting them in turn
    // still waits at most the grace period overall. A task that has already
    // returned yields its result even past the deadline (the handle is polled
    // before the timer), so its panic is reported rather than lost.
    let deadline = tokio::time::Instant::now() + TASK_SHUTDOWN_GRACE;
    let mut outcomes = TaskOutcomes::default();
    for (name, handle) in tasks {
        match tokio::time::timeout_at(deadline, handle).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!(task = name, %err, "background task failed");
                outcomes.failed.push(name);
            }
            Err(_elapsed) => outcomes.abandoned.push(name),
        }
    }
    if !outcomes.abandoned.is_empty() {
        tracing::warn!(
            tasks = ?outcomes.abandoned,
            grace_secs = TASK_SHUTDOWN_GRACE.as_secs(),
            "background tasks still running after the shutdown grace period; abandoning them"
        );
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::state::HCons;
    use figment::providers::Format as _;

    /// A trivial plugin that pushes an i32 onto the state.
    struct AnswerPlugin;
    impl<S: State> Plugin<S> for AnswerPlugin {
        type Output = i32;
        type Config = ();
        const CONFIG_PREFIX: &'static str = "";

        async fn build(self, _cfg: (), _state: &S, _ctx: &mut BuildCtx) -> Result<i32> {
            Ok(42)
        }
    }

    #[tokio::test]
    async fn empty_chain_builds() {
        let fig = Figment::new();
        let app = AppBuilder::<HNil>::with_figment(fig);
        assert!(matches!(app.state(), HNil));
    }

    #[tokio::test]
    async fn with_plugin_grows_state() {
        let fig = Figment::new();
        let app = AppBuilder::<HNil>::with_figment(fig)
            .with_plugin(AnswerPlugin)
            .await
            .unwrap();
        // After AnswerPlugin: state is HCons<i32, HNil>.
        let _: &HCons<i32, HNil> = app.state();
        assert_eq!(app.state().head, 42);
    }

    /// Registers a serve fn that returns at once and a task that records
    /// that it observed shutdown.
    struct FlushPlugin(Arc<std::sync::atomic::AtomicBool>);
    impl<S: State> Plugin<S> for FlushPlugin {
        type Output = ();
        type Config = ();
        const CONFIG_PREFIX: &'static str = "";

        async fn build(self, _cfg: (), _state: &S, ctx: &mut BuildCtx) -> Result<()> {
            let flushed = self.0;
            ctx.tasks.spawn("flush", move |shutdown| async move {
                shutdown.cancelled().await;
                // Yield so the flush only completes if `run` actually waits.
                tokio::task::yield_now().await;
                flushed.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            ctx.set_serve_fn(Box::new(|_router| Box::pin(async { Ok(()) })))
        }
    }

    #[tokio::test]
    async fn run_waits_for_tasks_after_serving_ends() {
        let flushed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(FlushPlugin(Arc::clone(&flushed)))
            .await
            .unwrap()
            .run()
            .await
            .unwrap();
        assert!(flushed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_reports_failed_and_abandoned_tasks() {
        let hangs = tokio::spawn(std::future::pending::<()>());
        let fails = tokio::spawn(async { panic!("flush failed") });
        let returns = tokio::spawn(async {});
        let outcomes = join_tasks(vec![
            ("hangs", hangs),
            ("fails", fails),
            ("returns", returns),
        ])
        .await;
        // `fails` is awaited after the deadline passed waiting on `hangs`, yet
        // its panic is still reported.
        assert_eq!(outcomes.failed, ["fails"]);
        assert_eq!(outcomes.abandoned, ["hangs"]);
    }

    /// A plugin that mounts one route and (optionally) registers a task.
    struct RoutePlugin {
        with_task: bool,
    }
    impl<S: State> Plugin<S> for RoutePlugin {
        type Output = ();
        type Config = ();
        const CONFIG_PREFIX: &'static str = "";

        async fn build(self, _cfg: (), _state: &S, ctx: &mut BuildCtx) -> Result<()> {
            ctx.router.mount(
                "/",
                axum::Router::new().route("/ping", axum::routing::get(|| async { "pong" })),
            );
            if self.with_task {
                ctx.tasks.spawn(
                    "ticker",
                    |shutdown| async move { shutdown.cancelled().await },
                );
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn into_router_returns_composed_router_and_state() {
        use tower::ServiceExt as _;

        let (router, state) = AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(AnswerPlugin)
            .await
            .unwrap()
            .with_plugin(RoutePlugin { with_task: false })
            .await
            .unwrap()
            .into_router()
            .unwrap();
        assert_eq!(state.tail.head, 42);

        let request = http::Request::get("/ping")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn into_router_rejects_registered_tasks() {
        let app = AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(RoutePlugin { with_task: true })
            .await
            .unwrap();
        let err = app.into_router().map(|_| ()).unwrap_err();
        assert!(
            err.to_string()
                .contains("a background task is registered: ticker"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn into_router_without_routes_is_an_error() {
        let err = AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(AnswerPlugin)
            .await
            .unwrap()
            .into_router()
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("no routes"), "{err}");
    }

    #[tokio::test]
    async fn into_router_accepts_taken_tasks() {
        let mut app = AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(RoutePlugin { with_task: true })
            .await
            .unwrap();
        let shutdown = app.ctx().shutdown.clone();
        let tasks = std::mem::take(&mut app.ctx_mut().tasks).spawn_all(&shutdown);
        assert_eq!(tasks.len(), 1);
        assert!(app.into_router().is_ok());
        // The kept token still reaches the tasks after the context is gone.
        shutdown.cancel();
        for (_, task) in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn run_without_a_serving_plugin_is_an_error() {
        let err = AppBuilder::<HNil>::with_figment(Figment::new())
            .run()
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no serve function"), "{err}");
    }

    #[tokio::test]
    async fn config_falls_back_to_default_when_prefix_absent() {
        use serde::Deserialize;

        #[derive(Deserialize, Default, Debug, PartialEq, Eq)]
        struct MyCfg {
            #[serde(default)]
            n: u32,
        }

        struct UsesCfg;
        impl<S: State> Plugin<S> for UsesCfg {
            type Output = u32;
            type Config = MyCfg;
            const CONFIG_PREFIX: &'static str = "absent_section";

            async fn build(self, cfg: MyCfg, _s: &S, _c: &mut BuildCtx) -> Result<u32> {
                Ok(cfg.n)
            }
        }

        let fig = Figment::new();
        let app = AppBuilder::<HNil>::with_figment(fig)
            .with_plugin(UsesCfg)
            .await
            .unwrap();
        assert_eq!(app.state().head, 0);
    }

    #[tokio::test]
    async fn config_extracted_when_prefix_present() {
        use serde::Deserialize;

        #[derive(Deserialize, Default, Debug)]
        struct MyCfg {
            n: u32,
        }

        struct UsesCfg;
        impl<S: State> Plugin<S> for UsesCfg {
            type Output = u32;
            type Config = MyCfg;
            const CONFIG_PREFIX: &'static str = "cfg";

            async fn build(self, cfg: MyCfg, _s: &S, _c: &mut BuildCtx) -> Result<u32> {
                Ok(cfg.n)
            }
        }

        let fig = Figment::new().merge(figment::providers::Toml::string("[cfg]\nn = 7\n"));
        let app = AppBuilder::<HNil>::with_figment(fig)
            .with_plugin(UsesCfg)
            .await
            .unwrap();
        assert_eq!(app.state().head, 7);
    }
}
