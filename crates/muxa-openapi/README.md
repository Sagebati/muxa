# muxa-openapi

OpenAPI for [muxa](../muxa). It serves an [aide](https://docs.rs/aide) OpenAPI document as JSON, next to an interactive [Scalar](https://scalar.com) docs page.

Through the facade this is the `openapi` feature.

## Usage

Most apps want `ApiPlugin` from [`muxa-web`](../muxa-web). It takes an aide `ApiRouter`, generates the document from it, serves both and runs the server, in one plugin.

`OpenApiPlugin` is the lower-level piece for when you finish the router yourself. Add it before `WebPlugin`:

```rust
use axum::{Json, Router};
use muxa::aide::axum::ApiRouter;
use muxa::aide::axum::routing::get;
use muxa::aide::openapi::OpenApi;
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

fn with_router<S>(router: Router) -> impl FnOnce(&S) -> Router + Send + 'static {
    move |_state: &S| router
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    let mut api = OpenApi::default();
    let router = ApiRouter::new()
        .api_route("/thing", get(thing))
        .finish_api(&mut api);

    App::default()
        .with_plugin(OpenApiPlugin::new(api)).await?       // /openapi.json + /docs
        .with_plugin(WebPlugin::new(with_router(router))).await?
        .run().await
}
```

`docs_router(&api, &cfg)` returns the same two routes as a plain `axum::Router`, for mounting by hand.

## Config

`[openapi]`, or `MUXA_OPENAPI__*`:

| Key | Default | |
|---|---|---|
| `json_path` | `"/openapi.json"` | where the document is served |
| `docs_path` | `"/docs"` | where the docs page is served |
| `title` | `"muxa API"` | title of the docs page |

`ApiPlugin` reads the same section. Use one or the other: never add both `ApiPlugin` and `OpenApiPlugin`.

The document is serialized once at startup. The docs page loads the Scalar viewer from the jsDelivr CDN, so the browser needs internet access to render it.

## aide and schemars

The crate re-exports `aide` and `schemars` at the versions it is built against. Use them through the facade, as `muxa::aide` and `muxa::schemars`. You do not need your own `aide` dependency. If you add one, it has to be exactly this version (`=0.16.0-alpha.4`), or your `OpenApi` type and muxa's stop being the same type.

The re-exported aide has the `axum`, `axum-json`, `axum-query` and `macros` features enabled.

The derives emit paths that start with the crate name, so bring the crate into scope where you derive:

```rust
use muxa::aide::{self, OperationIo};
use muxa::schemars::{self, JsonSchema};
```

## License

MIT OR Apache-2.0
