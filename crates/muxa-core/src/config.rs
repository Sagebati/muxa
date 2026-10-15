//! figment-based configuration loading.

use std::path::{Path, PathBuf};

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
    match std::env::var_os(format!("{prefix}CONFIG")) {
        Some(path) => load_figment_from_with_prefix(path, prefix),
        None => load_figment_from_with_prefix(DEFAULT_CONFIG_PATH, prefix),
    }
}

/// Build the application figment from a specific config-file path.
///
/// Same env-var layering as [`load_figment`], but the file path is
/// supplied explicitly.
pub fn load_figment_from<P: AsRef<Path>>(path: P) -> Figment {
    load_figment_from_with_prefix(path, DEFAULT_ENV_PREFIX)
}

/// Build the application figment from an explicit path with a custom
/// env-var prefix.
///
/// The most general loader: combines the explicit-path behaviour of
/// [`load_figment_from`] with the custom prefix of
/// [`load_figment_with_prefix`]. The prefix should include its trailing
/// separator.
pub fn load_figment_from_with_prefix<P: AsRef<Path>>(path: P, prefix: &str) -> Figment {
    Figment::new()
        .merge(Toml::file(path))
        // Strip the framework's own bootstrap env var so it doesn't leak
        // into the figment as a top-level `config` key. (The `{prefix}CONFIG`
        // var maps to the `config` key once the prefix is stripped.)
        .merge(Env::prefixed(prefix).split("__").ignore(&["config"]))
}

/// Default config-file path for a named app ([`load_figment_for_app`]):
/// `config.toml` in the current directory.
pub const DEFAULT_APP_CONFIG_PATH: &str = "config.toml";

