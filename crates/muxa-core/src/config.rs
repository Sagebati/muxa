//! figment-based configuration loading.
//!
//! An app's configuration is named after the app: env vars carry a prefix
//! derived from its name (`katago-ws` → `KATAGO_WS_WEB__PORT`) and the file is
//! `config.toml`. The framework's own name never shows up in either.
//!
//! Below both, a plugin can name the conventional unprefixed variables of its
//! ecosystem (`DATABASE_URL`, `SENTRY_DSN`, `PORT`, …) as [`EnvFallback`]s:
//! each fills its key only when the app's own configuration leaves it unset.

use std::path::Path;

use figment::providers::{Env, Format as _, Toml};
use figment::value::{Dict, Map, Value};
use figment::{Figment, Metadata, Profile, Provider};
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

/// A conventional, unprefixed environment variable that stands in for one key
/// of a plugin's section when the app's own configuration doesn't set it.
///
/// `EnvFallback { var: "DATABASE_URL", key: "url" }` on the `sqlx` section
/// makes `DATABASE_URL` the value of `sqlx.url` unless `[sqlx] url` or
/// `<APP>_SQLX__URL` is set: the app's configuration always wins, so a
/// variable the platform happens to export never overrides what the app chose.
/// An unset or empty variable is skipped.
///
/// A plugin lists its fallbacks in
/// [`Plugin::ENV_FALLBACKS`](crate::Plugin::ENV_FALLBACKS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvFallback {
    /// The environment variable, exactly as the ecosystem spells it
    /// (e.g. `SENTRY_DSN`).
    pub var: &'static str,
    /// The key it fills, inside the plugin's section (e.g. `dsn`).
    pub key: &'static str,
}

/// One [`EnvFallback`] that is set: its value at `<section>.<key>`, reported in
/// errors as coming from that variable.
struct FallbackValue {
    var: &'static str,
    section: String,
    key: &'static str,
    raw: String,
}

impl FallbackValue {
    fn read(section: &str, fallback: EnvFallback) -> Option<Self> {
        let raw = std::env::var(fallback.var)
            .ok()
            .filter(|raw| !raw.is_empty())?;
        Some(Self {
            var: fallback.var,
            section: section.to_owned(),
            key: fallback.key,
            raw,
        })
    }
}

impl Provider for FallbackValue {
    fn metadata(&self) -> Metadata {
        Metadata::named(format!("`{}` environment variable", self.var))
    }

    fn data(&self) -> figment::Result<Map<Profile, Dict>> {
        // Parsed the way `Env` parses values, so `PORT=8080` is a number.
        let Ok(value) = self.raw.parse::<Value>();
        let section = Dict::from([(self.key.to_owned(), value)]);
        let root = Dict::from([(self.section.clone(), Value::from(section))]);
        Ok(Profile::Default.collect(root))
    }
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
        self.get_or_env(prefix, &[])
    }

    /// Like [`get`](Self::get), with `fallbacks` filling the keys the
    /// configuration leaves unset (see [`EnvFallback`]). A section that only a
    /// fallback provides counts as present.
    pub fn get_or_env<T>(&self, prefix: &str, fallbacks: &[EnvFallback]) -> Result<T>
    where
        T: DeserializeOwned + Default,
    {
        if prefix.is_empty() {
            return Ok(T::default());
        }
        let set: Vec<_> = fallbacks
            .iter()
            .filter_map(|fallback| FallbackValue::read(prefix, *fallback))
            .collect();
        if set.is_empty() {
            return extract_section(self.figment, prefix);
        }
        // `join` only adds what isn't there yet: the configuration wins.
        let figment = set.into_iter().fold(self.figment.clone(), Figment::join);
        extract_section(&figment, prefix)
    }
}

fn extract_section<T>(figment: &Figment, prefix: &str) -> Result<T>
where
    T: DeserializeOwned + Default,
{
    match figment.find_value(prefix) {
        Ok(_) => figment.extract_inner(prefix).map_err(Into::into),
        Err(_) => Ok(T::default()),
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

    const URL: EnvFallback = EnvFallback {
        var: "DATABASE_URL",
        key: "url",
    };

    #[derive(serde::Deserialize, Default, Debug, PartialEq, Eq)]
    #[serde(default)]
    struct Db {
        url: String,
        max: u32,
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn env_fallback_fills_only_what_the_config_leaves_unset() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("DATABASE_URL", "postgres://standard");
            let db = |toml: &str| {
                let fig =
                    Figment::from(Toml::string(toml)).merge(Env::prefixed("APP_").split("__"));
                Sections::new(&fig).get_or_env::<Db>("db", &[URL]).unwrap()
            };

            // No section at all: the fallback alone makes it present.
            assert_eq!(db("").url, "postgres://standard");
            // The section without the key: filled, the rest kept.
            assert_eq!(
                db("[db]\nmax = 3\n"),
                Db {
                    url: "postgres://standard".into(),
                    max: 3
                }
            );
            // The file wins, and so does the app's prefixed var.
            assert_eq!(db("[db]\nurl = \"file\"\n").url, "file");
            jail.set_env("APP_DB__URL", "prefixed");
            assert_eq!(db("[db]\nurl = \"file\"\n").url, "prefixed");
            // `get` reads no fallback.
            let fig = Figment::new();
            assert_eq!(Sections::new(&fig).get::<Db>("db").unwrap(), Db::default());
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn env_fallback_skips_empty_and_names_the_var_in_errors() {
        figment::Jail::expect_with(|jail| {
            let port = EnvFallback {
                var: "PORT",
                key: "max",
            };
            let fig = Figment::new();
            let sections = Sections::new(&fig);

            jail.set_env("PORT", "");
            assert_eq!(
                sections.get_or_env::<Db>("db", &[port]).unwrap(),
                Db::default()
            );

            jail.set_env("PORT", "8080");
            assert_eq!(sections.get_or_env::<Db>("db", &[port]).unwrap().max, 8080);

            jail.set_env("PORT", "lots");
            let Err(crate::Error::Config(err)) = sections.get_or_env::<Db>("db", &[port]) else {
                panic!("an invalid fallback must be a config error");
            };
            assert!(
                err.to_string().contains("`PORT` environment variable"),
                "{err}"
            );
            Ok(())
        });
    }
}
