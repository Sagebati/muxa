# muxa

Batteries-included web framework on [axum](https://docs.rs/axum) 0.8.

This is the facade crate. It re-exports [`muxa-core`](../muxa-core) and, behind Cargo features, every integration plugin in the workspace. Applications depend on this crate only.

## Quick start

```toml
[dependencies]
muxa  = { git = "https://github.com/Sagebati/muxa" }   # default features: web + otel
axum  = "0.8"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use axum::Router;
use axum::routing::get;
use muxa::prelude::*;

fn routes<S>(_state: &S) -> Router {
    Router::new().route("/", get(|| async { "hello from muxa" }))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::default()
        .with_plugin(OtelPlugin).await?
        .with_plugin(WebPlugin::new(routes)).await?
        .run().await
}
```

## How an app is put together

An app is a chain of plugins. Each `with_plugin(...).await?`:

1. reads the plugin's section of the config,
2. runs the plugin's `build`,
3. pushes what the plugin produced (a pool, a handle, or `()`) onto the app state.

The state is a typed list, so order matters and is checked at compile time: a plugin can only use what earlier plugins produced. `WebPlugin` (or `ApiPlugin`) goes last, because its routes callback receives the finished state and it owns the serve loop.

To use a resource in your routes, ask for it with `Selector`:

```rust
fn routes<S, Idx>(state: &S) -> Router
where
    S: Selector<SqlitePool, Idx>,
{
    let pool: SqlitePool = state.select().dupe();
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .with_state(pool)
}
```

The routes callback has to be a `fn` item like the ones above, or a function returning `impl FnOnce(&S) -> Router`. A bare `move |_state| router` closure does not compile (`FnOnce` is not general enough); see [`muxa-web`](../muxa-web) for the helper.

The plugin model itself (the `Plugin` trait, the state list, `BuildCtx`) is documented in [`muxa-core`](../muxa-core).

## Features

| Feature | Enables |
|---|---|
| `web` *(default)* | `WebPlugin`: axum serve loop with ctrl-c graceful shutdown ([`muxa-web`](../muxa-web)) |
| `ratelimit` | per-IP rate-limit layer: `RateLimitConfig`, `per_ip_layer` |
| `otel` *(default)* | `OtelPlugin` ([`muxa-otel`](../muxa-otel)) |
| `otel-otlp-tonic`, `otel-otlp-http` | OTLP export over gRPC or HTTP |
| `otel-tracing-bridge`, `otel-metrics`, `otel-logs` | which signals are exported |
| `otel-traces` | the trace SDK without the `tracing` bridge |
| `sentry` | `SentryPlugin` with the `tracing` bridge ([`muxa-sentry`](../muxa-sentry)) |
| `sqlx` | Postgres `SqlxPlugin` / `SqlxPool`, rustls ([`muxa-sqlx`](../muxa-sqlx)) |
| `sqlite` | `SqlitePlugin` / `SqlitePool` |
| `sqlx-macros`, `sqlx-migrate`, `sqlx-chrono`, `sqlx-time`, `sqlx-uuid`, `sqlx-json`, `sqlx-bigdecimal`, `sqlx-rust_decimal` | forwarded to sqlx; no-ops unless `sqlx` or `sqlite` is on |
| `diesel` | `DieselPlugin` / `DieselPool`, async Postgres ([`muxa-diesel`](../muxa-diesel)) |
| `diesel-migrations` | `MigrationsRunner`, `embed_migrations!` (implies `diesel`) |
| `diesel-sentry` | a tracing span per query (implies `diesel`) |
| `diesel-chrono`, `diesel-uuid`, `diesel-json`, `diesel-numeric`, `diesel-network` | forwarded to diesel |
| `pgmq` | `PgmqPlugin` ([`muxa-pgmq`](../muxa-pgmq)); the backend follows `sqlx` or `diesel` |
| `pgmq-tracing-spans` | spans around queue operations |
| `openapi` | `OpenApiPlugin`, `muxa::aide`, `muxa::schemars` ([`muxa-openapi`](../muxa-openapi)); with `web`, also `ApiPlugin` |
| `full` | `web`, `sqlx` (+ chrono, uuid, json), `pgmq`, `otel`, `sentry`, `openapi` |

`use muxa::prelude::*;` brings in `App`, the `Plugin` trait, the state and capability traits, and the plugin, config and resource types of every enabled feature. The integration crates are also reachable as modules: `muxa::web`, `muxa::sqlx`, `muxa::diesel`, `muxa::pgmq`, `muxa::otel`, `muxa::sentry`, `muxa::openapi`.

`muxa::sqlx` and `muxa::diesel` are the muxa plugin crates, not sqlx and diesel themselves. To write queries, add `sqlx` or `diesel` + `diesel-async` to your own `Cargo.toml`.

### Current limitations

- `web-no-signal`, `sentry-no-tracing` and `sqlx-tls-native` pull in their integration crate, but the facade's re-exports are gated on `web`, `sentry` and `sqlx`, so on their own they expose nothing. For those variants, depend on `muxa-web`, `muxa-sentry` or `muxa-sqlx` directly.
- `diesel-mysql` only compiles diesel-async's MySQL driver. There is no MySQL plugin yet.
- The facade does not forward `muxa-otel`'s `http-layer` feature, so `OtelPlugin` used through the facade does not mount the tower-http `TraceLayer`. Adding `muxa-otel` as a direct dependency (its default features include `http-layer`) turns it on.

## Configuration

Two layers, last one wins:

1. one TOML file: `$MUXA_CONFIG` if set, otherwise `./muxa.toml`. A missing file is fine.
2. environment variables prefixed `MUXA_`, with `__` between keys: `MUXA_WEB__PORT=8080` sets `web.port`.

```toml
# muxa.toml
env = "production"        # optional: development | production

[web]
host = "0.0.0.0"
port = 3000

[otel]
service_name = "my-app"
```

Each plugin reads one section, and a missing section means that plugin's defaults. The sections are listed in each crate's README.

| Constructor | Config file | Env prefix |
|---|---|---|
| `App::default()` | `$MUXA_CONFIG` or `./muxa.toml` | `MUXA_` |
| `App::with_config_file(path)` | `path` | `MUXA_` |
| `App::with_env_prefix("MYAPP_")` | `$MYAPP_CONFIG` or `./muxa.toml` | `MYAPP_` |
| `App::with_config_file_and_env_prefix(path, "MYAPP_")` | `path` | `MYAPP_` |
| `App::with_figment(figment)` | whatever you built | whatever you built |

Log verbosity comes from `RUST_LOG` (default `info`).

## Examples

Run from the workspace root.

| Example | Command | Needs |
|---|---|---|
| `web_only`: `OtelPlugin` + `WebPlugin` | `just web-only` | nothing |
| `hello`: adds an in-memory SQLite pool | `just hello` | nothing |
| `diesel_widgets`: JSON API on Postgres with embedded migrations, exporting traces, metrics and logs over OTLP | `just diesel-example` | Docker |

Without `just`:

```bash
cargo run -p muxa --example web_only
```

```bash
cargo run -p muxa --example hello --features sqlite
```

`just diesel-example` starts Postgres and a `grafana/otel-lgtm` collector with docker compose, then runs the example. Grafana is at <http://localhost:3001>. `just diesel-example-down` removes the containers.

## Crates

| Crate | What it is |
|---|---|
| [`muxa-core`](../muxa-core) | `Plugin` trait, typed state, `App`, `BuildCtx`, config, errors |
| [`muxa-telemetry`](../muxa-telemetry) | the shared `tracing` subscriber that plugins attach layers to |
| [`muxa-web`](../muxa-web) | `WebPlugin`, `ApiPlugin`, rate limiting |
| [`muxa-sqlx`](../muxa-sqlx) | SQLx pools: Postgres and SQLite |
| [`muxa-diesel`](../muxa-diesel) | diesel-async Postgres pool, embedded migrations |
| [`muxa-pgmq`](../muxa-pgmq) | installs pgmq and creates queues at startup |
| [`muxa-otel`](../muxa-otel) | OpenTelemetry traces, metrics, logs |
| [`muxa-sentry`](../muxa-sentry) | Sentry errors, performance, logs |
| [`muxa-openapi`](../muxa-openapi) | serves an aide OpenAPI document and a docs page |
| [`diesel-sentry`](../diesel-sentry) | diesel query spans; usable without muxa |

## License

MIT OR Apache-2.0
