//! figment-based configuration loading.

use std::path::PathBuf;

use figment::Figment;
use figment::providers::{Env, Format as _, Toml};
use serde::de::DeserializeOwned;

use crate::error::Result;

/// Default config-file path: `muxa.toml` in the current directory.
pub const DEFAULT_CONFIG_PATH: &str = "muxa.toml";

/// Default environment-variable prefix: `MUXA_`.
///
/// The prefix includes its trailing separator. Override it via
/// [`load_figment_with_prefix`] / [`load_figment_from_with_prefix`] (or the
/// matching [`crate::AppBuilder`] constructors) so an app can read, e.g.,
/// `MYAPP_PGMQ__URL` instead of `MUXA_PGMQ__URL`.
pub const DEFAULT_ENV_PREFIX: &str = "MUXA_";

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
/// [`load_figment_from`]. To use a different env-var prefix, call
/// [`load_figment_with_prefix`].
pub fn load_figment() -> Figment {
    load_figment_with_prefix(DEFAULT_ENV_PREFIX)
}

/// Build the application figment with a custom env-var prefix.
///
/// Same as [`load_figment`], but env vars are read with `prefix` instead of
/// `MUXA_`, and the bootstrap config-path var becomes `{prefix}CONFIG`
/// (e.g. prefix `MYAPP_` → `$MYAPP_CONFIG`). The prefix should include its
/// trailing separator (matching figment's `Env::prefixed` convention).
pub fn load_figment_with_prefix(prefix: &str) -> Figment {
    let config_var = format!("{prefix}CONFIG");
    let path = std::env::var_os(config_var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
    load_figment_from_with_prefix(path, prefix)
}

/// Build the application figment from a specific config-file path.
///
/// Same env-var layering as [`load_figment`], but the file path is
/// supplied explicitly.
pub fn load_figment_from<P: Into<PathBuf>>(path: P) -> Figment {
    load_figment_from_with_prefix(path, DEFAULT_ENV_PREFIX)
}

/// Build the application figment from an explicit path with a custom
/// env-var prefix.
///
/// The most general loader: combines the explicit-path behaviour of
/// [`load_figment_from`] with the custom prefix of
/// [`load_figment_with_prefix`]. The prefix should include its trailing
/// separator.
pub fn load_figment_from_with_prefix<P: Into<PathBuf>>(path: P, prefix: &str) -> Figment {
    Figment::new()
        .merge(Toml::file(path.into()))
        // Strip the framework's own bootstrap env var so it doesn't leak
        // into the figment as a top-level `config` key. (The `{prefix}CONFIG`
        // var maps to the `config` key once the prefix is stripped.)
        .merge(Env::prefixed(prefix).split("__").ignore(&["config"]))
}

/// A plugin's view of the application configuration: named sections, read one
/// at a time into a type.
///
/// Plugins never hold the merged configuration itself, so they can't clone it,
/// print it or walk other plugins' keys. [`Plugin::read_config`](crate::Plugin::read_config)
/// receives a `Sections` and reads the section (or sections) the plugin's
/// `Config` type is made of. The application, which owns the configuration,
/// reaches all of it through [`AppBuilder::figment`](crate::AppBuilder::figment).
pub struct Sections<'figment> {
    figment: &'figment Figment,
}

impl<'figment> Sections<'figment> {
    pub(crate) fn new(figment: &'figment Figment) -> Self {
        Self { figment }
    }

    /// Deserialize the section at `prefix` (e.g. `"pgmq"` for the `[pgmq]`
    /// table and `MUXA_PGMQ__*` env vars).
    ///
    /// * `prefix` is `""` — "no configuration": returns `T::default()`.
    /// * The section is absent — returns `T::default()`.
    /// * The section is present but doesn't deserialize — returns the error,
    ///   so a typo in a value fails the build instead of becoming a default.
    pub fn get<T>(&self, prefix: &str) -> Result<T>
    where
        T: DeserializeOwned + Default,
    {
        if prefix.is_empty() {
            return Ok(T::default());
        }
        match self.figment.find_value(prefix) {
            Ok(_) => self.figment.extract_inner(prefix).map_err(Into::into),
            Err(_) => Ok(T::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn custom_prefix_reads_namespaced_env_vars() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("MYAPP_PGMQ__URL", "postgres://example");

            let fig = load_figment_from_with_prefix("does-not-exist.toml", "MYAPP_");
            let url: String = fig.extract_inner("pgmq.url").unwrap();
            assert_eq!(url, "postgres://example");

            // The default prefix must NOT pick up the custom-prefixed var.
            let default = load_figment_from_with_prefix("does-not-exist.toml", DEFAULT_ENV_PREFIX);
            assert!(default.extract_inner::<String>("pgmq.url").is_err());

            Ok(())
        });
    }

    #[derive(serde::Deserialize, Default, Debug, PartialEq, Eq)]
    struct Limits {
        #[serde(default)]
        max: u32,
    }

    #[test]
    fn section_absent_or_unnamed_falls_back_to_default() {
        let fig = Figment::new().merge(Toml::string("[other]\nmax = 9\n"));
        let sections = Sections::new(&fig);
        assert_eq!(sections.get::<Limits>("limits").unwrap(), Limits::default());
        assert_eq!(sections.get::<Limits>("").unwrap(), Limits::default());
    }

    #[test]
    fn section_present_is_read() {
        let fig = Figment::new().merge(Toml::string("[limits]\nmax = 7\n"));
        assert_eq!(
            Sections::new(&fig).get::<Limits>("limits").unwrap(),
            Limits { max: 7 }
        );
    }

    #[test]
    fn section_present_but_invalid_is_an_error() {
        let fig = Figment::new().merge(Toml::string("[limits]\nmax = \"many\"\n"));
        assert!(Sections::new(&fig).get::<Limits>("limits").is_err());
    }
}
