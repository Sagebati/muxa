//! SQLite-driver SQLx plugin: `SqlitePlugin` + `SqlitePool`.
//!
//! Unlike the Postgres driver, SQLite has no pgmq backend impl — pgmq is
//! Postgres-only — so this module doesn't ship a `PgmqBackend` marker.

use std::ops::Deref;
use std::str::FromStr as _;

use bon::Builder;
use dupe::Dupe;
use muxa_core::{BuildCtx, Error, Plugin, Result, State};
use serde::Deserialize;
use sqlx::sqlite::SqliteConnectOptions;

/// Newtype wrapping `sqlx::SqlitePool`.
#[derive(Clone)]
pub struct SqlitePool(pub sqlx::SqlitePool);

impl Deref for SqlitePool {
    type Target = sqlx::SqlitePool;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<sqlx::SqlitePool> for SqlitePool {
    fn from(pool: sqlx::SqlitePool) -> Self {
        Self(pool)
    }
}

// `sqlx::SqlitePool` is Arc-backed; cloning the newtype is a refcount bump.
impl Dupe for SqlitePool {}

fn default_url() -> String {
    "sqlite::memory:".into()
}

fn default_max_connections() -> u32 {
    5
}

fn default_create_if_missing() -> bool {
    true
}

/// Configuration for [`SqlitePlugin`]. Read from `[sqlite]`.
///
/// `url` accepts any sqlx SQLite URL: `sqlite::memory:`,
/// `sqlite:./data.db`, `sqlite:file:/abs/path.db?mode=rwc`, etc.
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct SqliteConfig {
    /// SQLite URL. Defaults to `sqlite::memory:` (an in-memory DB) — fine
    /// for examples/tests, but it disappears when the pool is dropped.
    #[builder(default = default_url())]
    pub url: String,
    /// Whether to create the database file if it doesn't exist (ignored
    /// for `:memory:` URLs).
    #[builder(default = default_create_if_missing())]
    pub create_if_missing: bool,
    /// Maximum number of pool connections.
    #[builder(default = default_max_connections())]
    pub max_connections: u32,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self {
            url: default_url(),
            create_if_missing: default_create_if_missing(),
            max_connections: default_max_connections(),
        }
    }
}

/// SQLx SQLite plugin. Produces a [`SqlitePool`] resource on success.
#[derive(Default)]
pub struct SqlitePlugin;

impl<S: State> Plugin<S> for SqlitePlugin {
    type Output = SqlitePool;
    type Config = SqliteConfig;
    const CONFIG_PREFIX: &'static str = "sqlite";

    async fn build(self, cfg: SqliteConfig, _state: &S, _ctx: &mut BuildCtx) -> Result<SqlitePool> {
        tracing::info!(
            url = %cfg.url,
            max_connections = cfg.max_connections,
            "muxa-sqlx[sqlite]: connecting"
        );

        let opts = SqliteConnectOptions::from_str(&cfg.url)
            .map_err(Error::other)?
            .create_if_missing(cfg.create_if_missing);

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(cfg.max_connections)
            .connect_with(opts)
            .await
            .map_err(Error::other)?;

        tracing::info!("muxa-sqlx[sqlite]: connected");
        Ok(SqlitePool(pool))
    }
}
