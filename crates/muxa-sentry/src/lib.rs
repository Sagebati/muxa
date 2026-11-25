//! muxa-sentry — Sentry SDK integration.
//!
//! Three responsibilities:
//!
//! 1. Initialize the native Sentry SDK (panic capture, error reporting,
//!    transactions). The `ClientInitGuard` is held in plugin output so
//!    events flush on shutdown.
//! 2. Install `sentry-tower` middleware (per-request scope + HTTP context
//!    capture + optional transactions for performance monitoring) on the
//!    router. Transactions are named after the matched route template
//!    (`GET /jobs/{id}`), so Sentry groups them per endpoint.
//! 3. **With the `tracing-bridge` feature**: push a `TracingBridge` layer
//!    onto the shared tracing subscriber so `tracing` spans and events flow
//!    into Sentry as transactions / error events / breadcrumbs / logs.
//!
//! Database traces, metrics, and logs that arrive through `tracing`
//! (e.g. sqlx's per-query spans when its `tracing` feature is on) are
//! routed to Sentry the same way — there's nothing Sentry-specific to do
//! per-data-source.
//!
//! ```ignore
//! use muxa::prelude::*;
//! use muxa_sentry::SentryPlugin;
//!
//! #[tokio::main]
//! async fn main() -> muxa::Result<()> {
//!     muxa::app!()
//!         .with_plugin(SentryPlugin).await?
//!         .with_plugin(OtelPlugin).await?
//!         // ... add WebPlugin last
//!         .run().await
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::borrow::Cow;
use std::collections::HashMap;

use bon::Builder;
use muxa_core::{BuildCtx, EnvFallback, Error, Plugin, Result, RunMode, State};
use secrecy::{ExposeSecret as _, SecretString};
use sentry::ClientInitGuard;
use sentry_tower::{NewSentryLayer, SentryHttpLayer};
use serde::Deserialize;

/// Configuration for [`SentryPlugin`]. Read from `[sentry]`; the SDK's own
/// `SENTRY_DSN`, `SENTRY_ENVIRONMENT` and `SENTRY_RELEASE` fill `dsn`,
/// `environment` and `release` when the app's configuration leaves them unset.
///
/// `Default` is hand-written (not derived) so the values match the bon
/// `#[builder(default = …)]` attributes. This matters because `#[serde(default)]`
/// fills missing config fields from `Default` — a derived `Default` would make
/// `bool` fields `false`, silently disabling `attach_stacktrace`,
/// `http_transactions`, and `logs` whenever they're absent from `[sentry]`.
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct SentryConfig {
    /// Sentry DSN. If `None`/empty, the SDK initializes a no-op client and
    /// no events are sent — useful for development. A DSN that doesn't parse
    /// fails the build instead of silently disabling Sentry. A [`SecretString`]
    /// so it's redacted in `Debug`/logs; exposed only to parse it at init.
    pub dsn: Option<SecretString>,
    /// Logical environment (e.g. `"development"`, `"production"`). When unset,
    /// it defaults to the app-wide [`RunMode`](muxa_core::RunMode) label
    /// (`BuildCtx::mode`), so Sentry events are always tagged with an
    /// environment. Set it to override (e.g. `"staging"`).
    pub environment: Option<String>,
    /// Release identifier (e.g. `"my-app@1.2.3"`).
    pub release: Option<String>,
    /// Fraction of transactions to sample for performance monitoring.
    /// `0.0` disables performance; `1.0` samples every request.
    ///
    /// When unset, a sane default is chosen from the app-wide
    /// [`RunMode`](muxa_core::RunMode) (see [`default_traces_sample_rate`]):
    /// production samples a small fraction (cost/quota control), development
    /// samples everything (full local visibility). Set it explicitly — including
    /// `0.0` — to override.
    pub traces_sample_rate: Option<f32>,
    /// Per-transaction overrides of [`traces_sample_rate`](Self::traces_sample_rate),
    /// keyed by the exact transaction name — `"GET /healthz"` (method + route
    /// template) for a request, the span name for a `tracing` root span. `0.0`
    /// drops a noisy transaction (a probe, a polled endpoint) entirely.
    ///
    /// A transaction continuing an incoming trace keeps its parent's sampling
    /// decision either way, so a distributed trace is never cut in half. Muxa's
    /// own `muxa.task` (a background task's lifetime span) defaults to `0.0`.
    #[builder(default)]
    pub transaction_sample_rates: HashMap<String, f32>,
    /// Attach stack traces to every captured event.
    #[builder(default = true)]
    pub attach_stacktrace: bool,
    /// Send default PII (e.g. user IP, cookies). Usually `false` for
    /// privacy.
    #[builder(default = false)]
    pub send_default_pii: bool,
    /// Whether to wrap each request in a Sentry performance transaction.
    /// Has no effect when the resolved `traces_sample_rate` is `0.0`.
    #[builder(default = true)]
    pub http_transactions: bool,
    /// Capture `tracing` events (INFO/WARN/ERROR) as Sentry **structured logs**,
    /// queryable in the Logs explorer. Independent of tracing/transactions.
    ///
    /// Requires the `tracing-bridge` feature (which carries the `logs` cargo
    /// feature) — without it this flag is inert. On by default once a DSN is
    /// set; note every INFO+ event is shipped, so it consumes Sentry log quota.
    /// Set to `false` to disable.
    #[builder(default = true)]
    pub logs: bool,
}

