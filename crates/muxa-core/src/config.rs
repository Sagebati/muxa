//! figment-based configuration loading.
//!
//! An app's configuration is named after the app: env vars carry a prefix
//! derived from its name (`katago-ws` → `KATAGO_WS_WEB__PORT`) and the file is
//! `config.toml`. The framework's own name never shows up in either.

use std::path::Path;

use figment::Figment;
use figment::providers::{Env, Format as _, Toml};
use serde::de::DeserializeOwned;

use crate::error::Result;

/// Default config-file path: `config.toml` in the current directory.
pub const DEFAULT_CONFIG_PATH: &str = "config.toml";

/// The env-var prefix for an app name: uppercased, with every character that
/// isn't ASCII alphanumeric replaced by `_`, plus a trailing `_`.
/// `"katago-ws"` → `"KATAGO_WS_"`.
#[must_use]
pub fn env_prefix(app_name: &str) -> String {
    let mut prefix: String = app_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    prefix.push('_');
    prefix
}

/// Build an app's figment. Layers (last wins):
///
/// 1. One TOML file: `$<PREFIX>CONFIG` if set, else `./config.toml`. A missing
///    file is fine.
/// 2. Env vars with the app's [`env_prefix`], `__` as the key separator: for
///    `katago-ws`, `KATAGO_WS_PGMQ__URL` sets `pgmq.url` and `KATAGO_WS_ENV`
///    the run mode.
///
/// [`crate::app!`] calls this (through [`crate::AppBuilder::named`]) with the
/// calling crate's package name.
#[must_use]
pub fn load_figment(app_name: &str) -> Figment {
    match std::env::var_os(format!("{}CONFIG", env_prefix(app_name))) {
        Some(path) => load_figment_from(path, app_name),
        None => load_figment_from(DEFAULT_CONFIG_PATH, app_name),
    }
}

/// Like [`load_figment`], with the config-file path supplied explicitly.
#[must_use]
pub fn load_figment_from<P: AsRef<Path>>(path: P, app_name: &str) -> Figment {
    Figment::new()
        .merge(Toml::file(path))
        // `<PREFIX>CONFIG` picks the file; keep it out of the tree, where it
        // would land as a top-level `config` key.
        .merge(
            Env::prefixed(&env_prefix(app_name))
                .split("__")
                .ignore(&["config"]),
        )
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
    /// table and `<APP>_PGMQ__*` env vars).
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

    const APP: &str = "katago-ws";

    #[test]
    fn env_prefix_derives_from_the_name() {
        assert_eq!(env_prefix("katago-ws"), "KATAGO_WS_");
        assert_eq!(env_prefix("my.app"), "MY_APP_");
        assert_eq!(env_prefix("MyApp2"), "MYAPP2_");
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn env_overrides_the_file_under_the_app_prefix() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "config.toml",
                "env = \"dev\"\n[web]\nport = 1\nhost = \"file\"\n",
            )?;
            jail.set_env("KATAGO_WS_WEB__PORT", "3000");
            jail.set_env("KATAGO_WS_ENV", "production");
            // Another app's (or the framework's old) prefix is not read.
            jail.set_env("MUXA_WEB__HOST", "muxa");
            jail.set_env("OTHER_WEB__HOST", "other");

            let fig = load_figment(APP);
            assert_eq!(fig.extract_inner::<u16>("web.port").unwrap(), 3000);
            assert_eq!(fig.extract_inner::<String>("web.host").unwrap(), "file");
            assert_eq!(
                crate::RunMode::from_figment(&fig),
                crate::RunMode::Production
            );
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn config_var_picks_the_file_and_stays_out_of_the_tree() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("config.toml", "[web]\nport = 1\n")?;
            jail.create_file("prod.toml", "[web]\nport = 2\n")?;
            let port = || load_figment(APP).extract_inner::<u16>("web.port").unwrap();

            assert_eq!(port(), 1);
            jail.set_env("KATAGO_WS_CONFIG", "prod.toml");
            assert_eq!(port(), 2);
            assert!(load_figment(APP).find_value("config").is_err());
            Ok(())
        });
    }
}
