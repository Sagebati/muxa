//! muxa-pgmq — `PgmqPlugin<B, Idx>` generic over a `PgmqBackend`, backed by
//! the [Sagebati/pgmq](https://github.com/Sagebati/pgmq) fork that abstracts
//! over driver pools through Cargo features.
//!
//! `PgmqPlugin` is **installation-only** (`type Output = ()`): it provisions
//! pgmq at startup and adds nothing to the application state. Its `build` step:
//!
//! 1. Borrows the matching pool from application state via the
//!    `HasPgExecutorFor<B, _>` capability.
//! 2. Calls `pgmq::install::{driver}::install_sql_from_embedded(&pool)` to
//!    ensure the pgmq SQL extension and migration tables exist (idempotent).
//! 3. Acquires a connection and calls `PGMQueueExt::create(queue)` for each
//!    declared queue name.
//!
//! Queue operations (`send`/`read`/`archive`/…) are pgmq's `Queue` trait
//! methods on a `&mut` connection from that same pool — call them directly on a
//! connection from the pool plugin's resource; there is no separate client.
//!
//! Per-backend behaviour lives in feature-gated submodules; there is **no**
//! trait abstraction across backends inside this crate. That's a deliberate
//! workaround for a known Rust HRTB limitation with sqlx's `Executor<'_>`
//! impl on `&mut PgConnection` — wrapping the install/create calls behind a
//! `+ Send` future-returning trait method confuses Send inference. Direct
//! per-backend `Plugin` impls keep the future type concrete and side-step
//! the issue.
//!
//! ```ignore
//! use muxa::prelude::*;
//! use muxa_sqlx::{SqlxBackend, SqlxPlugin};
//! use muxa_pgmq::PgmqPlugin;
//!
//! #[tokio::main]
//! async fn main() -> muxa::Result<()> {
//!     App::default()
//!         .with_plugin(SqlxPlugin::default()).await?
//!         .with_plugin(PgmqPlugin::<SqlxBackend, _>::new().queues(["jobs"])).await?
//!         // ... add WebPlugin last
//!         .run().await
//! }
//! ```
//!
//! ## Features
//!
//! - `sqlx` — enables the `Plugin` impl for `PgmqPlugin<SqlxBackend, _>`.
//!   Pulls in `muxa-sqlx` and the pgmq fork's `sqlx` adapter.
//! - `diesel-async` — enables the `Plugin` impl for
//!   `PgmqPlugin<DieselBackend, _>`. Pulls in `muxa-diesel` and the pgmq
//!   fork's `diesel-async` adapter.
//!
//! The `muxa` facade auto-activates these when its own `sqlx` / `diesel`
//! feature is enabled.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::marker::PhantomData;

use muxa_core::{Here, PgmqBackend};
use serde::Deserialize;

/// Configuration for [`PgmqPlugin`]. Read from `[pgmq]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PgmqConfig {
    /// Queues to ensure exist at startup. Merged with queues passed via
    /// [`PgmqPlugin::queues`].
    pub queues: Vec<String>,
}

/// The pgmq plugin.
///
/// Generic over a backend marker `B` and an HList index phantom `Idx`.
/// Users write `PgmqPlugin::<SqlxBackend, _>::new().queues([...])` and let
/// the compiler infer the index, or `PgmqPlugin::<SqlxBackend>::…` if the
/// pool plugin sits immediately before this one.
///
/// The actual `Plugin` impls live in feature-gated submodules — one per
/// supported backend.
pub struct PgmqPlugin<B: PgmqBackend, Idx = Here> {
    queues: Vec<String>,
    _phantom: PhantomData<fn() -> (B, Idx)>,
}

