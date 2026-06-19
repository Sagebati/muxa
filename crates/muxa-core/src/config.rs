//! figment-based configuration loading.

use std::path::PathBuf;

use figment::Figment;
use figment::providers::{Env, Format as _, Toml};

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
}
