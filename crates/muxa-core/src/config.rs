//! figment-based configuration loading.

use std::path::Path;

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

/// Build the application figment with an **app env prefix** layered on top of
/// the framework's `MUXA_` one.
///
/// For apps whose own config sections (`[engine]`, `[worker]`, …) aren't
/// muxa's and shouldn't live under `MUXA_`, while muxa's plugin sections
/// (`[web]`, `[diesel]`, `[otel]`, …) and the `env` run mode keep `MUXA_`.
///
/// Layers (last wins):
/// 1. A single TOML file: `$<app_prefix>CONFIG` if set, else `$MUXA_CONFIG`,
///    else `./muxa.toml`. Missing file is fine.
/// 2. `MUXA_*` env vars, exactly as [`load_figment`].
/// 3. `<app_prefix>*` env vars, `__`-nested onto the same tree, but **only**
///    for nested keys under `app_sections`. e.g. with prefix `MYAPP_` and
///    sections `["engine"]`, `MYAPP_ENGINE__MAX_VISITS` maps to
///    `engine.max_visits`, while `MYAPP_ENV` / `MYAPP_WEB__PORT` are ignored,
///    so the app prefix can't touch the run mode or a framework section.
///
/// `MUXA_<SECTION>__*` still feeds app sections at lower precedence, so
/// existing deployments keep working; see [`legacy_app_env_vars`] to find
/// and warn about them.
pub fn load_figment_with_app_prefix(app_prefix: &str, app_sections: &[&str]) -> Figment {
    match [
        format!("{app_prefix}CONFIG"),
        format!("{DEFAULT_ENV_PREFIX}CONFIG"),
    ]
    .into_iter()
    .find_map(std::env::var_os)
    {
        Some(path) => load_figment_from_with_app_prefix(path, app_prefix, app_sections),
        None => load_figment_from_with_app_prefix(DEFAULT_CONFIG_PATH, app_prefix, app_sections),
    }
}

/// Like [`load_figment_with_app_prefix`], but the config-file path is supplied
/// explicitly (neither `{app_prefix}CONFIG` nor `MUXA_CONFIG` is consulted).
pub fn load_figment_from_with_app_prefix<P: AsRef<Path>>(
    path: P,
    app_prefix: &str,
    app_sections: &[&str],
) -> Figment {
    let sections: Vec<String> = app_sections.iter().map(|sec| (*sec).to_owned()).collect();
    load_figment_from_with_prefix(path, DEFAULT_ENV_PREFIX).merge(
        Env::prefixed(app_prefix).split("__").filter(move |key| {
            // `split` already ran, so `ENGINE__MAX_VISITS` arrives as
            // `engine.max_visits` (case not yet folded).
            key.as_str()
                .split_once('.')
                .is_some_and(|(head, _)| sections.iter().any(|sec| sec.eq_ignore_ascii_case(head)))
        }),
    )
}

