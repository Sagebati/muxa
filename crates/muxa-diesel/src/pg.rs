//! Postgres-driver Diesel plugin via `diesel-async` + deadpool.

use std::ops::Deref;

use bon::Builder;
use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use dupe::Dupe;
use muxa_core::{BuildCtx, Error, PgmqBackend, PgmqPool, Plugin, Result, State};
use serde::Deserialize;

/// Newtype wrapping a deadpool-managed `AsyncPgConnection` pool.
#[derive(Clone)]
pub struct DieselPool(pub Pool<AsyncPgConnection>);

impl Deref for DieselPool {
    type Target = Pool<AsyncPgConnection>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Pool<AsyncPgConnection>> for DieselPool {
    fn from(pool: Pool<AsyncPgConnection>) -> Self {
        Self(pool)
    }
}

// `deadpool::managed::Pool` is Arc-backed; cloning the newtype is a
// refcount bump.
impl Dupe for DieselPool {}

impl PgmqPool for DieselPool {}

/// Backend marker for the async Diesel Postgres pool.
pub struct DieselBackend;

impl PgmqBackend for DieselBackend {
    type Pool = DieselPool;
}
// `HasPgExecutorFor<DieselBackend, _>` is supplied by the blanket impl in
// muxa-core for any state HList containing a `DieselPool`.

fn default_max_connections() -> u32 {
    10
}

/// Configuration for [`DieselPlugin`]. Read from `[diesel]`.
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct DieselConfig {
    /// Postgres connection URL. **Required** — there is no default.
    pub url: String,
    /// Maximum number of pool connections.
    #[builder(default = default_max_connections())]
    pub max_connections: u32,
}

impl Default for DieselConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: default_max_connections(),
        }
    }
}

/// The Diesel (async Postgres) plugin.
///
/// # Migrations mode
///
/// With the `migrations` feature, hand the plugin a
/// [`MigrationsRunner`](crate::MigrationsRunner) via
/// [`with_migrations`](Self::with_migrations): all pending migrations are
/// applied (in one transaction) at startup, *before* the pool is published, so
/// later plugins and handlers observe the migrated schema.
///
/// ```ignore
/// use muxa::diesel::{embed_migrations, EmbeddedMigrations, DieselPlugin, MigrationsRunner};
///
/// const MIGRATIONS: EmbeddedMigrations = embed_migrations!();
///
/// App::default()
///     .with_plugin(DieselPlugin::new().with_migrations(MigrationsRunner::new(MIGRATIONS)))
///     .await?
///     // …
/// ```
#[derive(Default)]
pub struct DieselPlugin {
    /// Migration runner to execute at startup when migrations mode is enabled.
    #[cfg(feature = "migrations")]
    migrations: Option<crate::migrations::MigrationsRunner>,
}

impl DieselPlugin {
    /// Create the plugin with migrations mode disabled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable migrations mode: apply all pending migrations (in one
    /// transaction) at startup before the pool is published.
    #[cfg(feature = "migrations")]
    #[must_use]
    pub fn with_migrations(mut self, runner: crate::migrations::MigrationsRunner) -> Self {
        self.migrations = Some(runner);
        self
    }
}

impl<S: State> Plugin<S> for DieselPlugin {
    type Output = DieselPool;
    type Config = DieselConfig;
    const CONFIG_PREFIX: &'static str = "diesel";

    async fn build(self, cfg: DieselConfig, _state: &S, _ctx: &mut BuildCtx) -> Result<DieselPool> {
        if cfg.url.is_empty() {
            return Err(Error::other(
                "muxa-diesel: `diesel.url` is required (set in config or MUXA_DIESEL__URL)",
            ));
        }

        // With the `sentry` feature: register diesel-sentry's query
        // instrumentation as diesel's global default *before* any connection
        // (migration, probe, pool) is established, so they're all traced.
        #[cfg(feature = "sentry")]
        {
            diesel_sentry::install().map_err(Error::other)?;
            tracing::info!("muxa-diesel[pg]: query tracing enabled (diesel-sentry)");
        }

        // Apply migrations before building the pool so the schema is ready for
        // any plugin or handler that touches the database after this point.
        #[cfg(feature = "migrations")]
        if let Some(runner) = self.migrations {
            let applied = runner.run(&cfg.url).await?;
            tracing::info!(applied, "muxa-diesel[pg]: applied pending migrations");
        }

        tracing::info!(
            max_connections = cfg.max_connections,
            "muxa-diesel[pg]: connecting"
        );

        let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(cfg.url);
        let pool = Pool::builder(manager)
            .max_size(cfg.max_connections as usize)
            .build()
            .map_err(Error::other)?;

        tracing::info!("muxa-diesel[pg]: pool built");
        Ok(DieselPool(pool))
    }
}
