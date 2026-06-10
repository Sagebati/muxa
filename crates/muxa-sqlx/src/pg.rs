//! Postgres-driver SQLx plugin: `SqlxPlugin` + `SqlxPool` + `SqlxBackend`.

use std::ops::Deref;

use bon::Builder;
use dupe::Dupe;
use muxa_core::{BuildCtx, Error, PgmqBackend, PgmqPool, Plugin, Result, State};
use serde::Deserialize;

/// Newtype wrapping `sqlx::PgPool`.
///
/// We wrap rather than expose `sqlx::PgPool` directly so capability impls
/// across multiple pool crates (sqlx, diesel, …) don't collide via
/// coherence rules. `Deref` makes call sites transparent.
#[derive(Clone)]
pub struct SqlxPool(pub sqlx::PgPool);

impl Deref for SqlxPool {
    type Target = sqlx::PgPool;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<sqlx::PgPool> for SqlxPool {
    fn from(pool: sqlx::PgPool) -> Self {
        Self(pool)
    }
}

// `sqlx::PgPool` is Arc-backed; cloning the newtype is a refcount bump.
impl Dupe for SqlxPool {}

impl PgmqPool for SqlxPool {}

/// Per-crate `PgmqBackend` marker. Used by pgmq-consumer plugins to ask
/// the type system "use the sqlx Postgres pool" rather than "use whatever
/// pool".
pub struct SqlxBackend;

impl PgmqBackend for SqlxBackend {
    type Pool = SqlxPool;
}
// The `HasPgExecutorFor<SqlxBackend, _>` impl is provided by the blanket in
// muxa-core — adding `impl PgmqBackend for SqlxBackend` is all that's needed.

fn default_max_connections() -> u32 {
    10
}

fn default_min_connections() -> u32 {
    0
}

/// Configuration for [`SqlxPlugin`]. Read from `[sqlx]`.
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct SqlxConfig {
    /// Postgres connection URL. **Required** — there is no default.
    pub url: String,
    /// Maximum number of pool connections. Defaults to 10.
    #[builder(default = default_max_connections())]
    pub max_connections: u32,
    /// Minimum number of pool connections. Defaults to 0.
    #[builder(default = default_min_connections())]
    pub min_connections: u32,
}

impl Default for SqlxConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: default_max_connections(),
            min_connections: default_min_connections(),
        }
    }
}

/// SQLx Postgres plugin. Produces a [`SqlxPool`] resource on success.
#[derive(Default)]
pub struct SqlxPlugin;

impl<S: State> Plugin<S> for SqlxPlugin {
    type Output = SqlxPool;
    type Config = SqlxConfig;
    const CONFIG_PREFIX: &'static str = "sqlx";

    async fn build(self, cfg: SqlxConfig, _state: &S, _ctx: &mut BuildCtx) -> Result<SqlxPool> {
        if cfg.url.is_empty() {
            return Err(Error::other(
                "muxa-sqlx: `sqlx.url` is required (set in config or MUXA_SQLX__URL)",
            ));
        }

        tracing::info!(
            max_connections = cfg.max_connections,
            min_connections = cfg.min_connections,
            "muxa-sqlx[pg]: connecting"
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .min_connections(cfg.min_connections)
            .connect(&cfg.url)
            .await
            .map_err(Error::other)?;

        tracing::info!("muxa-sqlx[pg]: connected");
        Ok(SqlxPool(pool))
    }
}
