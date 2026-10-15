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
pub mod mode;
pub mod plugin;
pub mod state;

pub use app::{App, AppBuilder};

/// An [`App`] named after the calling crate: `muxa::app!()` is
/// `App::named(env!("CARGO_PKG_NAME"))`, so a crate `katago-ws` reads
/// `KATAGO_WS_*` env vars and `config.toml`. See [`AppBuilder::named`].
#[macro_export]
macro_rules! app {
    () => {
        $crate::App::named(env!("CARGO_PKG_NAME"))
    };
}
pub use capability::{HasPgExecutorFor, PgBackend};
pub use config::{
    DEFAULT_APP_CONFIG_PATH, DEFAULT_CONFIG_PATH, DEFAULT_ENV_PREFIX, Sections, app_config_path,
    app_env_prefix, deprecated_config_names, load_figment, load_figment_for_app, load_figment_from,
    load_figment_from_for_app, load_figment_from_with_prefix, load_figment_with_prefix,
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
        App, AppBuilder, BuildCtx, Dupe, Error, HasPgExecutorFor, PgBackend, Plugin, Result,
        RouterRegistry, RunMode, Selector, ShutdownToken, State, TaskRegistry,
    };
}
