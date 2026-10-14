//! A figment provider over string variables, for hosts with no process
//! environment (Cloudflare Workers' vars and secrets, …).

use std::collections::btree_map::Entry;

use figment::value::{Dict, Map, Value};
use figment::{Metadata, Profile, Provider};

/// A figment [`Provider`] over `(name, value)` string variables.
///
/// It plays the part of figment's `Env` provider where there is no process
/// environment to read — on wasm hosts, chiefly a Cloudflare Worker, whose
/// vars and secrets arrive as properties of its `Env` object
/// ([`Vars::from_js`]). [`App::with_vars`](crate::App::with_vars) reads them
/// with the same convention muxa applies to environment variables natively:
/// the var `MUXA_PGMQ__URL` sets `pgmq.url`.
///
/// It behaves like figment's `Env`: names are trimmed and lowercased,
/// [`prefixed`](Self::prefixed) keeps only the names with a prefix (and strips
/// it), [`split`](Self::split) turns a separator into nesting,
/// [`ignore`](Self::ignore) drops keys, and a key with an empty segment
/// (`pgmq.`, `.url`) is skipped. Values are parsed the same way: `"true"` is a
/// bool, `"42"` a number, `"[1, 2]"` an array, anything else a string. The
/// data lands in the default profile, so a provider merged later still wins.
///
/// Its `Debug` output lists key names only, since values may be secrets.
#[derive(Clone)]
pub struct Vars {
    pairs: Vec<(String, String)>,
    prefix: Option<String>,
    split: Option<String>,
    ignored: Vec<String>,
}

impl Vars {
    /// Build from explicit `(name, value)` pairs.
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
            ignored: Vec::new(),
        }
    }

    /// Read the own properties of a JS object that hold a string, a number
    /// or a boolean — for a Cloudflare Worker, its `Env`
    /// (`Vars::from_js(env.as_ref())`): the `[vars]` of `wrangler.toml` and
    /// the secrets. Wrangler hands `MUXA_WEB__BANNER = false` or
    /// `MUXA_PGMQ__POOL = 4` over as a JS boolean or number; they are turned
    /// back into text (`"false"`, `"4"`) and parsed like every other var.
    /// Other properties (D1, KV and other bindings, object or array vars) are
    /// skipped. The values are copied out, so the provider holds no JS handle.
    #[cfg(target_family = "wasm")]
    #[must_use]
    pub fn from_js(object: &wasm_bindgen::JsValue) -> Self {
        use js_sys::{Array, Object};
        use wasm_bindgen::JsCast as _;

        let entries = Object::entries(object.unchecked_ref::<Object>());
        Self::from_pairs(entries.iter().filter_map(|entry| {
            let entry = Array::from(&entry);
            Some((entry.get(0).as_string()?, scalar_text(&entry.get(1))?))
        }))
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

    /// Drop these keys (case-insensitive), compared after prefixing and
    /// splitting.
    #[must_use]
    pub fn ignore(mut self, keys: &[&str]) -> Self {
        self.ignored
            .extend(keys.iter().map(|key| key.to_lowercase()));
        self
    }

    /// The `(dotted.key, raw value)` pairs after prefix filtering, splitting
    /// and ignoring — the same steps and checks as figment's `Env`.
    fn keyed(&self) -> impl Iterator<Item = (String, &str)> {
        self.pairs.iter().filter_map(|(name, value)| {
            let name = name.trim().to_lowercase();
            let name = match &self.prefix {
                Some(prefix) => name.strip_prefix(prefix.as_str())?.to_owned(),
                None => name,
            };
            let key = match &self.split {
                Some(pattern) => name.replace(pattern.as_str(), "."),
                None => name,
            };
            let key = key.trim();
            if key.split('.').any(str::is_empty)
                || self.ignored.iter().any(|ignored| ignored == key)
            {
                return None;
            }
            Some((key.to_owned(), value.as_str()))
        })
    }
}

