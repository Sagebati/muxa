//! The `Plugin` trait.
//!
//! A plugin is a unit of work that:
//!
//! 1. Reads its own configuration slice from a `figment::Figment`.
//! 2. Optionally consumes capabilities from prior plugins (via trait bounds
//!    on `S`).
//! 3. Produces a single resource of type `Self::Output` that becomes part of
//!    the application state.
//! 4. Optionally mutates the side-channel [`crate::BuildCtx`] (router,
//!    tasks, serve loop).
//!
//! Plugins are composed at compile time via the chain
//! `App::default().with_plugin(p1).await?.with_plugin(p2).await?...` — there is
//! no `dyn Plugin` and no runtime registry.

use core::future::Future;

use crate::{BuildCtx, Result, State};

/// A muxa plugin.
///
/// `S` is the application state HList at the point this plugin is added —
/// add capability trait bounds on `S` in your `impl` to require resources
/// from earlier plugins (see the `muxa-pgmq` crate for the canonical
/// example).
pub trait Plugin<S: State>: Sized + Send + 'static {
    /// The resource this plugin contributes to the state. Use `()` if the
    /// plugin only registers routes/tasks/middleware via [`BuildCtx`].
    type Output: Send + Sync + 'static;

    /// The deserialized config slice this plugin reads.
    ///
    /// `Default` is required so the framework can fall back to default
    /// values when the corresponding section is absent from the figment.
    /// Use `#[serde(default = "...")]` per field for meaningful defaults.
    type Config: serde::de::DeserializeOwned + Default + Send + 'static;

    /// figment key path this plugin reads its config from.
    ///
    /// E.g. `const CONFIG_PREFIX: &'static str = "pgmq";` reads the `[pgmq]`
    /// table from TOML and `MUXA_PGMQ__*` env vars.
    ///
    /// Use the empty string `""` for "this plugin has no configuration" —
    /// the default `read_config` will return `Self::Config::default()`
    /// without touching the figment.
    const CONFIG_PREFIX: &'static str;

    /// Build the plugin: produce its resource and (optionally) register
    /// routes, tasks, or middleware via `ctx`.
    ///
    /// The returned future is intentionally **not** required to be `Send`.
    /// The build phase is awaited inline by [`crate::AppBuilder::with_plugin`]
    /// on the current thread and is never spawned across runtimes, so a
    /// non-`Send` future is fine here. Dropping the `Send` bound lets
    /// plugins call APIs like sqlx's `Executor<'_>` on `&mut PgConnection`
    /// inside their build body without tripping a known Rust HRTB
    /// limitation when those calls are wrapped in a trait method.
    ///
    /// Background tasks spawned via [`crate::TaskRegistry::spawn`] are
    /// still required to be `Send + 'static`, as that's enforced where
    /// they're handed to `tokio::spawn` — not here.
    fn build(
        self,
        cfg: Self::Config,
        state: &S,
        ctx: &mut BuildCtx,
    ) -> impl Future<Output = Result<Self::Output>>;

    /// Read this plugin's config from the figment.
    ///
    /// Default implementation:
    /// * If `CONFIG_PREFIX` is `""`, return `Self::Config::default()`
    ///   without touching the figment.
    /// * If `CONFIG_PREFIX` is present in the figment, extract that sub-tree.
    /// * Otherwise return `Self::Config::default()`.
    ///
    /// Override if you need custom precedence or validation.
    fn read_config(figment: &figment::Figment) -> Result<Self::Config> {
        if Self::CONFIG_PREFIX.is_empty() {
            return Ok(Self::Config::default());
        }
        match figment.find_value(Self::CONFIG_PREFIX) {
            Ok(_) => figment
                .extract_inner(Self::CONFIG_PREFIX)
                .map_err(Into::into),
            Err(_) => Ok(Self::Config::default()),
        }
    }
}
