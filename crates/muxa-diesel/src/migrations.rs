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
//!
//! Concurrent runs (several replicas starting at once) are serialized by a
//! transaction-scoped Postgres advisory lock taken before anything else in that
//! transaction; see [`MigrationsRunner`].

use diesel::RunQueryDsl as _;
use diesel::connection::SimpleConnection as _;
use diesel::sql_types::BigInt;
use diesel_async::AsyncConnection as _;
use diesel_async::AsyncPgConnection;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness as _};
use muxa_core::{Error, Result};

/// 64-bit FNV-1a, usable in a `const`.
const fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

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
///
/// # Concurrent runs
///
/// Runs against the same database are serialized, so several replicas can
/// start at once: the first statement of the migration transaction is
/// `SELECT pg_advisory_xact_lock(key)`. The other runners wait on the lock,
/// then find nothing pending and return `Ok(0)`. The lock is released when the
/// transaction commits or rolls back, including when the process dies
/// mid-migration.
///
/// The key defaults to [`DEFAULT_LOCK_KEY`](Self::DEFAULT_LOCK_KEY), shared by
/// every muxa app. Two apps that share a database but keep separate migration
/// sets can pick their own with [`with_lock_key`](Self::with_lock_key) so they
/// don't wait on each other.
pub struct MigrationsRunner {
    migrations: EmbeddedMigrations,
    lock_key: i64,
}

impl MigrationsRunner {
    /// The advisory-lock key used unless [`with_lock_key`](Self::with_lock_key)
    /// sets another: the 64-bit FNV-1a hash of `"muxa-diesel-migrations"`.
    #[expect(
        clippy::cast_possible_wrap,
        reason = "the hash's bits are reinterpreted as Postgres' signed bigint"
    )]
    pub const DEFAULT_LOCK_KEY: i64 = fnv1a_64(b"muxa-diesel-migrations") as i64;

    /// Wrap the application's embedded migrations.
    pub const fn new(migrations: EmbeddedMigrations) -> Self {
        Self {
            migrations,
            lock_key: Self::DEFAULT_LOCK_KEY,
        }
    }

    /// Use `key` as the advisory-lock key that serializes concurrent runs,
    /// instead of [`DEFAULT_LOCK_KEY`](Self::DEFAULT_LOCK_KEY).
    #[must_use]
    pub const fn with_lock_key(mut self, key: i64) -> Self {
        self.lock_key = key;
        self
    }

    /// Apply all pending migrations against `url`, in a single transaction.
    /// Returns the number of migrations applied (0 if already up to date).
    ///
    /// Waits for any concurrent run against the same database (with the same
    /// lock key) to finish first.
    pub async fn run(self, url: &str) -> Result<usize> {
        // A dedicated connection (not from the pool): migrations are a
        // one-shot startup step and the sync wrapper takes an owned connection.
        let conn = AsyncPgConnection::establish(url)
            .await
            .map_err(Error::other)?;
        let mut wrapper = AsyncConnectionWrapper::<AsyncPgConnection>::from(conn);
        let Self {
            migrations,
            lock_key,
        } = self;

        // The wrapper blocks on the runtime, so it must run off the executor.
        // Fully-qualify the sync `Connection::transaction` — the wrapper also
        // implements `AsyncConnection`, so the bare method call is ambiguous.
        tokio::task::spawn_blocking(move || {
            diesel::Connection::transaction(&mut wrapper, |conn| {
                // READ COMMITTED takes a fresh snapshot per statement, so once
                // the lock is ours the harness sees what the previous holder
                // committed. Pin it in case the server's default is stricter
                // (a REPEATABLE READ snapshot would predate the wait).
                conn.batch_execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")?;
                diesel::sql_query("SELECT pg_advisory_xact_lock($1)")
                    .bind::<BigInt, _>(lock_key)
                    .execute(conn)?;
                conn.run_pending_migrations(migrations)
                    .map(|versions| versions.len())
            })
        })
        .await
        .map_err(Error::other)? // spawn_blocking JoinError
        .map_err(Error::other) // migration / transaction error
    }
}
