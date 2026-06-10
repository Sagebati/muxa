//! muxa — facade crate re-exporting `muxa-core` plus the integration
//! plugins enabled by Cargo features.
//!
//! Most applications depend on just `muxa = { features = ["web", "sqlite",
//! "otel"] }` and `use muxa::prelude::*;`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use muxa_core::*;

#[cfg(feature = "diesel")]
pub use muxa_diesel as diesel;
#[cfg(feature = "openapi")]
pub use muxa_openapi as openapi;
// Re-export aide + schemars (the versions muxa-openapi is built against) so apps
// use `muxa::aide` / `muxa::schemars` and the `OpenApi` type unifies. No direct
// `aide` dep is needed: the `OperationIo` derive emits relative `aide::` paths,
// which resolve through `use muxa::aide;` (muxa-openapi enables aide's
// `macros`/`axum-json`/`axum-query` features so the re-export is complete).
#[cfg(feature = "openapi")]
pub use muxa_openapi::{aide, schemars};
#[cfg(feature = "otel")]
pub use muxa_otel as otel;
#[cfg(feature = "pgmq")]
pub use muxa_pgmq as pgmq;
#[cfg(feature = "sentry")]
pub use muxa_sentry as sentry;
#[cfg(feature = "sqlx")]
pub use muxa_sqlx as sqlx;
#[cfg(feature = "web")]
pub use muxa_web as web;

/// Common imports for application code. Brings `App`, `AppBuilder`, the
/// `Plugin` trait, the capability traits, and one canonical plugin from
/// each enabled feature into scope.
pub mod prelude {
    pub use muxa_core::prelude::*;

    #[cfg(feature = "web")]
    pub use muxa_web::WebPlugin;

    #[cfg(feature = "ratelimit")]
    pub use muxa_web::ratelimit::{RateLimitConfig, per_ip_layer};

    #[cfg(feature = "sqlx")]
    pub use muxa_sqlx::{SqlxBackend, SqlxConfig, SqlxPlugin, SqlxPool};

    #[cfg(feature = "sqlite")]
    pub use muxa_sqlx::{SqliteConfig, SqlitePlugin, SqlitePool};

    #[cfg(feature = "diesel")]
    pub use muxa_diesel::{DieselBackend, DieselConfig, DieselPlugin, DieselPool};

    #[cfg(feature = "diesel-migrations")]
    pub use muxa_diesel::{EmbeddedMigrations, MigrationsRunner, embed_migrations};

    #[cfg(feature = "diesel-sentry")]
    pub use muxa_diesel::SentryInstrumentation;

    #[cfg(feature = "pgmq")]
    pub use muxa_pgmq::{PgmqConfig, PgmqPlugin};

    #[cfg(feature = "otel")]
    pub use muxa_otel::{OtelConfig, OtelPlugin, TelemetryHandles};

    #[cfg(feature = "sentry")]
    pub use muxa_sentry::{SentryConfig, SentryHandle, SentryPlugin};

    #[cfg(feature = "openapi")]
    pub use muxa_openapi::{OpenApiConfig, OpenApiPlugin};

    // The aide-aware web plugin (finishes an `ApiRouter` + serves the spec).
    #[cfg(all(feature = "web", feature = "openapi"))]
    pub use muxa_web::ApiPlugin;
}