/// The env-var prefix for an app name: uppercased, with every character that
/// isn't ASCII alphanumeric replaced by `_`, plus a trailing `_`.
/// `"katago-ws"` → `"KATAGO_WS_"`.
#[must_use]
pub fn app_env_prefix(app_name: &str) -> String {
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

/// The config file a named app loads, first match wins:
///
/// 1. `$<PREFIX>CONFIG` (e.g. `$KATAGO_WS_CONFIG`);
/// 2. `$MUXA_CONFIG` (legacy);
/// 3. `./config.toml` if it exists;
/// 4. `./muxa.toml` if it exists (legacy);
/// 5. otherwise `./config.toml` (a missing file is fine).
#[must_use]
pub fn app_config_path(app_name: &str) -> PathBuf {
    let prefix = app_env_prefix(app_name);
    [
        format!("{prefix}CONFIG"),
        format!("{DEFAULT_ENV_PREFIX}CONFIG"),
    ]
    .into_iter()
    .find_map(std::env::var_os)
    .map(PathBuf::from)
    .or_else(|| {
        [DEFAULT_APP_CONFIG_PATH, DEFAULT_CONFIG_PATH]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    })
    .unwrap_or_else(|| PathBuf::from(DEFAULT_APP_CONFIG_PATH))
}

/// Build the figment for an app that owns its configuration namespace, with
/// no muxa branding: env vars are prefixed by the app's name
/// ([`app_env_prefix`]) and the file is `config.toml` ([`app_config_path`]).
///
/// Layers (last wins):
/// 1. the TOML file from [`app_config_path`];
/// 2. legacy `MUXA_*` env vars, so deployments that predate the rename keep
///    working (see [`deprecated_config_names`]);
/// 3. `<PREFIX>*` env vars, `__`-nested like `MUXA_*`: with the name
///    `katago-ws`, `KATAGO_WS_WEB__PORT` sets `web.port` and `KATAGO_WS_ENV`
///    the run mode.
///
/// [`crate::app!`] calls this (through [`crate::AppBuilder::named`]) with the
/// calling crate's package name.
#[must_use]
pub fn load_figment_for_app(app_name: &str) -> Figment {
    load_figment_from_for_app(app_config_path(app_name), app_name)
}

/// Like [`load_figment_for_app`], but the config-file path is supplied
/// explicitly.
#[must_use]
pub fn load_figment_from_for_app<P: AsRef<Path>>(path: P, app_name: &str) -> Figment {
    let prefix = app_env_prefix(app_name);
    load_figment_from_with_prefix(path, DEFAULT_ENV_PREFIX)
        .merge(Env::prefixed(&prefix).split("__").ignore(&["config"]))
}

/// Legacy names a named app still honours, each paired with its replacement:
/// every `MUXA_*` env var set (`("MUXA_DIESEL__URL", "KATAGO_WS_DIESEL__URL")`)
/// and, when [`app_config_path`] fell back to it, the `muxa.toml` file
/// (`("muxa.toml", "config.toml")`). Names only, never values. Sorted.
///
/// [`crate::AppBuilder::named`] logs one warning per entry.
#[must_use]
pub fn deprecated_config_names(app_name: &str) -> Vec<(String, String)> {
    let prefix = app_env_prefix(app_name);
    if prefix == DEFAULT_ENV_PREFIX {
        return Vec::new();
    }
    let mut found: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter_map(|name| {
            let rest = strip_prefix_ignore_case(&name, DEFAULT_ENV_PREFIX)?;
            let renamed = format!("{prefix}{rest}");
            Some((name, renamed))
        })
        .collect();
    if app_config_path(app_name) == Path::new(DEFAULT_CONFIG_PATH) {
        found.push((
            DEFAULT_CONFIG_PATH.to_owned(),
            DEFAULT_APP_CONFIG_PATH.to_owned(),
        ));
    }
    found.sort();
    found
}

/// `name` without `prefix`, matched case-insensitively (as figment's `Env` does).
fn strip_prefix_ignore_case<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let head = name.get(..prefix.len())?;
    let rest = name.get(prefix.len()..)?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
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

    const APP: &str = "katago-ws";

    #[test]
    fn app_env_prefix_derives_from_the_name() {
        assert_eq!(app_env_prefix("katago-ws"), "KATAGO_WS_");
        assert_eq!(app_env_prefix("my.app"), "MY_APP_");
        assert_eq!(app_env_prefix("MyApp2"), "MYAPP2_");
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn app_prefix_wins_over_legacy_prefix_and_file() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "config.toml",
                "[engine]\nmax_visits = 10\nmodel = \"file\"\nbinary = \"file\"\n",
            )?;
            jail.set_env("MUXA_ENGINE__MAX_VISITS", "20");
            jail.set_env("MUXA_ENGINE__MODEL", "muxa");

            // Only the legacy `MUXA_` var: it still beats the file.
            let fig = load_figment_for_app(APP);
            assert_eq!(fig.extract_inner::<u32>("engine.max_visits").unwrap(), 20);

            // Both set: the app prefix wins; untouched keys keep their source.
            jail.set_env("KATAGO_WS_ENGINE__MAX_VISITS", "50");
            let fig = load_figment_for_app(APP);
            assert_eq!(fig.extract_inner::<u32>("engine.max_visits").unwrap(), 50);
            assert_eq!(fig.extract_inner::<String>("engine.model").unwrap(), "muxa");
            assert_eq!(
                fig.extract_inner::<String>("engine.binary").unwrap(),
                "file"
            );
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn app_prefix_covers_framework_sections_and_run_mode() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("MUXA_ENV", "dev");
            jail.set_env("MUXA_WEB__PORT", "8080");
            let fig = load_figment_for_app(APP);
            assert_eq!(
                crate::RunMode::from_figment(&fig),
                crate::RunMode::Development
            );
            assert_eq!(fig.extract_inner::<u16>("web.port").unwrap(), 8080);

            jail.set_env("KATAGO_WS_ENV", "production");
            jail.set_env("KATAGO_WS_WEB__PORT", "3000");
            let fig = load_figment_for_app(APP);
            assert_eq!(
                crate::RunMode::from_figment(&fig),
                crate::RunMode::Production
            );
            assert_eq!(fig.extract_inner::<u16>("web.port").unwrap(), 3000);

            // The bootstrap vars never become keys; plain `load_figment` never
            // reads the app prefix.
            jail.set_env("KATAGO_WS_CONFIG", "missing.toml");
            assert!(load_figment_for_app(APP).find_value("config").is_err());
            assert_eq!(
                load_figment().extract_inner::<u16>("web.port").unwrap(),
                8080
            );
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn app_config_path_lookup_order() {
        figment::Jail::expect_with(|jail| {
            let path = || app_config_path(APP);
            assert_eq!(path(), Path::new("config.toml"), "nothing on disk");

            jail.create_file("muxa.toml", "")?;
            assert_eq!(path(), Path::new("muxa.toml"), "legacy file only");

            jail.create_file("config.toml", "")?;
            assert_eq!(
                path(),
                Path::new("config.toml"),
                "config.toml beats muxa.toml"
            );

            jail.set_env("MUXA_CONFIG", "legacy.toml");
            assert_eq!(path(), Path::new("legacy.toml"));

            jail.set_env("KATAGO_WS_CONFIG", "app.toml");
            assert_eq!(path(), Path::new("app.toml"));
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn deprecated_config_names_lists_legacy_vars_and_file() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("MUXA_ENGINE__MODEL", "m");
            jail.set_env("MUXA_ENV", "dev");
            jail.set_env("KATAGO_WS_WEB__PORT", "3000");
            assert_eq!(
                deprecated_config_names(APP),
                vec![
                    (
                        "MUXA_ENGINE__MODEL".to_owned(),
                        "KATAGO_WS_ENGINE__MODEL".to_owned()
                    ),
                    ("MUXA_ENV".to_owned(), "KATAGO_WS_ENV".to_owned()),
                ]
            );

            jail.create_file("muxa.toml", "")?;
            assert!(
                deprecated_config_names(APP)
                    .contains(&("muxa.toml".to_owned(), "config.toml".to_owned()))
            );

            // An app literally named `muxa` has nothing to migrate.
            assert!(deprecated_config_names("muxa").is_empty());
            Ok(())
        });
    }
}
