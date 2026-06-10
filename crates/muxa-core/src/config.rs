//! figment-based configuration loading.

use std::path::PathBuf;

use figment::Figment;
use figment::providers::{Env, Format as _, Toml};

/// Default config-file path: `muxa.toml` in the current directory.
pub const DEFAULT_CONFIG_PATH: &str = "muxa.toml";

/// Build the application figment using the default lookup rules.
///
/// Layers (last wins):
/// 1. A single TOML file. Path comes from `$MUXA_CONFIG` if set, else
///    `./muxa.toml`. Missing file is fine.
/// 2. Environment variables prefixed `MUXA_`, with `__` as key separator.
///    e.g. `MUXA_PGMQ__URL=postgres://...` maps to `pgmq.url`.
///
/// To use a different config file path from code, build the figment
/// yourself and pass it to [`crate::AppBuilder::with_figment`], or call
/// [`load_figment_from`].
pub fn load_figment() -> Figment {
    let path = std::env::var_os("MUXA_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
    load_figment_from(path)
}

/// Build the application figment from a specific config-file path.
///
/// Same env-var layering as [`load_figment`], but the file path is
/// supplied explicitly.
pub fn load_figment_from<P: Into<PathBuf>>(path: P) -> Figment {
    Figment::new()
        .merge(Toml::file(path.into()))
        // Strip the framework's own bootstrap env var so it doesn't leak
        // into the figment as a top-level `config` key.
        .merge(Env::prefixed("MUXA_").split("__").ignore(&["config"]))
}
