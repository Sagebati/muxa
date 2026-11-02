//! muxa-otel — OpenTelemetry plugin.
//!
//! The base `tracing-subscriber` (fmt + EnvFilter) is owned by
//! [`muxa-telemetry`](muxa_core::TelemetryRegistry); this plugin pushes
//! OpenTelemetry layers/middleware onto it and (when configured) wires the OTLP
//! export pipeline.
//!
//! Features:
//!
//! | Feature           | Default | Effect                                                   |
//! |-------------------|---------|----------------------------------------------------------|
//! | `http-layer`      | ✓       | Mount `tower-http` `TraceLayer` on the router            |
//! | `traces`          |         | Build an OpenTelemetry `SdkTracerProvider`               |
//! | `metrics`         |         | Build an OpenTelemetry `SdkMeterProvider` (periodic OTLP)|
//! | `logs`            |         | Build an OpenTelemetry `SdkLoggerProvider` + log bridge  |
//! | `otlp-tonic`      |         | OTLP exporter over gRPC (implies `traces`)               |
//! | `otlp-http`       |         | OTLP exporter over HTTP (implies `traces`)               |
//! | `tracing-bridge`  |         | Route `tracing` spans to the tracer provider             |
//!
//! Export turns on only when `[otel].endpoint` (or, failing it,
//! `OTEL_EXPORTER_OTLP_ENDPOINT`) is set **and** an OTLP transport feature is
//! compiled in; otherwise the plugin just installs the HTTP
//! `TraceLayer` and records the service name (export is a no-op).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bon::Builder;
use muxa_core::{BuildCtx, EnvFallback, Plugin, Result, State};
use serde::Deserialize;

