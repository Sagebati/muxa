# diesel-sentry

A [Diesel](https://diesel.rs) `Instrumentation` that turns connection, query and transaction events into `tracing` spans, tagged for both Sentry and OpenTelemetry.

It depends only on `diesel` and `tracing`. It lives in the [muxa](../muxa) workspace but does not depend on muxa, and works with any diesel or diesel-async setup.

```toml
[dependencies]
diesel-sentry = { git = "https://github.com/Sagebati/muxa" }
```

## Usage

Install it once, before you build your connection pool:

```rust
diesel_sentry::install().expect("set diesel default instrumentation");
// … build the pool. Connections established from here on are instrumented.
```

`install()` sets diesel's process-global default instrumentation. Connections opened earlier are not affected.

Or instrument a single connection:

```rust
use diesel_sentry::SentryInstrumentation;

conn.set_instrumentation(SentryInstrumentation::default());
```

In a muxa app you do neither: the `diesel-sentry` feature of the facade (`sentry` on [`muxa-diesel`](../muxa-diesel)) makes `DieselPlugin` call `install()` for you.

## What you get

The crate only creates spans. What happens to them depends on the `tracing` layers you have installed:

- with a `sentry-tracing` layer, queries appear as database spans under the current request transaction;
- with a `tracing-opentelemetry` layer, they become OTel client spans.

| Span | Fields |
|---|---|
| `db.connect` | `sentry.op = "db.connect"`, `sentry.name = "establish_connection"`, `db.system`, `otel.kind = "client"`, `error` |
| `db.query` | `sentry.op = "db.sql.query"`, `sentry.name` and `db.statement` (the SQL), `db.operation` (first keyword, e.g. `SELECT`), `db.transaction.depth`, `db.system`, `otel.kind = "client"`, `error` |

Details:

- Bind values are stripped. `db.statement` carries only the parameterised SQL, so parameter values never reach your telemetry backend.
- `error` is filled in when the query or connection fails.
- `db.transaction.depth` is `0` outside a transaction.
- `db.system` is always `"postgresql"`, whatever backend the connection uses.
- Spans are created at INFO level. Prepared-statement caching is logged at DEBUG on the `diesel_sentry` target.

## License

MIT OR Apache-2.0
