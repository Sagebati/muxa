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
//! Export turns on only when `[otel].endpoint` is set **and** an OTLP transport
//! feature is compiled in; otherwise the plugin just installs the HTTP
//! `TraceLayer` and records the service name (export is a no-op).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bon::Builder;
use muxa_core::{BuildCtx, Plugin, Result, State};
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

/// Configuration for [`OtelPlugin`]. Read from `[otel]` (and `MUXA_OTEL__*`).
#[derive(Debug, Clone, Deserialize, Builder)]
#[serde(default)]
pub struct OtelConfig {
    /// Logical service name (the `service.name` resource attribute).
    #[builder(default = default_service_name())]
    pub service_name: String,
    /// OTLP collector endpoint, e.g. `http://localhost:4317` (gRPC) or
    /// `http://localhost:4318` (HTTP). When unset, export is disabled.
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

/// Build the OTLP exporter for the selected transport, endpoint and timeout.
/// `otlp-tonic` wins if both transports are somehow enabled.
#[cfg(any(feature = "otlp-tonic", feature = "otlp-http"))]
macro_rules! configured_exporter {
    ($builder:expr, $endpoint:expr, $timeout:expr) => {{
        use opentelemetry_otlp::WithExportConfig as _;
        #[cfg(feature = "otlp-tonic")]
        let builder = $builder.with_tonic();
        #[cfg(all(feature = "otlp-http", not(feature = "otlp-tonic")))]
        let builder = $builder.with_http();
        builder
            .with_endpoint($endpoint)
            .with_timeout($timeout)
            .build()
    }};
}

/// The OpenTelemetry plugin.
#[derive(Default)]
pub struct OtelPlugin;

impl<S: State> Plugin<S> for OtelPlugin {
    type Output = TelemetryHandles;
    type Config = OtelConfig;
    const CONFIG_PREFIX: &'static str = "otel";

    async fn build(self, cfg: OtelConfig, _state: &S, ctx: &mut BuildCtx) -> Result<TelemetryHandles> {
        // (1) HTTP request spans on the router.
        #[cfg(feature = "http-layer")]
        ctx.router
            .layer(|router| router.layer(tower_http::trace::TraceLayer::new_for_http()));

        // (2)+(3) OTLP export. Only when a transport is compiled in, at least one
        // signal is selected, and an endpoint is configured. Each enabled signal
        // builds its provider, registers its bridge layer (traces/logs) or global
        // (metrics), and a single shutdown task flushes them all on stop.
        #[cfg(all(
            any(feature = "otlp-tonic", feature = "otlp-http"),
            any(feature = "tracing-bridge", feature = "metrics", feature = "logs")
        ))]
        if let Some(endpoint) = cfg.endpoint.clone() {
            let timeout = std::time::Duration::from_secs(cfg.timeout_secs);
            let resource = opentelemetry_sdk::Resource::builder()
                .with_service_name(cfg.service_name.clone())
                .build();
            // Each entry flushes one provider on shutdown.
            let mut flushes: Vec<Box<dyn FnOnce() + Send>> = Vec::new();

            #[cfg(feature = "tracing-bridge")]
            {
                use opentelemetry::trace::TracerProvider as _;
                let exporter =
                    configured_exporter!(opentelemetry_otlp::SpanExporter::builder(), endpoint.as_str(), timeout)
                        .map_err(muxa_core::Error::other)?;
                let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource.clone())
                    .build();
                let tracer = provider.tracer(cfg.service_name.clone());
                // Route `tracing` spans → OTel via the shared subscriber.
                ctx.telemetry
                    .add_layer(tracing_opentelemetry::layer().with_tracer(tracer));
                opentelemetry::global::set_tracer_provider(provider.clone());
                flushes.push(Box::new(move || {
                    let _ = provider.shutdown();
                }));
            }

            #[cfg(feature = "metrics")]
            {
                let exporter =
                    configured_exporter!(opentelemetry_otlp::MetricExporter::builder(), endpoint.as_str(), timeout)
                        .map_err(muxa_core::Error::other)?;
                let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
                    .with_interval(std::time::Duration::from_secs(cfg.metric_interval_secs))
                    .build();
                let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                    .with_reader(reader)
                    .with_resource(resource.clone())
                    .build();
                opentelemetry::global::set_meter_provider(provider.clone());
                flushes.push(Box::new(move || {
                    let _ = provider.shutdown();
                }));
            }

            #[cfg(feature = "logs")]
            {
                let exporter =
                    configured_exporter!(opentelemetry_otlp::LogExporter::builder(), endpoint.as_str(), timeout)
                        .map_err(muxa_core::Error::other)?;
                let provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource.clone())
                    .build();
                // Route `tracing` events → OTel logs via the shared subscriber.
                ctx.telemetry.add_layer(
                    opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider),
                );
                flushes.push(Box::new(move || {
                    let _ = provider.shutdown();
                }));
            }

            // Flush all providers on graceful shutdown so the batch tail isn't lost.
            ctx.tasks.spawn("otel-shutdown", move |shutdown| async move {
                shutdown.cancelled().await;
                for flush in flushes {
                    flush();
                }
            });

            tracing::info!(service = %cfg.service_name, %endpoint, "muxa-otel: OTLP export enabled");
            return Ok(TelemetryHandles {
                service_name: cfg.service_name,
                endpoint: Some(endpoint),
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
}
