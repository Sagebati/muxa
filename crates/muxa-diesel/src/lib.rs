//! muxa-diesel — Diesel pool plugin for muxa, **async-only** via
//! `diesel-async` + deadpool.
//!
//! | Feature        | Default | Types                                              |
//! |----------------|---------|----------------------------------------------------|
//! | `postgres`     | ✓       | [`DieselPool`], [`DieselPlugin`], [`DieselBackend`] |
//! | `mysql`        |         | (bring-your-own plugin for now)                    |
//!
//! `diesel-async` has no SQLite backend, and muxa-diesel does not bridge
//! sync diesel onto a blocking pool. If you need async SQLite, use
//! `muxa-sqlx` with its `sqlite` feature.
//!
//! The Postgres pool implements `HasPgExecutorFor<DieselBackend>` so
//! `muxa-pgmq` can run over it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub use pg::*;

#[cfg(feature = "migrations")]
mod migrations;
#[cfg(feature = "migrations")]
pub use migrations::MigrationsRunner;

// The `sentry` feature makes `DieselPlugin` install diesel-sentry's query
// instrumentation globally at build (see `pg.rs`). The instrumentation type is
// re-exported for direct / per-connection use.
#[cfg(feature = "sentry")]
pub use diesel_sentry::SentryInstrumentation;
// Re-exported so applications need not depend on `diesel_migrations` directly:
// `const MIGRATIONS: EmbeddedMigrations = embed_migrations!();`
#[cfg(feature = "migrations")]
pub use diesel_migrations::{EmbeddedMigrations, embed_migrations};