impl Default for SentryConfig {
    /// Keep these in lockstep with the `#[builder(default = …)]` attributes
    /// above — `#[serde(default)]` uses this impl for fields omitted from config.
    fn default() -> Self {
        Self {
            dsn: None,
            environment: None,
            release: None,
            traces_sample_rate: None,
            transaction_sample_rates: HashMap::new(),
            attach_stacktrace: true,
            send_default_pii: false,
            http_transactions: true,
            logs: true,
        }
    }
}

/// Sane default for [`SentryConfig::traces_sample_rate`] when it isn't set
/// explicitly, keyed off the app-wide [`RunMode`]: production keeps a small
/// fraction to bound ingest cost/quota; development samples every transaction
/// for full visibility. A non-zero default means performance monitoring is on
/// by default once a DSN is set — set the field to `0.0` to opt back out.
#[must_use]
pub fn default_traces_sample_rate(mode: RunMode) -> f32 {
    match mode {
        RunMode::Production => 0.1,
        RunMode::Development => 1.0,
    }
}

/// Transactions sampled at a fixed rate unless
/// [`SentryConfig::transaction_sample_rates`] says otherwise.
///
/// `muxa.task` is the span each background task runs in: it lives as long as
/// the app, so as a transaction it would reach Sentry only at shutdown, cut at
/// the SDK's 1000-span cap. Work in a task that should be traced starts a trace
/// of its own (a fresh hub with no active span).
const DEFAULT_TRANSACTION_SAMPLE_RATES: &[(&str, f32)] = &[("muxa.task", 0.0)];

/// The sample rate for one transaction: the parent's decision when it continues
/// an incoming trace, else its [`SentryConfig::transaction_sample_rates`] entry,
/// else `default_rate`.
fn sample_rate_for(
    rates: &HashMap<String, f32>,
    default_rate: f32,
    name: &str,
    parent_sampled: Option<bool>,
) -> f32 {
    parent_sampled.map_or_else(
        || rates.get(name).copied().unwrap_or(default_rate),
        f32::from,
    )
}

/// Plugin output handle.
///
/// Holds the `ClientInitGuard` so Sentry's worker thread isn't dropped
/// while the app is running — events flush in `Drop`.
pub struct SentryHandle {
    _guard: ClientInitGuard,
}

/// The Sentry plugin.
#[derive(Default)]
pub struct SentryPlugin;

