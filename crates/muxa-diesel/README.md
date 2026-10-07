# muxa-diesel

Async [Diesel](https://diesel.rs) for [muxa](../muxa): a Postgres connection pool built on `diesel-async` and deadpool, with optional embedded migrations and per-query tracing.

Through the facade this is the `diesel` feature, plus `diesel-migrations` and `diesel-sentry`.

## Usage

```rust
use axum::Router;
use axum::routing::get;
use muxa::prelude::*;

fn routes<S, Idx>(state: &S) -> Router
where
    S: Selector<DieselPool, Idx>,
{
    let pool: DieselPool = state.select().dupe();
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .with_state(pool)        // handlers take `State<DieselPool>`
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::default()
        .with_plugin(OtelPlugin).await?
        .with_plugin(DieselPlugin::new()).await?
        .with_plugin(WebPlugin::new(routes)).await?
        .run().await
}
```

`DieselPlugin` pushes a `DieselPool` onto the app state. It derefs to deadpool's `Pool<AsyncPgConnection>`, and cloning it (`dupe()`) is a reference-count bump.

In a handler, take a connection from the pool and run diesel-async queries on it:

```rust
let mut conn = pool.get().await?;
let rows = widgets::table
    .select(Widget::as_select())
    .load(&mut conn)
    .await?;
```

The pool is built lazily: no connection is opened until the first `pool.get()`, so a wrong URL shows up on first use and not at startup. Migrations mode is the exception, since it connects during startup.

This crate does not re-export diesel. Add these to your own `Cargo.toml` for the schema, derives and query execution:

```toml
diesel       = { version = "2", default-features = false, features = ["postgres"] }
diesel-async = { version = "0.9", default-features = false, features = ["postgres", "deadpool"] }
```

A complete, runnable version is the [`diesel_widgets`](../muxa/examples/diesel_widgets) example.

## Config

`[diesel]`, or `MUXA_DIESEL__*`:

| Key | Default | |
|---|---|---|
| `url` | none, required | Postgres connection URL. Startup fails if it is empty |
| `max_connections` | `10` | pool size |

`url` is held as a `SecretString`, so a `Debug` print of the config does not reveal it.

## Migrations

With the `migrations` feature, the plugin applies pending migrations at startup, before the pool is published. Later plugins and all handlers see the migrated schema.

```rust
const MIGRATIONS: EmbeddedMigrations = embed_migrations!();   // ./migrations

App::default()
    .with_plugin(DieselPlugin::new().with_migrations(MigrationsRunner::new(MIGRATIONS)))
    .await?
```

- All pending migrations run in one transaction: either all apply or none do. A migration that cannot run in a transaction, such as `CREATE INDEX CONCURRENTLY`, is incompatible with this mode.
- They run on a dedicated connection, not one from the pool.
- `embed_migrations!` expands to `diesel_migrations::` paths, so your crate needs `diesel_migrations = "2"` as a direct dependency.
- `MigrationsRunner::new(MIGRATIONS).run(url).await` runs them without the plugin and returns how many were applied.

## Query tracing

With the `sentry` feature, `DieselPlugin` installs [`diesel-sentry`](../diesel-sentry) as diesel's global instrumentation before it opens any connection. Every query, connection and transaction then produces a `tracing` span, which reaches Sentry through [`muxa-sentry`](../muxa-sentry) or an OTLP collector through [`muxa-otel`](../muxa-otel). Nothing else has to be configured.

## Features

| Feature | Default | Effect |
|---|---|---|
| `postgres` | yes | `DieselPlugin`, `DieselPool`, `DieselBackend`, `DieselConfig` |
| `mysql` | | compiles diesel-async's MySQL driver. There is no MySQL plugin yet |
| `migrations` | | `MigrationsRunner`, re-exported `embed_migrations!` and `EmbeddedMigrations` |
| `sentry` | | query tracing; re-exports `SentryInstrumentation` |
| `chrono`, `uuid`, `serde_json`, `numeric`, `network` | | column-type support, forwarded to diesel |

SQLite is not supported: `diesel-async` has no SQLite backend, and this crate does not run sync diesel on a blocking pool. Use [`muxa-sqlx`](../muxa-sqlx) with its `sqlite` feature.

## pgmq

`DieselBackend` is the marker type that lets [`muxa-pgmq`](../muxa-pgmq) run over this pool: `PgmqPlugin::<DieselBackend, _>`.

## License

MIT OR Apache-2.0