/// `MUXA_*` env vars that set one of `app_sections`, each paired with the
/// `<app_prefix>*` name that now owns it, e.g.
/// `("MUXA_ENGINE__MODEL", "KATAGO_WS_ENGINE__MODEL")`. Sorted by name.
///
/// They still apply (below the app prefix, see
/// [`load_figment_with_app_prefix`]); this lets the app log a deprecation
/// once its tracing subscriber is up.
#[must_use]
pub fn legacy_app_env_vars(app_prefix: &str, app_sections: &[&str]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter_map(|name| {
            let rest = strip_prefix_ignore_case(&name, DEFAULT_ENV_PREFIX)?;
            let (head, _) = rest.split_once("__")?;
            let renamed = format!("{app_prefix}{rest}");
            app_sections
                .iter()
                .any(|sec| sec.eq_ignore_ascii_case(head))
                .then_some((name, renamed))
        })
        .collect();
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

    const APP: &str = "MYAPP_";
    const SECTIONS: &[&str] = &["engine", "worker"];

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn app_prefix_wins_over_muxa_prefix_and_file() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "muxa.toml",
                "[engine]\nmax_visits = 10\nmodel = \"file\"\nbinary = \"file\"\n",
            )?;
            jail.set_env("MUXA_ENGINE__MAX_VISITS", "20");
            jail.set_env("MUXA_ENGINE__MODEL", "muxa");

            // Only the legacy `MUXA_` var: it still beats the file.
            let fig = load_figment_with_app_prefix(APP, SECTIONS);
            assert_eq!(fig.extract_inner::<u32>("engine.max_visits").unwrap(), 20);

            // Both set: the app prefix wins; untouched keys keep their source.
            jail.set_env("MYAPP_ENGINE__MAX_VISITS", "50");
            let fig = load_figment_with_app_prefix(APP, SECTIONS);
            assert_eq!(fig.extract_inner::<u32>("engine.max_visits").unwrap(), 50);
            assert_eq!(fig.extract_inner::<String>("engine.model").unwrap(), "muxa");
            assert_eq!(
                fig.extract_inner::<String>("engine.binary").unwrap(),
                "file"
            );

            // Case-insensitive, like figment's own prefix matching.
            jail.set_env("myapp_Worker__Slots", "3");
            let fig = load_figment_with_app_prefix(APP, SECTIONS);
            assert_eq!(fig.extract_inner::<u32>("worker.slots").unwrap(), 3);
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn app_prefix_does_not_bleed_into_framework_keys() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("MUXA_ENV", "dev");
            jail.set_env("MUXA_WEB__PORT", "8080");
            jail.set_env("MYAPP_ENV", "production");
            jail.set_env("MYAPP_ROLE", "worker");
            jail.set_env("MYAPP_WEB__PORT", "1");
            jail.set_env("MYAPP_ENGINE", "scalar");

            let fig = load_figment_with_app_prefix(APP, SECTIONS);
            // The run mode stays on `MUXA_ENV`.
            assert_eq!(
                crate::RunMode::from_figment(&fig),
                crate::RunMode::Development
            );
            assert_eq!(fig.extract_inner::<String>("env").unwrap(), "dev");
            // Framework sections stay on `MUXA_`.
            assert_eq!(fig.extract_inner::<u16>("web.port").unwrap(), 8080);
            // Non-section / un-nested app vars never reach the figment.
            assert!(fig.find_value("role").is_err());
            assert!(fig.find_value("engine").is_err());
            assert!(fig.find_value("config").is_err());

            // And the plain `MUXA_` loader never reads the app prefix.
            jail.set_env("MYAPP_ENGINE__MAX_VISITS", "50");
            assert!(load_figment().find_value("engine.max_visits").is_err());
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn app_config_var_selects_file_ahead_of_muxa_config() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("muxa.toml", "[engine]\nmodel = \"default\"\n")?;
            jail.create_file("muxa-cfg.toml", "[engine]\nmodel = \"muxa-config\"\n")?;
            jail.create_file("app-cfg.toml", "[engine]\nmodel = \"app-config\"\n")?;
            let model = || {
                load_figment_with_app_prefix(APP, SECTIONS)
                    .extract_inner::<String>("engine.model")
                    .unwrap()
            };

            assert_eq!(model(), "default");
            jail.set_env("MUXA_CONFIG", "muxa-cfg.toml");
            assert_eq!(model(), "muxa-config");
            jail.set_env("MYAPP_CONFIG", "app-cfg.toml");
            assert_eq!(model(), "app-config");
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn legacy_app_env_vars_lists_only_app_sections() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("MUXA_ENGINE__MODEL", "m");
            jail.set_env("MUXA_WORKER__SLOTS", "2");
            jail.set_env("MUXA_WEB__PORT", "8080");
            jail.set_env("MUXA_ENV", "dev");
            jail.set_env("MUXA_ENGINEX__MODEL", "not-a-section");
            jail.set_env("MYAPP_ENGINE__MODEL", "m");

            assert_eq!(
                legacy_app_env_vars(APP, SECTIONS),
                vec![
                    (
                        "MUXA_ENGINE__MODEL".to_owned(),
                        "MYAPP_ENGINE__MODEL".to_owned()
                    ),
                    (
                        "MUXA_WORKER__SLOTS".to_owned(),
                        "MYAPP_WORKER__SLOTS".to_owned()
                    ),
                ]
            );
            Ok(())
        });
    }
}