impl<S: State> Plugin<S> for SentryPlugin {
    type Output = SentryHandle;
    type Config = SentryConfig;
    const CONFIG_PREFIX: &'static str = "sentry";
    const ENV_FALLBACKS: &'static [EnvFallback] = &[
        EnvFallback {
            var: "SENTRY_DSN",
            key: "dsn",
        },
        EnvFallback {
            var: "SENTRY_ENVIRONMENT",
            key: "environment",
        },
        EnvFallback {
            var: "SENTRY_RELEASE",
            key: "release",
        },
    ];

    async fn build(
        self,
        cfg: SentryConfig,
        _state: &S,
        ctx: &mut BuildCtx,
    ) -> Result<SentryHandle> {
        // Treat empty string as None. The DSN is a secret — expose it only to
        // parse it here, never to logs (the parse error doesn't echo it).
        let dsn = cfg
            .dsn
            .as_ref()
            .map(|secret| secret.expose_secret())
            .filter(|raw| !raw.is_empty())
            .map(|raw| {
                raw.parse::<sentry::types::Dsn>().map_err(|err| {
                    Error::other(format!(
                        "muxa-sentry: `sentry.dsn` is not a valid DSN: {err}"
                    ))
                })
            })
            .transpose()?;

        // The app-wide run mode is the source of truth for env-aware defaults.
        let mode = ctx.mode;
        // Default the environment label to the run mode; an explicit config wins.
        let environment = cfg
            .environment
            .map_or(Cow::Borrowed(mode.as_str()), Cow::Owned);
        // Unset rate ⇒ pick from the run mode (prod: light sampling, dev: full).
        // An explicit value (including 0.0, to disable) wins.
        let traces_sample_rate = cfg
            .traces_sample_rate
            .unwrap_or_else(|| default_traces_sample_rate(mode));

        // Structured logs: ship INFO+ tracing events to Sentry's Logs product.
        // They come from the tracing bridge's log layer, so they exist only
        // with the `tracing-bridge` feature — gate on it to keep the startup
        // log honest.
        let enable_logs = cfg.logs && cfg!(feature = "tracing-bridge");

        // Per-transaction rates: the built-in defaults, then the app's overrides.
        let overrides = cfg.transaction_sample_rates.len();
        let mut rates: HashMap<String, f32> = DEFAULT_TRANSACTION_SAMPLE_RATES
            .iter()
            .map(|&(name, rate)| (name.to_owned(), rate))
            .collect();
        rates.extend(cfg.transaction_sample_rates);

        // The sampler covers the base rate too (`traces_sample_rate` is its
        // fallback), so no separate fixed-rate strategy is set.
        let mut options = sentry::ClientOptions::new()
            .environment(environment)
            .maybe_release(cfg.release)
            .traces_sampler(move |tx: &sentry::TransactionContext| {
                sample_rate_for(&rates, traces_sample_rate, tx.name(), tx.sampled())
            })
            .attach_stacktrace(cfg.attach_stacktrace)
            .send_default_pii(cfg.send_default_pii);
        // Assigned, not `.dsn(&str)`: that one panics on a bad DSN, and this
        // one was already parsed (and failed the build) above.
        options.dsn = dsn;

        let guard = sentry::init(options);

        // Tower middleware. Axum applies the last-added layer outermost,
        // so order matters: `SentryHttpLayer` first (inner) and
        // `NewSentryLayer` second (outer — the request gets its Sentry scope
        // before `SentryHttpLayer` runs). Relative to *other* plugins'
        // middleware the position follows plugin order: a plugin added after
        // this one wraps it.
        let http_transactions = cfg.http_transactions;
        ctx.router.layer(move |router| {
            let http_layer = if http_transactions {
                SentryHttpLayer::new().enable_transaction()
            } else {
                SentryHttpLayer::new()
            };
            router
                .layer(http_layer)
                .layer(NewSentryLayer::<axum::extract::Request>::new_from_top())
        });

        // tracing→Sentry bridge: attach the layer to the shared subscriber.
        // No subscriber init here — there is one subscriber per process and
        // `ctx.telemetry` is the way onto it.
        #[cfg(feature = "tracing-bridge")]
        {
            // Sentry ships events over reqwest (→ hyper → h2, through tower).
            // Keep what that transport logs away from the bridge, or it would
            // report its own reporting.
            ctx.telemetry
                .exclude_targets(&["h2", "hyper", "hyper_util", "reqwest", "tower"]);
            ctx.telemetry.add_layer(TracingBridge::new(enable_logs));
        }

        // Whether events are actually sent.
        if guard.is_enabled() {
            let options = guard.options();
            tracing::info!(
                env = options.environment.as_deref().unwrap_or("-"),
                release = options.release.as_deref().unwrap_or("-"),
                traces_sample_rate,
                transaction_rate_overrides = overrides,
                logs = enable_logs,
                "muxa-sentry: initialized"
            );
        } else {
            tracing::info!("muxa-sentry: no DSN set, running as no-op client");
        }

        Ok(SentryHandle { _guard: guard })
    }
}