/// A JS string, number or boolean as the text an env var would hold. `None`
/// for anything else (objects — bindings among them — arrays, `null`, …).
#[cfg(target_family = "wasm")]
fn scalar_text(value: &wasm_bindgen::JsValue) -> Option<String> {
    if let Some(text) = value.as_string() {
        return Some(text);
    }
    if let Some(flag) = value.as_bool() {
        return Some(flag.to_string());
    }
    // `f64`'s `Display` writes integers without a fraction (`4`, not `4.0`),
    // so figment parses them back as integers.
    value.as_f64().map(|number| number.to_string())
}

impl core::fmt::Debug for Vars {
    /// Names only — the values may be secrets.
    fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        fmt.debug_struct("Vars")
            .field(
                "keys",
                &self.keyed().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Provider for Vars {
    fn metadata(&self) -> Metadata {
        Metadata::named("vars")
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        // Built like figment's `Env::data`: nest each dotted key, merge the
        // nested dicts (later wins), emit to the default profile.
        let mut dict = Dict::new();
        for (key, raw) in self.keyed() {
            let Ok(value) = raw.parse::<Value>();
            if let Value::Dict(_, nested) = figment::util::nest(&key, value) {
                merge_into(&mut dict, nested);
            }
        }
        Ok(Profile::Default.collect(dict))
    }
}

/// Deep-merge `from` into `into`: dicts merge key by key, anything else is
/// replaced by the incoming value.
fn merge_into(into: &mut Dict, from: Dict) {
    for (key, value) in from {
        match into.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(value);
            }
            Entry::Occupied(mut slot) => match (slot.get_mut(), value) {
                (Value::Dict(_, existing), Value::Dict(_, incoming)) => {
                    merge_into(existing, incoming);
                }
                (current, value) => *current = value,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use figment::Figment;
    use figment::providers::Serialized;

    use super::*;

    #[test]
    fn prefix_is_stripped_and_split_nests() {
        let vars = Vars::from_pairs([
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
        let figment = Figment::from(Vars::from_pairs([("LIMIT", "40"), ("DEBUG", "true")]));
        assert_eq!(figment.extract_inner::<u32>("limit").unwrap(), 40);
        assert!(figment.extract_inner::<bool>("debug").unwrap());
    }

    #[test]
    fn nested_keys_merge_and_bad_keys_are_skipped() {
        let vars = Vars::from_pairs([
            ("MUXA_PGMQ__URL", "postgres://example"),
            ("MUXA_PGMQ__POOL", "4"),
            (" MUXA_WEB__PORT ", "8080"),
            ("MUXA_PGMQ__", "empty last segment"),
            ("MUXA___URL", "empty first segment"),
            ("MUXA_CONFIG", "names the config file natively"),
        ])
        .prefixed("MUXA_")
        .split("__")
        .ignore(&["config"]);
        let figment = Figment::from(vars);

        assert_eq!(
            figment.extract_inner::<String>("pgmq.url").unwrap(),
            "postgres://example"
        );
        assert_eq!(figment.extract_inner::<u32>("pgmq.pool").unwrap(), 4);
        assert_eq!(figment.extract_inner::<u16>("web.port").unwrap(), 8080);
        // Only well-formed keys made it: no `""` segments, no `config`.
        let root = figment.data().unwrap().remove(&Profile::Default).unwrap();
        assert_eq!(root.keys().collect::<Vec<_>>(), ["pgmq", "web"]);
        let pgmq = figment.extract_inner::<Dict>("pgmq").unwrap();
        assert_eq!(pgmq.keys().collect::<Vec<_>>(), ["pool", "url"]);
    }

    #[test]
    fn a_provider_merged_later_wins() {
        let figment = Figment::from(Vars::from_pairs([("LIMIT", "40")]))
            .merge(Serialized::default("limit", 7));
        assert_eq!(figment.extract_inner::<u32>("limit").unwrap(), 7);
    }

    #[test]
    fn debug_shows_names_not_values() {
        let vars = Vars::from_pairs([("API_TOKEN", "hunter2")]);
        let debug = format!("{vars:?}");
        assert!(debug.contains("api_token"), "{debug}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }
}
