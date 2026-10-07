# muxa-sqlx

[SQLx](https://docs.rs/sqlx) connection-pool plugins for [muxa](../muxa): Postgres and SQLite.

Through the facade these are the `sqlx` (Postgres) and `sqlite` features.

## Postgres

```rust
use axum::Router;
use axum::routing::get;
use muxa::prelude::*;

fn routes<S, Idx>(state: &S) -> Router
where
    S: Selector<SqlxPool, Idx>,
{
    let pool: SqlxPool = state.select().dupe();
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .with_state(pool)
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::default()
        .with_plugin(OtelPlugin).await?
        .with_plugin(SqlxPlugin).await?
        .with_plugin(WebPlugin::new(routes)).await?
        .run().await
}
```

`SqlxPlugin` connects at startup and pushes a `SqlxPool` onto the app state.

`[sqlx]`, or `MUXA_SQLX__*`:

| Key | Default | |
|---|---|---|
| `url` | none, required | Postgres connection URL. Startup fails if it is empty |
| `max_connections` | `10` | |
| `min_connections` | `0` | |

`url` is held as a `SecretString`, so a `Debug` print of the config does not reveal it.

## SQLite

`SqlitePlugin` pushes a `SqlitePool`. Use it the same way, with `Selector<SqlitePool, Idx>`.

`[sqlite]`, or `MUXA_SQLITE__*`:

| Key | Default | |
|---|---|---|
| `url` | `"sqlite::memory:"` | any sqlx SQLite URL, e.g. `sqlite:./data.db` |
| `create_if_missing` | `true` | create the database file if it does not exist |
| `max_connections` | `5` | |

The default is an in-memory database, which is gone once the pool is dropped. It suits tests and examples.

## Running queries

`SqlxPool` and `SqlitePool` are thin wrappers that deref to `sqlx::PgPool` and `sqlx::SqlitePool`. Cloning one (`dupe()`) is a reference-count bump.

This crate does not re-export sqlx. Add it to your own `Cargo.toml` at the same version line (0.8) and pass the pool where sqlx expects an executor:

```rust
let (one,): (i64,) = sqlx::query_as("SELECT 1").fetch_one(&*pool).await?;
```

The facade forwards sqlx's optional features as `sqlx-macros`, `sqlx-migrate`, `sqlx-chrono`, `sqlx-uuid`, `sqlx-json` and so on.

## pgmq

`SqlxBackend` is the marker type that lets [`muxa-pgmq`](../muxa-pgmq) run over the Postgres pool: `PgmqPlugin::<SqlxBackend, _>`. SQLite has no such marker, because pgmq is Postgres-only.

## Features

| Feature | Default | Effect |
|---|---|---|
| `postgres` | yes | `SqlxPlugin`, `SqlxPool`, `SqlxBackend`, `SqlxConfig` |
| `sqlite` | | `SqlitePlugin`, `SqlitePool`, `SqliteConfig` |
| `mysql` | | compiles the sqlx MySQL driver. There is no MySQL plugin yet |
| `tls-rustls` | yes | TLS through rustls |
| `tls-native` | | TLS through native-tls. Pick one of the two |
| `macros` | | `sqlx::query!` and friends |
| `migrate` | | `sqlx::migrate!` |
| `chrono`, `time`, `uuid`, `json`, `bigdecimal`, `rust_decimal`, `ipnet`, `mac_address`, `bit-vec` | | column-type support, forwarded to sqlx |
| `db-traces` | | reserved; has no effect today |

## License

MIT OR Apache-2.0