/// The `tracing` → Sentry bridge: sentry-tracing's four layers behind one
/// level routing, the one sentry-tracing's legacy combined layer had.
///
/// - `ERROR`/`WARN`/`INFO` spans → transactions and spans;
/// - `ERROR` events → error events (Sentry issues);
/// - `WARN`/`INFO` events → breadcrumbs, sent with the next error;
/// - `ERROR`/`WARN`/`INFO` events → structured logs, when `[sentry].logs` is on.
///
/// The routing is done here rather than with a `with_filter` per layer:
/// muxa-telemetry adds plugin layers to a reload slot after the subscriber is
/// built, and a per-layer filter added that way never gets its `FilterId`.
#[cfg(feature = "tracing-bridge")]
pub struct TracingBridge {
    spans: sentry_tracing::SpanLayer,
    breadcrumbs: sentry_tracing::BreadcrumbLayer,
    errors: sentry_tracing::ErrorLayer,
    logs: Option<sentry_tracing::LogLayer>,
}

#[cfg(feature = "tracing-bridge")]
impl TracingBridge {
    /// The bridge; `logs` adds the structured-logs layer.
    pub fn new(logs: bool) -> Self {
        Self {
            spans: sentry_tracing::SpanLayer::new(),
            breadcrumbs: sentry_tracing::BreadcrumbLayer::new(),
            errors: sentry_tracing::ErrorLayer::new(),
            logs: logs.then(sentry_tracing::LogLayer::new),
        }
    }
}

#[cfg(feature = "tracing-bridge")]
mod bridge {
    use tracing::{Event, Level, Subscriber, span};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::registry::LookupSpan;

    use super::TracingBridge;

    /// `INFO` or more severe (tracing orders levels by verbosity).
    fn captured(level: Level) -> bool {
        level <= Level::INFO
    }

