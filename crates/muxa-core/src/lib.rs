//! muxa-core — core plugin trait, HList-based state, capability traits, and
//! the application builder.
//!
//! # Overview
//!
//! ```ignore
//! use muxa_core::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> muxa_core::Result<()> {
//!     App::default()
//!         // .with_plugin(MyPlugin::default()).await?
//!         // .with_plugin(AnotherPlugin::builder().build()).await?
//!         .run().await
//! }
//! ```
//!
//! See the [`Plugin`] trait for the plugin contract and the
//! [`crate::capability`] module for the capability-trait pattern that lets
//! consumer plugins share behaviour with provider plugins (e.g. PGMQ using
//! whichever Postgres pool plugin is added).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod capability;
pub mod config;
pub mod ctx;
pub mod error;
pub mod plugin;
pub mod state;

pub use app::{App, AppBuilder};
pub use capability::{HasPgExecutorFor, PgmqBackend, PgmqPool};
pub use config::{
    DEFAULT_CONFIG_PATH, DEFAULT_ENV_PREFIX, load_figment, load_figment_from,
    load_figment_from_with_prefix, load_figment_with_prefix,
};
pub use ctx::{
    BoxFuture, BoxTask, BuildCtx, Mount, RouterRegistry, ServeFn, ShutdownToken, TaskRegistry,
};
pub use dupe::{Dupe, Dupe_};
pub use error::{BoxedError, Error, Result};
pub use muxa_telemetry::TelemetryRegistry;
pub use plugin::Plugin;
pub use state::{HCons, HNil, Here, Selector, State, There};

/// Common imports for application and plugin code.
pub mod prelude {
    pub use crate::{
        App, AppBuilder, BuildCtx, Dupe, Error, HasPgExecutorFor, PgmqBackend, PgmqPool, Plugin,
        Result, RouterRegistry, Selector, ShutdownToken, State, TaskRegistry,
    };
}
