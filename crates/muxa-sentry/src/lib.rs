//! muxa-sentry — Sentry SDK integration.
//!
//! Three responsibilities:
//!
//! 1. Initialize the native Sentry SDK (panic capture, error reporting,
//!    transactions). The `ClientInitGuard` is held in plugin output so
//!    events flush on shutdown.
//! 2. Install `sentry-tower` middleware (per-request scope + HTTP context
//!    capture + optional transactions for performance monitoring) on the
//!    router.
//! 3. **With the `tracing-bridge` feature**: push `sentry-tracing::layer()`
//!    onto the shared `muxa-telemetry` subscriber so `tracing` events and
//!    spans flow into Sentry as breadcrumbs / events / transactions.
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
//!     App::default()
//!         .with_plugin(SentryPlugin).await?
//!         .with_plugin(OtelPlugin).await?
//!         // ... add WebPlugin last
//!         .run().await
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::borrow::Cow;

use bon::Builder;
use muxa_core::{BuildCtx, Plugin, Result, RunMode, State};
use secrecy::{ExposeSecret as _, SecretString};
use sentry::ClientInitGuard;
use sentry_tower::{NewSentryLayer, SentryHttpLayer};
use serde::Deserialize;

/// Configuration for [`SentryPlugin`]. Read from `[sentry]`.
#[derive(Debug, Clone, Default, Deserialize, Builder)]
#[serde(default)]
pub struct SentryConfig {
    /// Sentry DSN. If `None`/empty, the SDK initializes a no-op client and
    /// no events are sent — useful for development. A [`SecretString`] so it's
    /// redacted in `Debug`/logs; exposed only to parse it at init.
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

    async fn build(
        self,
        cfg: SentryConfig,
        _state: &S,
        ctx: &mut BuildCtx,
    ) -> Result<SentryHandle> {
        // Treat empty string as None. The DSN is a secret — expose it only to
        // parse it here, never to logs.
        let raw_dsn = cfg
            .dsn
            .as_ref()
            .map(|secret| secret.expose_secret())
            .filter(|raw| !raw.is_empty());
        let dsn = raw_dsn.and_then(|raw| raw.parse::<sentry::types::Dsn>().ok());

        // The app-wide run mode is the source of truth for env-aware defaults.
        let mode = ctx.mode;
        // Default the environment label to the run mode; an explicit config wins.
        let environment = cfg
            .environment
            .clone()
            .unwrap_or_else(|| mode.as_str().to_owned());
        // Unset rate ⇒ pick from the run mode (prod: light sampling, dev: full).
        // An explicit value (including 0.0, to disable) wins.
        let traces_sample_rate = cfg
            .traces_sample_rate
            .unwrap_or_else(|| default_traces_sample_rate(mode));

        let options = sentry::ClientOptions {
            dsn,
            environment: Some(Cow::Owned(environment.clone())),
            release: cfg.release.clone().map(Cow::Owned),
            traces_sample_rate,
            attach_stacktrace: cfg.attach_stacktrace,
            send_default_pii: cfg.send_default_pii,
            ..Default::default()
        };

        let guard = sentry::init(options);

        // Tower middleware. Axum applies the last-added layer outermost,
        // so order matters: `SentryHttpLayer` first (inner) and
        // `NewSentryLayer` second (outermost — scopes the request before
        // any other middleware runs).
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

        // tracing→Sentry bridge: fill the typed sentry slot on the
        // shared subscriber that muxa-telemetry installed. No subscriber
        // init here — a single shared subscriber is the whole point of
        // muxa-telemetry, and the slot is reserved at install time by
        // the `muxa-telemetry/sentry` feature (auto-enabled via feature
        // linking from this crate's `tracing-bridge` feature).
        #[cfg(feature = "tracing-bridge")]
        ctx.telemetry.set_sentry_layer(sentry_tracing::layer());

        if raw_dsn.is_some() {
            tracing::info!(
                env = environment,
                release = cfg.release.as_deref().unwrap_or("-"),
                traces_sample_rate,
                "muxa-sentry: initialized"
            );
        } else {
            tracing::info!("muxa-sentry: no DSN set, running as no-op client");
        }

        Ok(SentryHandle { _guard: guard })
    }
}