    impl<S> Layer<S> for TracingBridge
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
            let level = *event.metadata().level();
            if !captured(level) {
                return;
            }
            // Breadcrumbs before errors, so an event that is both would be
            // in its own error's trail (none is, with this routing).
            if level == Level::ERROR {
                self.errors.on_event(event, ctx.clone());
            } else {
                self.breadcrumbs.on_event(event, ctx.clone());
            }
            if let Some(logs) = &self.logs {
                logs.on_event(event, ctx);
            }
        }

        fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
            // A span skipped here has no Sentry data, which the span layer's
            // other hooks treat as "not mine".
            if captured(*attrs.metadata().level()) {
                self.spans.on_new_span(attrs, id, ctx);
            }
        }

        fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
            self.spans.on_enter(id, ctx);
        }

        fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
            self.spans.on_exit(id, ctx);
        }

        fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
            self.spans.on_close(id, ctx);
        }

        fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
            self.spans.on_record(id, values, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use figment::Figment;
    use figment::providers::{Format as _, Toml};
    use muxa_core::App;

    use std::collections::HashMap;

    use super::{SentryConfig, SentryPlugin, sample_rate_for};

    /// A named transaction takes its override, any other the default rate, and
    /// one continuing a trace its parent's decision — even over an override.
    #[test]
    fn sample_rate_prefers_parent_then_override_then_default() {
        let rates = HashMap::from([("GET /healthz".to_owned(), 0.0)]);
        assert_eq!(sample_rate_for(&rates, 0.5, "GET /healthz", None), 0.0);
        assert_eq!(sample_rate_for(&rates, 0.5, "GET /jobs/{id}", None), 0.5);
        assert_eq!(
            sample_rate_for(&rates, 0.5, "GET /healthz", Some(true)),
            1.0
        );
        assert_eq!(
            sample_rate_for(&rates, 0.5, "GET /jobs/{id}", Some(false)),
            0.0
        );
    }

    /// The overrides read from a `[sentry.transaction_sample_rates]` table, whose
    /// keys (`METHOD /route`) need TOML's quoted form.
    #[test]
    fn transaction_sample_rates_read_from_toml() {
        let config = Toml::string(
            "[sentry]\ntraces_sample_rate = 1.0\n\n[sentry.transaction_sample_rates]\n\"GET /healthz\" = 0.0\n",
        );
        let cfg: SentryConfig = Figment::from(config).extract_inner("sentry").unwrap();
        assert_eq!(cfg.transaction_sample_rates.get("GET /healthz"), Some(&0.0));
        assert_eq!(cfg.traces_sample_rate, Some(1.0));
    }

    /// `#[serde(default)]` fills omitted fields from `Default` — which must be
    /// the hand-written impl, not a derived all-`false`. Regression guard: an
    /// empty `[sentry]` table must still enable transactions, stacktraces, and
    /// logs (the bon builder defaults), not silently disable them.
    #[test]
    fn empty_config_keeps_boolean_defaults_on() {
        let cfg: SentryConfig = serde_json::from_str("{}").expect("empty object deserializes");
        assert!(cfg.http_transactions, "http_transactions must default true");
        assert!(cfg.attach_stacktrace, "attach_stacktrace must default true");
        assert!(cfg.logs, "logs must default true");
        assert!(!cfg.send_default_pii, "send_default_pii must default false");
        assert!(cfg.traces_sample_rate.is_none(), "sample rate stays unset");
        assert!(cfg.transaction_sample_rates.is_empty(), "no rate overrides");
    }

    /// An explicit `false` from config must still win over the default.
    #[test]
    fn explicit_false_overrides_default() {
        let cfg: SentryConfig =
            serde_json::from_str(r#"{"logs": false, "http_transactions": false}"#).unwrap();
        assert!(!cfg.logs);
        assert!(!cfg.http_transactions);
        assert!(cfg.attach_stacktrace, "untouched field keeps its default");
    }

    /// A DSN that doesn't parse fails the build rather than quietly disabling
    /// Sentry — and the error doesn't leak the DSN.
    #[tokio::test]
    async fn invalid_dsn_fails_the_build() {
        let config = Toml::string("[sentry]\ndsn = \"htps//secret-key@o1.ingest.sentry.io\"\n");
        let result = App::with_figment(Figment::from(config))
            .with_plugin(SentryPlugin)
            .await;
        let Err(err) = result else {
            panic!("an invalid DSN must fail the build");
        };
        let mut chain = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            chain.push_str(&format!(": {cause}"));
            source = cause.source();
        }
        assert!(chain.contains("not a valid DSN"), "{chain}");
        assert!(!chain.contains("secret-key"), "DSN leaked: {chain}");
    }

    /// ERROR events become Sentry events carrying the INFO/WARN breadcrumbs
    /// before them; DEBUG events are dropped.
    #[cfg(feature = "tracing-bridge")]
    #[test]
    fn bridge_routes_events_by_level() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let subscriber = tracing_subscriber::registry().with(super::TracingBridge::new(false));
        let events = tracing::subscriber::with_default(subscriber, || {
            sentry::test::with_captured_events(|| {
                tracing::debug!("too verbose");
                tracing::info!("crumb one");
                tracing::warn!("crumb two");
                tracing::error!("boom");
            })
        });

        assert_eq!(events.len(), 1, "only the ERROR event is an event");
        let event = &events[0];
        assert_eq!(event.message.as_deref(), Some("boom"));
        let crumbs: Vec<_> = event
            .breadcrumbs
            .iter()
            .filter_map(|crumb| crumb.message.as_deref())
            .collect();
        assert_eq!(crumbs, ["crumb one", "crumb two"]);
    }

    /// INFO spans become transactions; DEBUG spans don't.
    #[cfg(feature = "tracing-bridge")]
    #[test]
    fn bridge_turns_info_spans_into_transactions() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let transactions = |make_span: fn() -> tracing::Span| {
            let subscriber = tracing_subscriber::registry().with(super::TracingBridge::new(false));
            let envelopes = tracing::subscriber::with_default(subscriber, || {
                sentry::test::with_captured_envelopes_options(
                    || drop(make_span().entered()),
                    sentry::ClientOptions::new().traces_sample_rate(1.0),
                )
            });
            envelopes
                .iter()
                .flat_map(sentry::Envelope::items)
                .filter(|item| matches!(item, sentry::protocol::EnvelopeItem::Transaction(_)))
                .count()
        };

        assert_eq!(transactions(|| tracing::info_span!("request")), 1);
        assert_eq!(transactions(|| tracing::debug_span!("detail")), 0);
    }
}
