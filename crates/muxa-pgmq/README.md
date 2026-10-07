# muxa-pgmq

[pgmq](https://github.com/Sagebati/pgmq) (a Postgres-backed message queue) for [muxa](../muxa).

`PgmqPlugin` is an install-only plugin. At startup it installs pgmq into the database and makes sure your queues exist. It adds nothing to the app state. You send and read messages with pgmq's own API on the pool you already have.

Through the facade this is the `pgmq` feature, together with `sqlx` or `diesel` for the pool.

## Usage

```rust
use muxa::prelude::*;

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::default()
        .with_plugin(DieselPlugin::new()).await?
        .with_plugin(PgmqPlugin::<DieselBackend>::new().queues(["jobs"])).await?
        // … WebPlugin last
        .run().await
}
```

The type parameter names the pool to use: `DieselBackend` (from `muxa-diesel`) or `SqlxBackend` (from `muxa-sqlx`). The matching pool plugin has to come earlier in the chain. If it does not, the app does not compile: *"no Postgres pool available for backend … in the app state"*.

When other plugins sit between the pool and `PgmqPlugin`, add a second parameter for the compiler to infer:

```rust
.with_plugin(DieselPlugin::new()).await?
.with_plugin(OtelPlugin).await?
.with_plugin(PgmqPlugin::<DieselBackend, _>::new().queues(["jobs", "emails"])).await?
```

At build the plugin:

1. borrows the pool from the app state,
2. installs pgmq from the SQL embedded in the pgmq crate (idempotent),
3. creates each declared queue.

## Config

`[pgmq]`, or `MUXA_PGMQ__*`:

| Key | Default | |
|---|---|---|
| `queues` | `[]` | queues to create at startup |

Queues from the config and from `.queues([...])` are merged and de-duplicated.

## Sending and reading

There is no muxa client. Queue operations are the methods of pgmq's `Queue` trait, called on a connection from the pool. Add pgmq to your own `Cargo.toml` at the revision this workspace pins, so there is a single pgmq in the build:

```toml
pgmq = { git = "https://github.com/Sagebati/pgmq", rev = "e243a26a60a0c3a43eefca2051156556715e875e", default-features = false, features = ["diesel-async"] }
```

With the diesel backend it looks like this, where `job` is any `Serialize` value and `Job` its `Deserialize` type:

```rust
use pgmq::Queue as _;

let mut conn = pool.get().await?;
let msg_id = (&mut *conn).send("jobs", &job).await?;

if let Some(msg) = (&mut *conn).read::<Job>("jobs", 30).await? {   // invisible for 30 s
    // … handle msg.message …
    (&mut *conn).archive("jobs", msg.msg_id).await?;
}
```

The trait is implemented for `&mut AsyncPgConnection` (and, with the sqlx backend, for `&mut PgConnection`) and its methods take `self`, hence the reborrow on each call.

## Features

| Feature | Effect |
|---|---|
| `sqlx` | the `Plugin` impl for `PgmqPlugin<SqlxBackend, _>` |
| `diesel-async` | the `Plugin` impl for `PgmqPlugin<DieselBackend, _>` |
| `tracing-spans` | spans around queue operations (pgmq's `tracing` feature) |

No backend is enabled by default. The facade turns on the one matching its own `sqlx` / `diesel` feature.

There is one `Plugin` impl per backend, in feature-gated modules, and no trait abstracting over backends. That is deliberate: it keeps each build future concrete and avoids a Rust limitation around `Send` inference with sqlx's `Executor<'_>`.

pgmq is Postgres-only, so there is no SQLite or MySQL backend.

## Tests

The tests are compile-time checks that the plugin composes with each pool. They are not part of `cargo test --workspace`:

```bash
cargo test -p muxa-pgmq --features sqlx,diesel-async --tests
```

or `just test-pgmq`.

## License

MIT OR Apache-2.0