impl<B: PgmqBackend, Idx> PgmqPlugin<B, Idx> {
    /// Construct a plugin with no preconfigured queues.
    pub fn new() -> Self {
        Self {
            queues: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Declare queues that should be ensured at startup. Merged with any
    /// queues found in the figment's `[pgmq]` section.
    pub fn queues<I, T>(mut self, qs: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.queues.extend(qs.into_iter().map(Into::into));
        self
    }
}

impl<B: PgmqBackend, Idx> Default for PgmqPlugin<B, Idx> {
    fn default() -> Self {
        Self::new()
    }
}

/// Merge plugin-declared and figment-declared queues, sort + dedup.
#[doc(hidden)]
pub fn merge_queues(mut from_plugin: Vec<String>, from_config: Vec<String>) -> Vec<String> {
    from_plugin.extend(from_config);
    from_plugin.sort();
    from_plugin.dedup();
    from_plugin
}

#[allow(dead_code, reason = "dead when no driver features are enabled")]
fn box_pgmq_err(err: pgmq::PgmqError) -> muxa_core::Error {
    muxa_core::Error::other(format!("pgmq: {err}"))
}

// ---------------------------------------------------------------------------
// Per-driver `Plugin` impls. Direct impls (no trait dispatch) keep the
// returned future type concrete, side-stepping a Rust HRTB limitation with
// sqlx's `Executor<'_>` impl on `&mut PgConnection`.
// ---------------------------------------------------------------------------

#[cfg(feature = "sqlx")]
mod sqlx_impl {
    use muxa_core::{BuildCtx, Error, HasPgExecutorFor, Plugin, Result, State};
    use muxa_sqlx::SqlxBackend;
    use pgmq::PGMQueueExt;

    impl<S, Idx> Plugin<S> for super::PgmqPlugin<SqlxBackend, Idx>
    where
        S: State + HasPgExecutorFor<SqlxBackend, Idx>,
        Idx: 'static,
    {
        type Output = ();
        type Config = super::PgmqConfig;
        const CONFIG_PREFIX: &'static str = "pgmq";

        async fn build(
            self,
            cfg: super::PgmqConfig,
            state: &S,
            _ctx: &mut BuildCtx,
        ) -> Result<()> {
            let pool = state.pg_executor();
            let queues = super::merge_queues(self.queues, cfg.queues);

            tracing::info!(
                ?queues,
                "muxa-pgmq[sqlx]: installing pgmq and ensuring queues"
            );

            pgmq::install::sqlx::install_sql_from_embedded(&pool.0)
                .await
                .map_err(super::box_pgmq_err)?;

            let mut conn = pool
                .0
                .acquire()
                .await
                .map_err(|e| Error::other(format!("sqlx acquire: {e}")))?;
            for q in &queues {
                conn.create(q).await.map_err(super::box_pgmq_err)?;
            }

            Ok(())
        }
    }
}

#[cfg(feature = "diesel-async")]
mod diesel_async_impl {
    use muxa_core::{BuildCtx, Error, HasPgExecutorFor, Plugin, Result, State};
    use muxa_diesel::DieselBackend;
    use pgmq::Queue;

    impl<S, Idx> Plugin<S> for super::PgmqPlugin<DieselBackend, Idx>
    where
        S: State + HasPgExecutorFor<DieselBackend, Idx>,
        Idx: 'static,
    {
        type Output = ();
        type Config = super::PgmqConfig;
        const CONFIG_PREFIX: &'static str = "pgmq";

        async fn build(
            self,
            cfg: super::PgmqConfig,
            state: &S,
            _ctx: &mut BuildCtx,
        ) -> Result<()> {
            let pool = state.pg_executor();
            let queues = super::merge_queues(self.queues, cfg.queues);

            tracing::info!(
                ?queues,
                "muxa-pgmq[diesel-async]: installing pgmq and ensuring queues"
            );

            pgmq::install::diesel_async::install_sql_from_embedded(&pool.0)
                .await
                .map_err(super::box_pgmq_err)?;

            let mut conn = pool
                .0
                .get()
                .await
                .map_err(|e| Error::other(format!("diesel get: {e}")))?;
            for q in &queues {
                // `Queue` is implemented for `&mut AsyncPgConnection` and its
                // methods take `self`, so reborrow per call.
                (&mut *conn).create(q).await.map_err(super::box_pgmq_err)?;
            }

            Ok(())
        }
    }
}
