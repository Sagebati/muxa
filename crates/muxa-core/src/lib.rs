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
//!     muxa_core::app!()
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
pub mod mode;
pub mod plugin;
pub mod state;

pub use app::{App, AppBuilder};

/// An [`App`] named after the calling crate: `muxa::app!()` is
/// `App::named(env!("CARGO_PKG_NAME"))`, so a crate `katago-ws` reads
/// `config.toml` and `KATAGO_WS_*` env vars. See [`AppBuilder::named`].
#[macro_export]
macro_rules! app {
    () => {
        $crate::App::named(env!("CARGO_PKG_NAME"))
    };
}
pub use capability::{HasPgExecutorFor, PgBackend};
pub use config::{
    DEFAULT_CONFIG_PATH, EnvFallback, Sections, env_prefix, load_figment, load_figment_from,
};
pub use ctx::{BoxFuture, BuildCtx, Mount, RouterRegistry, ServeFn, ShutdownToken, TaskRegistry};
pub use dupe::{Dupe, Dupe_};
pub use error::{BoxedError, Error, Result};
pub use mode::RunMode;
pub use muxa_telemetry::TelemetryRegistry;
pub use plugin::Plugin;
pub use state::{HCons, HNil, Here, Selector, State, There};

/// Common imports for application and plugin code.
pub mod prelude {
    pub use crate::{
        App, AppBuilder, BuildCtx, Dupe, EnvFallback, Error, HasPgExecutorFor, PgBackend, Plugin,
        Result, RouterRegistry, RunMode, Selector, ShutdownToken, State, TaskRegistry,
    };
}