fn default_service_name() -> String {
    "muxa-app".to_owned()
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_metric_interval_secs() -> u64 {
    60
}

/// Configuration for [`OtelPlugin`]. Read from `[otel]` (and `<APP>_OTEL__*`);
/// the spec's `OTEL_SERVICE_NAME` and `OTEL_EXPORTER_OTLP_ENDPOINT` fill
/// `service_name` and `endpoint` when the app's configuration leaves them unset.
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct OtelConfig {
    /// Logical service name (the `service.name` resource attribute).
    #[builder(default = default_service_name())]
    pub service_name: String,
    /// OTLP collector endpoint, e.g. `http://localhost:4317` (gRPC) or
    /// `http://localhost:4318` (HTTP). Over HTTP it's the base URL, as with
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`: each signal goes to its `/v1/<signal>`
    /// path under it. When unset, export is disabled.
    pub endpoint: Option<String>,
    /// Per-export request timeout, in seconds.
    #[builder(default = default_timeout_secs())]
    pub timeout_secs: u64,
    /// How often metrics are pushed to the collector, in seconds.
    #[builder(default = default_metric_interval_secs())]
    pub metric_interval_secs: u64,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            service_name: default_service_name(),
            endpoint: None,
            timeout_secs: default_timeout_secs(),
            metric_interval_secs: default_metric_interval_secs(),
        }
    }
}

/// Handle stored on the application state.
#[derive(Debug, Clone)]
pub struct TelemetryHandles {
    /// The configured service name (also used as the tracer name).
    pub service_name: String,
    /// The OTLP endpoint exports are sent to, if export is enabled.
    pub endpoint: Option<String>,
}

/// OTLP export: the exporters, their bridge layers and the flush on shutdown.
///
/// Compiled in only when a transport is and at least one signal is selected;
/// [`OtelPlugin::build`] calls [`export::install`] under the same predicate.
#[cfg(all(
    any(feature = "otlp-tonic", feature = "otlp-http"),
    any(feature = "tracing-bridge", feature = "metrics", feature = "logs")
))]
mod export {
    use std::time::Duration;

    use muxa_core::{BuildCtx, Error, Result};

    use crate::OtelConfig;

    /// Build the OTLP exporter for the selected transport, endpoint and timeout.
    /// `otlp-tonic` wins if both transports are somehow enabled. Over HTTP the
    /// endpoint is a base URL and `$path` the signal's path under it (an
    /// explicit endpoint is otherwise sent to as-is by the SDK).
    macro_rules! configured_exporter {
        ($builder:expr, $endpoint:expr, $path:literal, $timeout:expr) => {{
            use opentelemetry_otlp::WithExportConfig as _;
            #[cfg(feature = "otlp-tonic")]
            let (builder, endpoint) = ($builder.with_tonic(), $endpoint.to_owned());
            #[cfg(all(feature = "otlp-http", not(feature = "otlp-tonic")))]
            let (builder, endpoint) = (
                $builder.with_http(),
                format!("{}{}", $endpoint.trim_end_matches('/'), $path),
            );
            builder
                .with_endpoint(endpoint)
                .with_timeout($timeout)
                .build()
                .map_err(Error::other)?
        }};
    }

    /// The SDK providers export was set up with, flushed together on shutdown
    /// so the batch tail isn't lost. One field per enabled signal.
    pub struct Providers {
        #[cfg(feature = "tracing-bridge")]
        pub tracer: opentelemetry_sdk::trace::SdkTracerProvider,
        #[cfg(feature = "metrics")]
        pub meter: opentelemetry_sdk::metrics::SdkMeterProvider,
        #[cfg(feature = "logs")]
        pub logger: opentelemetry_sdk::logs::SdkLoggerProvider,
    }

    impl Providers {
        /// Flush and shut down every provider, all at once and off the async
        /// workers.
        ///
        /// The SDK's `shutdown` blocks until its exporter has drained (or timed
        /// out). Called on a runtime worker it would stall that worker, and on
        /// a current-thread runtime the very I/O the export waits on. Errors
        /// are dropped: there is nowhere left to report them.
        pub async fn shutdown(self) {
            #[cfg(feature = "tracing-bridge")]
            let tracer = off_runtime(move || self.tracer.shutdown());
            #[cfg(feature = "metrics")]
            let meter = off_runtime(move || self.meter.shutdown());
            #[cfg(feature = "logs")]
            let logger = off_runtime(move || self.logger.shutdown());

            #[cfg(feature = "tracing-bridge")]
            let _ = tracer.await;
            #[cfg(feature = "metrics")]
            let _ = meter.await;
            #[cfg(feature = "logs")]
            let _ = logger.await;
        }
    }

    /// Run a blocking provider shutdown on the blocking pool.
    fn off_runtime<E>(
        shutdown: impl FnOnce() -> Result<(), E> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let _ = shutdown();
        })
    }

    /// Set up export to `endpoint` for every enabled signal: build each
    /// provider, register its bridge layer (traces/logs) or global (metrics),
    /// and schedule one task that flushes them all on shutdown.
    pub fn install(cfg: &OtelConfig, endpoint: &str, ctx: &mut BuildCtx) -> Result<()> {
        let timeout = Duration::from_secs(cfg.timeout_secs);
        let resource = opentelemetry_sdk::Resource::builder()
            .with_service_name(cfg.service_name.clone())
            .build();

        // The crates on the OTLP export path (tonic→h2→hyper, or reqwest, and
        // the SDK itself). A layer that captured their spans/events would
        // re-export what its own exporting produced — an amplifying feedback
        // loop — so hide them from the plugin layers. `tower` doesn't cover
        // `tower_http`, so the HTTP request spans still get exported.
        ctx.telemetry.exclude_targets(&[
            "h2",
            "hyper",
            "hyper_util",
            "tower",
            "tonic",
            "reqwest",
            "opentelemetry",
            "opentelemetry_sdk",
            "opentelemetry_otlp",
        ]);

        #[cfg(feature = "tracing-bridge")]
        let tracer = {
            use opentelemetry::trace::TracerProvider as _;
            let exporter = configured_exporter!(
                opentelemetry_otlp::SpanExporter::builder(),
                endpoint,
                "/v1/traces",
                timeout
            );
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(resource.clone())
                .build();
            let tracer = provider.tracer(cfg.service_name.clone());
            // Route `tracing` spans → OTel via the shared subscriber.
            ctx.telemetry
                .add_layer(tracing_opentelemetry::layer().with_tracer(tracer));
            opentelemetry::global::set_tracer_provider(provider.clone());
            provider
        };

        #[cfg(feature = "metrics")]
        let meter = {
            let exporter = configured_exporter!(
                opentelemetry_otlp::MetricExporter::builder(),
                endpoint,
                "/v1/metrics",
                timeout
            );
            let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
                .with_interval(Duration::from_secs(cfg.metric_interval_secs))
                .build();
            let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_reader(reader)
                .with_resource(resource.clone())
                .build();
            opentelemetry::global::set_meter_provider(provider.clone());
            provider
        };

        #[cfg(feature = "logs")]
        let logger = {
            let exporter = configured_exporter!(
                opentelemetry_otlp::LogExporter::builder(),
                endpoint,
                "/v1/logs",
                timeout
            );
            let provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(resource)
                .build();
            // Route `tracing` events → OTel logs via the shared subscriber.
            ctx.telemetry.add_layer(
                opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider),
            );
            provider
        };

        let providers = Providers {
            #[cfg(feature = "tracing-bridge")]
            tracer,
            #[cfg(feature = "metrics")]
            meter,
            #[cfg(feature = "logs")]
            logger,
        };
        ctx.tasks
            .spawn("otel-shutdown", move |shutdown| async move {
                shutdown.cancelled().await;
                providers.shutdown().await;
            });
        Ok(())
    }
}

/// The OpenTelemetry plugin.
#[derive(Default)]
pub struct OtelPlugin;

impl<S: State> Plugin<S> for OtelPlugin {
    type Output = TelemetryHandles;
    type Config = OtelConfig;
    const CONFIG_PREFIX: &'static str = "otel";
    const ENV_FALLBACKS: &'static [EnvFallback] = &[
        EnvFallback {
            var: "OTEL_SERVICE_NAME",
            key: "service_name",
        },
        EnvFallback {
            var: "OTEL_EXPORTER_OTLP_ENDPOINT",
            key: "endpoint",
        },
    ];

    async fn build(
        self,
        cfg: OtelConfig,
        _state: &S,
        ctx: &mut BuildCtx,
    ) -> Result<TelemetryHandles> {
        // (1) HTTP request spans on the router.
        #[cfg(feature = "http-layer")]
        ctx.router
            .layer(|router| router.layer(tower_http::trace::TraceLayer::new_for_http()));

        // (2)+(3) OTLP export. Only when a transport is compiled in, at least one
        // signal is selected, and an endpoint is configured.
        #[cfg(all(
            any(feature = "otlp-tonic", feature = "otlp-http"),
            any(feature = "tracing-bridge", feature = "metrics", feature = "logs")
        ))]
        if let Some(endpoint) = cfg.endpoint.as_deref() {
            export::install(&cfg, endpoint, ctx)?;
            tracing::info!(service = %cfg.service_name, %endpoint, "muxa-otel: OTLP export enabled");
            return Ok(TelemetryHandles {
                service_name: cfg.service_name,
                endpoint: cfg.endpoint,
            });
        }

        tracing::info!(service = %cfg.service_name, "muxa-otel: initialized (export disabled)");
        Ok(TelemetryHandles {
            service_name: cfg.service_name,
            endpoint: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otel_config_defaults() {
        let config = OtelConfig::default();
        assert_eq!(config.service_name, "muxa-app");
        assert_eq!(config.endpoint, None);
        assert_eq!(config.timeout_secs, 10);
        assert_eq!(config.metric_interval_secs, 60);
    }

    /// On a current-thread runtime the blocking provider shutdowns must not
    /// run on the one runtime thread: they go to the blocking pool, and the
    /// flush completes.
    #[cfg(all(
        any(feature = "otlp-tonic", feature = "otlp-http"),
        any(feature = "tracing-bridge", feature = "metrics", feature = "logs")
    ))]
    #[tokio::test(flavor = "current_thread")]
    async fn providers_shut_down_off_the_runtime() {
        let providers = export::Providers {
            #[cfg(feature = "tracing-bridge")]
            tracer: opentelemetry_sdk::trace::SdkTracerProvider::builder().build(),
            #[cfg(feature = "metrics")]
            meter: opentelemetry_sdk::metrics::SdkMeterProvider::builder().build(),
            #[cfg(feature = "logs")]
            logger: opentelemetry_sdk::logs::SdkLoggerProvider::builder().build(),
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), providers.shutdown())
            .await
            .unwrap();
    }
}
