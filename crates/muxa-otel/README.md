# muxa-otel

[OpenTelemetry](https://opentelemetry.io) for [muxa](../muxa): exports traces, metrics and logs over OTLP, and can add an HTTP trace layer to the router.

Through the facade this is the `otel` feature (on by default) and the `otel-*` features.

## Usage

```rust
App::default()
    .with_plugin(OtelPlugin).await?      // early, so later plugins' startup logs are exported too
    // … other plugins, WebPlugin last
    .run().await
```

```toml
[otel]
service_name = "my-app"
endpoint = "http://localhost:4317"
```

With the facade, exporting all three signals over gRPC takes these features:

```toml
muxa = { git = "https://github.com/Sagebati/muxa", features = [
    "otel-otlp-tonic", "otel-tracing-bridge", "otel-metrics", "otel-logs",
] }
```

## When export is on

Export needs all three of:

- a transport compiled in: `otlp-tonic` (gRPC) or `otlp-http`;
- at least one signal compiled in: `tracing-bridge`, `metrics` or `logs`;
- `endpoint` set in the config.

If any is missing, the plugin still builds and logs `initialized (export disabled)`. That makes it safe to keep in the chain for local development.

## Signals

| Signal | Feature | Source |
|---|---|---|
| Traces | `tracing-bridge` | every `tracing` span, e.g. `#[tracing::instrument]` |
| Logs | `logs` | every `tracing` event (`info!`, `warn!`, …) |
| Metrics | `metrics` | instruments you create from the global meter |

Traces and logs need no code beyond the `tracing` macros you already use. For metrics, the plugin installs the global meter provider and you record against it with the `opentelemetry` crate (same version line, 0.32):

```rust
use std::sync::LazyLock;
use opentelemetry::metrics::Counter;
use opentelemetry::{KeyValue, global};

static REQUESTS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("my-app").u64_counter("requests").build()
});

REQUESTS.add(1, &[KeyValue::new("op", "list")]);
```

The level filter is `RUST_LOG`, the same one that controls stdout. The crates on the export path itself (tonic, hyper, reqwest, the OTel SDK and so on) are hidden from the exported signals, so the exporter does not export its own activity.

When the app's shutdown token is cancelled (ctrl-c, with `muxa-web`'s defaults), a background task flushes every provider, and `App::run` waits for it (up to ten seconds). SIGTERM is not handled, so a process stopped that way, as Docker and Kubernetes do, exits without flushing.

## Config

`[otel]`, or `MUXA_OTEL__*`:

| Key | Default | |
|---|---|---|
| `service_name` | `"muxa-app"` | the `service.name` resource attribute |
| `endpoint` | unset | OTLP collector, e.g. `http://localhost:4317` for gRPC. For HTTP see the note below |
| `timeout_secs` | `10` | per-export request timeout |
| `metric_interval_secs` | `60` | how often metrics are pushed |

Over HTTP (`otlp-http`) the endpoint is used exactly as written, for every signal: no `/v1/traces`, `/v1/metrics` or `/v1/logs` is appended. Give the full URL of one signal, e.g. `http://localhost:4318/v1/traces`, and compile in only that signal. To export more than one signal, use gRPC.

The plugin's output on the app state is `TelemetryHandles { service_name, endpoint }`. `endpoint` is `None` when export is off.

## Features

| Feature | Default | Effect |
|---|---|---|
| `http-layer` | yes | adds tower-http's `TraceLayer` to the router |
| `otlp-tonic` | | OTLP over gRPC |
| `otlp-http` | | OTLP over HTTP, one signal only (see Config). If both transports are on, gRPC is used |
| `tracing-bridge` | | export `tracing` spans as OTel traces |
| `metrics` | | meter provider with a periodic OTLP reader |
| `logs` | | export `tracing` events as OTel logs |
| `traces` | | the trace SDK only. Implied by the transports and by `tracing-bridge` |

The [`muxa`](../muxa) facade's `otel` feature includes `http-layer`.

The `TraceLayer` is a local debugging aid. Its request spans and events are DEBUG level under the `tower_http` target, so they show on stdout only with something like `RUST_LOG=info,tower_http=debug`. They are never exported: `tower` is one of the target prefixes kept away from the OTLP and Sentry layers, and it covers `tower_http`.

## Try it

The [`diesel_widgets`](../muxa/examples/diesel_widgets) example exports all three signals to a local `grafana/otel-lgtm` collector:

```bash
just diesel-example
```

Grafana is then at <http://localhost:3001>.

## License

MIT OR Apache-2.0
