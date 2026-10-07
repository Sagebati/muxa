# muxa-sentry

[Sentry](https://sentry.io) for [muxa](../muxa): error and panic reporting, performance transactions per request, and structured logs.

Through the facade this is the `sentry` feature.

## Usage

```rust
App::default()
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

The DSN is a secret. Prefer the environment: `MUXA_SENTRY__DSN=…`.

The plugin does three things:

1. initialises the Sentry SDK (panics, errors, transactions). Its output, `SentryHandle`, keeps the client alive for the life of the app, and pending events are flushed when it is dropped.
2. adds the `sentry-tower` layers to the router: a Sentry scope per request, HTTP context on events, and optionally a transaction per request.
3. with the `tracing-bridge` feature, attaches `sentry-tracing` to the shared subscriber, so `tracing` events and spans reach Sentry.

Anything that already emits `tracing` spans shows up in Sentry through that bridge with no extra wiring. The query spans from [`diesel-sentry`](../diesel-sentry) are one example.

## Config

`[sentry]`, or `MUXA_SENTRY__*`:

| Key | Default | |
|---|---|---|
| `dsn` | unset | without one the SDK is a no-op client and nothing is sent |
| `environment` | the run mode | `"development"` or `"production"`; set it for anything else, e.g. `"staging"` |
| `release` | unset | e.g. `"my-app@1.2.3"` |
| `traces_sample_rate` | `1.0` in development, `0.1` in production | fraction of transactions kept; `0.0` turns performance monitoring off |
| `attach_stacktrace` | `true` | attach a stack trace to every event |
| `send_default_pii` | `false` | send user IP, cookies and similar |
| `http_transactions` | `true` | wrap each request in a transaction. No effect when the sample rate is `0.0` |
| `logs` | `true` | send `tracing` events at INFO and above as Sentry structured logs. Needs `tracing-bridge` |

The run mode is muxa's `RunMode`: the top-level `env` config key, or else the build profile (see [`muxa-core`](../muxa-core)).

Things to know:

- A DSN that is empty or does not parse is treated as absent. The SDK runs as a no-op and no error is raised.
- With a DSN set, performance monitoring and logs are on by default. `logs` ships every INFO+ event and counts against your Sentry log quota. Set `logs = false` to turn it off.

## Features

| Feature | Effect |
|---|---|
| `tracing-bridge` | route `tracing` events and spans to Sentry, and enable structured logs |

Nothing is on by default in this crate. The facade's `sentry` feature includes `tracing-bridge`.

## License

MIT OR Apache-2.0
