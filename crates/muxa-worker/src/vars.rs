//! A figment provider over Worker vars.

use figment::value::{Dict, Map, Value};
use figment::{Figment, Metadata, Profile, Provider};
use worker::Env;
use worker::js_sys::{Array, Object};
use worker::wasm_bindgen::{JsCast as _, JsValue};

/// A figment [`Provider`] that reads config from Worker **vars** (the
/// `[vars]` table of `wrangler.toml`) and **secrets** — every string-valued
/// property of the Worker [`Env`].
///
/// It mirrors figment's `Env` provider, which is what muxa uses natively:
/// keys are lowercased, [`prefixed`](Self::prefixed) keeps only the keys
/// with a prefix (and strips it), and [`split`](Self::split) turns a
/// separator into nesting. With `.prefixed("MUXA_").split("__")`, the var
/// `MUXA_PGMQ__URL` maps to `pgmq.url` — the same convention as muxa's
/// environment variables.
///
/// Values are parsed like figment's `Env` values: `"true"` is a bool, `"42"`
/// a number, `"[1, 2]"` an array, anything else a string.
///
/// The values are copied out of the `Env` at construction, so the provider
/// itself holds no JS handle.
///
/// ```ignore
/// let figment = Figment::from(WorkerVars::from_env(&env).prefixed("MUXA_").split("__"));
/// let app = App::with_figment(figment);
/// ```
#[derive(Clone)]
pub struct WorkerVars {
    pairs: Vec<(String, String)>,
    prefix: Option<String>,
    split: Option<String>,
}

impl WorkerVars {
    /// Read every string-valued binding (vars and secrets) from the `Env`.
    /// Non-string bindings (D1, KV, JSON vars, …) are skipped.
    #[must_use]
    pub fn from_env(env: &Env) -> Self {
        let value: &JsValue = env.as_ref();
        let pairs = Object::entries(value.unchecked_ref::<Object>())
            .iter()
            .filter_map(|entry| {
                let entry = Array::from(&entry);
                Some((entry.get(0).as_string()?, entry.get(1).as_string()?))
            })
            .collect();
        Self {
            pairs,
            prefix: None,
            split: None,
        }
    }

    /// Build from explicit `(name, value)` pairs — for tests, or for hosts
    /// that expose vars some other way.
    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            pairs: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            prefix: None,
            split: None,
        }
    }

    /// Keep only the vars whose name starts with `prefix` (case-insensitive)
    /// and strip it. The prefix should include its trailing separator.
    #[must_use]
    pub fn prefixed(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.to_lowercase());
        self
    }

    /// Split var names on `pattern` to form nested keys:
    /// with `"__"`, `PGMQ__URL` becomes `pgmq.url`.
    #[must_use]
    pub fn split(mut self, pattern: &str) -> Self {
        self.split = Some(pattern.to_lowercase());
        self
    }

    /// The `(dotted.key, raw value)` pairs after prefix filtering and
    /// splitting.
    fn keyed(&self) -> impl Iterator<Item = (String, &str)> {
        self.pairs.iter().filter_map(|(name, value)| {
            let name = name.to_lowercase();
            let name = match &self.prefix {
                Some(prefix) => name.strip_prefix(prefix.as_str())?.to_owned(),
                None => name,
            };
            let key = match &self.split {
                Some(pattern) => name.replace(pattern.as_str(), "."),
                None => name,
            };
            (!key.is_empty()).then_some((key, value.as_str()))
        })
    }
}

impl core::fmt::Debug for WorkerVars {
    /// Names only — the values may be secrets.
    fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        fmt.debug_struct("WorkerVars")
            .field(
                "keys",
                &self.keyed().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Provider for WorkerVars {
    fn metadata(&self) -> Metadata {
        Metadata::named("Worker vars")
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        // Merging `(dotted.key, value)` pairs through a figment nests and
        // coalesces them exactly like the built-in providers.
        let mut figment = Figment::new();
        for (key, raw) in self.keyed() {
            let value = match raw.parse::<Value>() {
                Ok(value) => value,
                Err(never) => match never {},
            };
            figment = figment.merge((key, value));
        }
        figment.data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_stripped_and_split_nests() {
        let vars = WorkerVars::from_pairs([
            ("MUXA_PGMQ__URL", "postgres://example"),
            ("MUXA_ENV", "production"),
            ("OTHER", "ignored"),
        ])
        .prefixed("MUXA_")
        .split("__");
        let figment = Figment::from(vars);

        assert_eq!(
            figment.extract_inner::<String>("pgmq.url").unwrap(),
            "postgres://example"
        );
        assert_eq!(
            figment.extract_inner::<String>("env").unwrap(),
            "production"
        );
        assert!(figment.extract_inner::<String>("other").is_err());
    }

    #[test]
    fn values_are_parsed_like_env_vars() {
        let figment = Figment::from(WorkerVars::from_pairs([("LIMIT", "40"), ("DEBUG", "true")]));
        assert_eq!(figment.extract_inner::<u32>("limit").unwrap(), 40);
        assert!(figment.extract_inner::<bool>("debug").unwrap());
    }

    #[test]
    fn debug_shows_names_not_values() {
        let vars = WorkerVars::from_pairs([("API_TOKEN", "hunter2")]);
        let debug = format!("{vars:?}");
        assert!(debug.contains("api_token"), "{debug}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }
}
