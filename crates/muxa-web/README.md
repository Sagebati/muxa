# muxa-web

The web plugin of [muxa](../muxa). `WebPlugin` takes your routes, composes them with whatever routes and middleware other plugins registered, and runs the axum serve loop with graceful shutdown.

Through the facade this is the `web` feature, which is on by default.

## WebPlugin

```rust
use axum::Router;
use axum::routing::get;
use muxa::prelude::*;

fn routes<S>(_state: &S) -> Router {
    Router::new().route("/", get(|| async { "hi" }))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::default()
        .with_plugin(WebPlugin::new(routes)).await?
        .run().await
}
```

Add it last. The routes callback is called with the app state at the point the plugin is added, so everything it needs has to be in the chain already. `WebPlugin` also registers the serve loop, and only one plugin can do that.

### The routes callback

The callback is `FnOnce(&S) -> Router`. Three forms work:

```rust
// 1. a fn item that ignores the state
fn routes<S>(_state: &S) -> Router { /* … */ }

// 2. a fn item that pulls a resource out of the state
fn routes<S, Idx>(state: &S) -> Router
where
    S: Selector<SqlitePool, Idx>,
{
    let pool: SqlitePool = state.select().dupe();
    Router::new().route("/health", get(|| async { "ok" })).with_state(pool)
}

// 3. a router you already built
fn with_router<S>(router: Router) -> impl FnOnce(&S) -> Router + Send + 'static {
    move |_state: &S| router
}
// … .with_plugin(WebPlugin::new(with_router(router)))
```

A bare closure, `WebPlugin::new(move |_state| router)`, does not compile: the closure is inferred for one lifetime and the plugin needs it for all of them (`FnOnce` is not general enough). The helper in form 3 fixes that through its return type.

`muxa_web::no_routes` is a ready-made callback for an app whose routes all come from plugins.

### Config

`[web]`, or `MUXA_WEB__*`:

| Key | Default | |
|---|---|---|
| `host` | `"0.0.0.0"` | bind address |
| `port` | `3000` | `0` lets the OS pick |
| `banner` | `true` | print the launch banner to stderr |

The banner shows the bound URL and the mounted route prefixes. It does not print configuration.

### Shutdown and peer address

With the `graceful-shutdown` feature (default), ctrl-c cancels the app's `ShutdownToken`. The server stops accepting connections and background tasks see the cancellation. Anything else holding the token can trigger the same shutdown.

The server is started with `into_make_service_with_connect_info::<SocketAddr>()`, so handlers and layers can extract `ConnectInfo<SocketAddr>`.

## ApiPlugin

With the `openapi` feature, `ApiPlugin` replaces `WebPlugin` when your routes are an [aide](https://docs.rs/aide) `ApiRouter`. It finishes the router into axum routes plus an OpenAPI document, serves the document and a docs page, and runs the serve loop.

```rust
use axum::Json;
use muxa::aide::axum::ApiRouter;
use muxa::aide::axum::routing::get;
use muxa::aide::openapi::{Info, OpenApi};
use muxa::prelude::*;
use muxa::schemars::{self, JsonSchema};
use serde::Serialize;

#[derive(Serialize, JsonSchema)]
struct Thing {
    name: String,
}

async fn thing() -> Json<Thing> {
    Json(Thing { name: "widget".to_owned() })
}

fn api<S>(_state: &S) -> ApiRouter {
    ApiRouter::new().api_route("/thing", get(thing))
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    let seed = OpenApi {
        info: Info {
            title: "my-api".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            ..Info::default()
        },
        ..OpenApi::default()
    };
    App::default()
        .with_plugin(ApiPlugin::new(api, seed)).await?
        .run().await
}
```

Its config, `ApiConfig`, is two sections: `[web]` for the listener and `[openapi]` for the spec and docs paths (see [`muxa-openapi`](../muxa-openapi)). An invalid value in either fails startup.

`ApiPlugin` is an alternative to `WebPlugin` + `OpenApiPlugin`. Never add both. The same rule about callbacks applies.

## Rate limiting

With the `ratelimit` feature, `ratelimit::per_ip_layer` builds a per-client-IP tower layer on [tower_governor](https://docs.rs/tower_governor). Apply it to the routes you want to protect:

```rust
let app = App::default().with_plugin(OtelPlugin).await?;

let cfg: RateLimitConfig = app
    .figment()
    .extract_inner("ratelimit")
    .unwrap_or_default();

let mut submit = Router::new().route("/submit", post(|| async { "queued" }));
if cfg.enabled {
    submit = submit.layer(per_ip_layer(&cfg)?);
}

app.with_plugin(WebPlugin::new(with_router(submit))).await?
    .run().await
```

No plugin reads the rate-limit config. You deserialize `RateLimitConfig` from whichever section you like and decide where the layer goes.

| Key | Default | |
|---|---|---|
| `enabled` | `true` | a flag for your own code; `per_ip_layer` does not check it |
| `period_ms` | `1000` | time to restore one request of allowance |
| `burst` | `5` | requests allowed at once |
| `trust_forwarded_for` | `false` | take the client IP from `X-Forwarded-For` / `X-Real-IP` / `Forwarded` |

With `period_ms = 10000, burst = 5`, a client can send 5 requests at once and then one more every 10 seconds. Responses carry `x-ratelimit-*` and `retry-after` headers.

Things to know:

- Buckets live in process memory. With N replicas, a client effectively gets N times the limit. Treat it as an abuse guard and put hard global caps at the edge.
- Leave `trust_forwarded_for` off unless a proxy you control sets those headers, or clients can spoof their IP. With it off behind a proxy, every client shares the proxy's bucket.
- `per_ip_layer` spawns a small cleanup task, so call it inside a Tokio runtime. The task ends once the router is dropped.

## Features

| Feature | Default | Effect |
|---|---|---|
| `graceful-shutdown` | yes | ctrl-c handling (`tokio/signal`) |
| `openapi` | | `ApiPlugin` |
| `ratelimit` | | the `ratelimit` module |

## License

MIT OR Apache-2.0
