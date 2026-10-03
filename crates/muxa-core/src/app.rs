//! `App` and `AppBuilder` — the entry point and plugin-chain accumulator.

use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;

use figment::Figment;
#[cfg(not(target_family = "wasm"))]
use tracing::Instrument as _;

use crate::ctx::BuildCtx;
use crate::error::{Error, Result};
use crate::plugin::Plugin;
use crate::state::{HNil, State};

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
        Self::with_figment(crate::config::load_figment_from(
            path.as_ref().to_path_buf(),
        ))
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
        Self::with_figment(crate::config::load_figment_from_with_prefix(
            path.as_ref().to_path_buf(),
            prefix,
        ))
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
        let cfg = P::read_config(self.ctx.figment())?;
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
    /// **error** here rather than being silently dropped. A caller that can
    /// run them itself takes them first with
    /// `app.ctx_mut().tasks.drain()`. A registered serve function (a
    /// `WebPlugin` in the chain) is an error for the same reason: it would
    /// never be invoked.
    pub fn into_router(self) -> Result<(axum::Router, S)> {
        if self.ctx.serve_fn.is_some() {
            return Err(Error::other(
                "into_router() called with a serve function registered — \
                 a serving plugin (WebPlugin) can't be combined with into_router()",
            ));
        }
        let pending = self.ctx.tasks.names();
        if !pending.is_empty() {
            return Err(Error::other(format!(
                "into_router() can't run background tasks, but {} are registered: {} — \
                 drain them with `ctx_mut().tasks.drain()` and run them yourself",
                pending.len(),
                pending.join(", "),
            )));
        }
        Ok((self.ctx.router.compose(), self.state))
    }

    /// Freeze the state, spawn background tasks, compose the router, and run
    /// the registered serve function until shutdown.
    ///
    /// Not available on wasm targets — there is no tokio runtime to spawn
    /// onto; use [`AppBuilder::into_router`] there.
    #[cfg(not(target_family = "wasm"))]
    pub async fn run(mut self) -> Result<()> {
        let serve = self.ctx.serve_fn.take().ok_or_else(|| {
            Error::other("no serve function registered — did you forget to add a WebPlugin?")
        })?;

        // Spawn background tasks.
        let shutdown = self.ctx.shutdown.clone();
        for (name, task) in self.ctx.tasks.drain() {
            let st = shutdown.child_token();
            tokio::spawn(
                async move {
                    tracing::info!(task = name, "spawning background task");
                    task(st).await;
                }
                .instrument(tracing::info_span!("muxa.task", name = name)),
            );
        }

        // The frozen state is reserved for future use (e.g. `Plugin::shutdown`
        // hooks) — for v0 we just keep it alive until run() returns.
        let _state = Arc::new(self.state);

        let router = self.ctx.router.compose();
        serve(router).await
    }
}

#[cfg(test)]
mod tests {
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
                ctx.tasks.spawn("ticker", |_shutdown| async {});
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
        assert!(err.to_string().contains("ticker"), "{err}");
    }

    #[tokio::test]
    async fn into_router_accepts_drained_tasks() {
        let mut app = AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(RoutePlugin { with_task: true })
            .await
            .unwrap();
        assert_eq!(app.ctx_mut().tasks.drain().len(), 1);
        assert!(app.into_router().is_ok());
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
