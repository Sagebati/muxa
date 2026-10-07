//! The `Plugin` trait.
//!
//! A plugin is a unit of work that:
//!
//! 1. Reads its own configuration section (see [`crate::Sections`]).
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

use crate::{BuildCtx, Result, Sections, State};

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
    /// values when the corresponding section is absent from the configuration.
    /// Use `#[serde(default = "...")]` per field for meaningful defaults.
    type Config: serde::de::DeserializeOwned + Default + Send + 'static;

    /// Name of the configuration section this plugin reads.
    ///
    /// E.g. `const CONFIG_PREFIX: &'static str = "pgmq";` reads the `[pgmq]`
    /// table from TOML and `MUXA_PGMQ__*` env vars.
    ///
    /// Use the empty string `""` for "this plugin has no configuration" —
    /// the default `read_config` will return `Self::Config::default()`.
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

    /// Read this plugin's config.
    ///
    /// `sections` gives access to the configuration by named section only —
    /// a plugin never sees the merged configuration as a whole.
    ///
    /// Default implementation: read the section at `CONFIG_PREFIX` (see
    /// [`Sections::get`] for the absent / empty-prefix / invalid rules).
    ///
    /// Override when `Config` is made of more than one section — read each
    /// one with [`Sections::get`] and assemble them — or to add validation.
    /// This is the only place a plugin reads configuration; `build` gets the
    /// resulting `Config` and nothing else.
    fn read_config(sections: &Sections<'_>) -> Result<Self::Config> {
        sections.get(Self::CONFIG_PREFIX)
    }
}
