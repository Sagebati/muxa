//! muxa-sqlx — SQLx pool plugin(s) for muxa.
//!
//! One module per database driver, each behind its own Cargo feature:
//!
//! | Feature        | Default | Types                                                 |
//! |----------------|---------|-------------------------------------------------------|
//! | `postgres`     | ✓       | [`SqlxPool`], [`SqlxPlugin`], [`SqlxBackend`]         |
//! | `sqlite`       |         | [`SqlitePool`], [`SqlitePlugin`]                      |
//! | `mysql`        |         | (drivers pulled in; bring-your-own-plugin for now)    |
//!
//! The Postgres pool also implements the `HasPgExecutorFor<SqlxBackend>`
//! capability so `muxa-pgmq` can run over it. SQLite has no pgmq backend
//! (pgmq is Postgres-only).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub use pg::*;

#[cfg(feature = "sqlite")]
mod sqlite;
#[cfg(feature = "sqlite")]
pub use sqlite::*;
