//! Coarse application run mode — **development** vs **production** — resolved
//! once at startup and shared via [`BuildCtx`](crate::BuildCtx) so every plugin
//! branches on the same source of truth.

use std::str::FromStr;

use serde::Serialize;

/// Whether the application is running in **development** or **production**.
///
/// Resolved once when the [`BuildCtx`](crate::BuildCtx) is created and exposed
/// as [`BuildCtx::mode`](crate::BuildCtx::mode), so plugins don't each re-derive
/// it. Resolution order (see [`from_figment`](RunMode::from_figment)):
///
/// 1. The top-level `env` config key — settable in the TOML file
///    (`env = "production"`) or via the prefixed env var (`MUXA_ENV=production`).
///    Case-insensitive; accepts `dev`/`development`/`debug` and
///    `prod`/`production`/`release`.
/// 2. Otherwise the compiled **build profile**: a debug build ⇒ [`Development`],
///    a release build ⇒ [`Production`] (via `cfg!(debug_assertions)`).
///
/// muxa-sentry reads it to default the Sentry `environment` and transaction
/// sample rate; apps can read it for launch banners, verbose logging, seed
/// data, stricter timeouts, etc.
///
/// [`Development`]: RunMode::Development
/// [`Production`]: RunMode::Production
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    /// Local/dev: verbose, permissive defaults (e.g. full trace sampling).
    Development,
    /// Production: conservative defaults (e.g. light trace sampling).
    Production,
}

impl RunMode {
    /// Resolve from a figment's top-level `env` key, falling back to the build
    /// profile when it's absent or unrecognized. See the [type docs](RunMode)
    /// for the full order.
    #[must_use]
    pub fn from_figment(figment: &figment::Figment) -> Self {
        figment
            .extract_inner::<String>("env")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or_else(Self::from_build_profile)
    }

    /// Deduce purely from the compiled build profile: debug ⇒ [`Development`],
    /// release ⇒ [`Production`]. The startup fallback when `env` is unset.
    ///
    /// [`Development`]: RunMode::Development
    /// [`Production`]: RunMode::Production
    #[must_use]
    pub fn from_build_profile() -> Self {
        if cfg!(debug_assertions) {
            Self::Development
        } else {
            Self::Production
        }
    }

    /// Canonical lowercase label — `"development"` or `"production"`. Suitable
    /// as a Sentry `environment`, a log field, or an API response value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
        }
    }

    /// `true` in production.
    #[must_use]
    pub fn is_production(self) -> bool {
        matches!(self, Self::Production)
    }

    /// `true` in development.
    #[must_use]
    pub fn is_development(self) -> bool {
        matches!(self, Self::Development)
    }
}

impl std::fmt::Display for RunMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error from [`RunMode`]'s [`FromStr`] for an unrecognized label.
#[derive(Debug, thiserror::Error)]
#[error("unknown run mode {0:?}; expected one of: development, production")]
pub struct ParseRunModeError(String);

impl FromStr for RunMode {
    type Err = ParseRunModeError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" | "debug" => Ok(Self::Development),
            "production" | "prod" | "release" => Ok(Self::Production),
            other => Err(ParseRunModeError(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_and_aliases_case_insensitively() {
        for raw in ["development", "Dev", " DEBUG "] {
            assert_eq!(raw.parse::<RunMode>().unwrap(), RunMode::Development);
        }
        for raw in ["production", "Prod", " RELEASE "] {
            assert_eq!(raw.parse::<RunMode>().unwrap(), RunMode::Production);
        }
        assert!("staging".parse::<RunMode>().is_err());
    }

    #[test]
    fn as_str_round_trips_through_from_str() {
        for mode in [RunMode::Development, RunMode::Production] {
            assert_eq!(mode.as_str().parse::<RunMode>().unwrap(), mode);
        }
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn figment_env_key_overrides_build_profile() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("MUXA_ENV", "production");
            let fig = crate::config::load_figment();
            assert_eq!(RunMode::from_figment(&fig), RunMode::Production);

            jail.set_env("MUXA_ENV", "dev");
            let fig = crate::config::load_figment();
            assert_eq!(RunMode::from_figment(&fig), RunMode::Development);
            Ok(())
        });
    }

    #[test]
    #[allow(
        clippy::result_large_err,
        reason = "Jail::expect_with's closure must return Result<_, figment::Error>"
    )]
    fn figment_without_env_falls_back_to_build_profile() {
        figment::Jail::expect_with(|_jail| {
            let fig = crate::config::load_figment();
            assert_eq!(RunMode::from_figment(&fig), RunMode::from_build_profile());
            Ok(())
        });
    }
}
