# muxa-sentry

[Sentry](https://sentry.io) for [muxa](../muxa): error and panic reporting, performance transactions per request, and structured logs.

Through the facade this is the `sentry` feature.

## Usage

```rust
muxa::app!()
    .with_plugin(SentryPlugin).await?    // early, so later plugins' startup logs and panics reach Sentry
    .with_plugin(OtelPlugin).await?
    // … other plugins, WebPlugin last
    .run().await
```

```toml
[sentry]
dsn = "https://<key>@<org>.ingest.sentry.io/<project>"
release = "my-app@1.2.3"
```

The DSN is a secret. Prefer the environment: `<APP>_SENTRY__DSN=…`, or the SDK's own `SENTRY_DSN`.

The plugin does three things:

1. initialises the Sentry SDK (panics, errors, transactions). Its output, `SentryHandle`, keeps the client alive for the life of the app, and pending events are flushed when it is dropped.
2. adds the `sentry-tower` layers to the router: a Sentry scope per request, HTTP context on events, and optionally a transaction per request, named after the matched route template (`GET /jobs/{id}`, not `GET /jobs/42`) so Sentry groups requests per endpoint.
3. with the `tracing-bridge` feature, attaches `sentry-tracing` to the shared subscriber, so `tracing` events and spans reach Sentry.

Anything that already emits `tracing` spans shows up in Sentry through that bridge with no extra wiring. The query spans from [`diesel-sentry`](../diesel-sentry) are one example.

## Config

`[sentry]`, or `<APP>_SENTRY__*`. When the app sets neither, `SENTRY_DSN`, `SENTRY_ENVIRONMENT` and `SENTRY_RELEASE` fill `dsn`, `environment` and `release`:

| Key | Default | |
|---|---|---|
| `dsn` | unset | without one the SDK is a no-op client and nothing is sent |
| `environment` | the run mode | `"development"` or `"production"`; set it for anything else, e.g. `"staging"` |
| `release` | unset | e.g. `"my-app@1.2.3"` |
| `traces_sample_rate` | `1.0` in development, `0.1` in production | fraction of transactions kept; `0.0` turns performance monitoring off |
| `transaction_sample_rates` | `muxa.task` = `0.0` | per-transaction overrides of `traces_sample_rate`, keyed by exact transaction name (see below) |
| `attach_stacktrace` | `true` | attach a stack trace to every event |
| `send_default_pii` | `false` | send user IP, cookies and similar |
| `http_transactions` | `true` | wrap each request in a transaction. No effect when the sample rate is `0.0` |
| `logs` | `true` | send `tracing` events at INFO and above as Sentry structured logs. Needs `tracing-bridge` |

The run mode is muxa's `RunMode`: the top-level `env` config key, or else the build profile (see [`muxa-core`](../muxa-core)).

Things to know:

- An empty DSN is treated as absent: the SDK runs as a no-op. A DSN that does not parse fails the build instead of silently turning Sentry off.
- With a DSN set, performance monitoring and logs are on by default. `logs` ships every INFO+ event and counts against your Sentry log quota. Set `logs = false` to turn it off.

### Per-transaction sample rates

A health probe or a polled endpoint can drown the transactions you care about. Give it its own rate, or `0.0` to drop it:

```toml
[sentry]
traces_sample_rate = 1.0

[sentry.transaction_sample_rates]
"GET /healthz" = 0.0
"GET /jobs/changes" = 0.05
```

A request's transaction is named `METHOD /route-template`, and a `tracing` root span's is the span name. `muxa.task`, the span a background task runs in, defaults to `0.0`: it lasts as long as the app, so it would only reach Sentry at shutdown, cut at the SDK's 1000-span limit. Work inside a task that should be traced needs a trace of its own: run it on a new hub with no active span (`Hub::new_from_top` plus `scope.set_span(None)`, bound with `bind_hub`), because the `tracing` bridge parents a span on the hub's active span, not on `parent: None`. A transaction that continues an incoming trace (a `sentry-trace` header, or the `sentry.trace` span field) keeps its parent's sampling decision, so distributed traces stay whole.

## Features

| Feature | Effect |
|---|---|
| `tracing-bridge` | route `tracing` events and spans to Sentry, and enable structured logs |

Nothing is on by default in this crate. The facade's `sentry` feature includes `tracing-bridge`.

## License

MIT OR Apache-2.0
