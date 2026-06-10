//! Error and Result types for muxa-core.

use thiserror::Error;

/// Boxed `std::error::Error` for opaque downstream errors.
pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Convenience `Result` alias defaulting to muxa's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Top-level error type returned by muxa-core APIs.
///
/// `figment::Error` is boxed because it's ~200 bytes and pushes
/// `Result<T, Error>` over clippy's `result_large_err` threshold.
#[derive(Debug, Error)]
pub enum Error {
    /// A plugin failed during its `build` phase.
    #[error("plugin '{plugin}' failed during build")]
    PluginBuild {
        /// Static name of the plugin (typically `std::any::type_name::<P>()`).
        plugin: &'static str,
        /// Underlying error.
        #[source]
        source: BoxedError,
    },

    /// A configuration value was missing or malformed.
    #[error("configuration error")]
    Config(#[source] Box<figment::Error>),

    /// An I/O error (binding sockets, opening files…).
    #[error("I/O error")]
    Io(#[from] std::io::Error),

    /// Catch-all for downstream errors that don't fit another variant.
    #[error("{0}")]
    Other(BoxedError),
}

impl From<figment::Error> for Error {
    fn from(err: figment::Error) -> Self {
        Error::Config(Box::new(err))
    }
}

impl Error {
    /// Wrap an arbitrary boxable error in [`Error::Other`].
    pub fn other<E: Into<BoxedError>>(err: E) -> Self {
        Self::Other(err.into())
    }

    /// Construct an [`Error::PluginBuild`] tagged with the plugin's type name.
    pub fn plugin_build<P, E: Into<BoxedError>>(source: E) -> Self {
        Self::PluginBuild {
            plugin: std::any::type_name::<P>(),
            source: source.into(),
        }
    }
}
