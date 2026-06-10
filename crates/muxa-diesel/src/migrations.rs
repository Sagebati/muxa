//! Optional embedded-migration support for the Diesel plugin.
//!
//! Enabled by the `migrations` feature. The pool is `diesel_async`, but
//! `diesel_migrations` is synchronous — so migrations run through
//! [`AsyncConnectionWrapper`] on a `spawn_blocking` thread (the wrapper blocks
//! on the runtime, which is only safe off the async executor).
//!
//! All pending migrations are applied inside **one** transaction: either every
//! pending migration applies or none do. (Migrations that opt out of
//! transactions — e.g. `CREATE INDEX CONCURRENTLY` — are incompatible with this
//! all-or-nothing mode.)

use diesel_async::AsyncConnection as _;
use diesel_async::AsyncPgConnection;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness};
use muxa_core::{Error, Result};

/// Runs the application's embedded migrations against the database.
///
/// Build it from [`embed_migrations!`](crate::embed_migrations) and hand it to
/// [`DieselPlugin::with_migrations`](crate::DieselPlugin::with_migrations), or
/// call [`run`](Self::run) directly:
///
/// ```ignore
/// use muxa::diesel::{embed_migrations, EmbeddedMigrations, MigrationsRunner};
///
/// const MIGRATIONS: EmbeddedMigrations = embed_migrations!();
/// MigrationsRunner::new(MIGRATIONS).run("postgres://…").await?;
/// ```
pub struct MigrationsRunner {
    migrations: EmbeddedMigrations,
}

impl MigrationsRunner {
    /// Wrap the application's embedded migrations.
    pub const fn new(migrations: EmbeddedMigrations) -> Self {
        Self { migrations }
    }

    /// Apply all pending migrations against `url`, in a single transaction.
    /// Returns the number of migrations applied (0 if already up to date).
    pub async fn run(self, url: &str) -> Result<usize> {
        // A dedicated connection (not from the pool): migrations are a
        // one-shot startup step and the sync wrapper takes an owned connection.
        let conn = AsyncPgConnection::establish(url)
            .await
            .map_err(Error::other)?;
        let mut wrapper = AsyncConnectionWrapper::<AsyncPgConnection>::from(conn);
        let migrations = self.migrations;

        // The wrapper blocks on the runtime, so it must run off the executor.
        // Fully-qualify the sync `Connection::transaction` — the wrapper also
        // implements `AsyncConnection`, so the bare method call is ambiguous.
        tokio::task::spawn_blocking(move || {
            diesel::Connection::transaction(&mut wrapper, |conn| {
                conn.run_pending_migrations(migrations).map(|versions| versions.len())
            })
        })
        .await
        .map_err(Error::other)? // spawn_blocking JoinError
        .map_err(Error::other) // migration / transaction error
    }
}
