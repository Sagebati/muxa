//! muxa example — a JSON API backed by a **Diesel (async Postgres)** database,
//! with **OpenTelemetry** tracing. End-to-end testable via Docker Compose.
//!
//! Plugin chain: `OtelPlugin` → `DieselPlugin` (with embedded migrations) →
//! `WebPlugin`. The Diesel pool lands in the state HList; the routes callback
//! pulls it out (`HasPgExecutorFor<DieselBackend>`) and hands it to the axum
//! handlers as router state.
//!
//! # Run it
//!
//! ```text
//! # 1. Start Postgres + the otel-lgtm collector (waits until healthy):
//! docker compose -f crates/muxa/examples/diesel_widgets/docker-compose.yml up -d --wait
//!
//! # 2. Run the example (applies migrations + exports OTLP at startup):
//! cargo run -p muxa --example diesel_widgets \
//!     --features "diesel-migrations otel-otlp-tonic otel-metrics otel-logs otel-tracing-bridge"
//!
//! # 3. Hit the JSON endpoints:
//! curl -s localhost:3000/widgets | jq
//! curl -s -X POST localhost:3000/widgets \
//!     -H 'content-type: application/json' \
//!     -d '{"name":"widget","quantity":3}' | jq
//!
//! # 4. See traces/metrics/logs in Grafana (service `diesel-widgets`):
//! #    http://localhost:3001  (Explore → Tempo / Prometheus / Loki)
//!
//! # 5. Tear down:
//! docker compose -f crates/muxa/examples/diesel_widgets/docker-compose.yml down -v
//! ```
//!
//! Or use the justfile shortcut: `just diesel-example`. Without the OTLP
//! features the example still runs — `muxa-otel` just skips export.

use std::sync::LazyLock;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use diesel::prelude::*;
use opentelemetry::metrics::Counter;
use opentelemetry::{KeyValue, global};
// Named (not `as _`): the binding shadows the sync `diesel::RunQueryDsl` that
// `diesel::prelude::*` also pulls in, so `.load()` / `.get_result()` resolve
// unambiguously to the async executor instead of erroring on E0034.
#[allow(
    clippy::unused_trait_names,
    reason = "the named import disambiguates load/get_result from diesel's sync RunQueryDsl"
)]
use diesel_async::RunQueryDsl;
use muxa::prelude::*;
use serde::{Deserialize, Serialize};

/// Diesel schema for the `widgets` table (normally generated into
/// `schema.rs` by the diesel CLI; inlined here to keep the example single-file).
mod schema {
    diesel::table! {
        widgets (id) {
            id -> Int4,
            name -> Text,
            quantity -> Int4,
        }
    }
}

/// A row read out of `widgets`, serialized straight to JSON.
#[derive(Debug, Queryable, Selectable, Serialize)]
#[diesel(table_name = schema::widgets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct Widget {
    id: i32,
    name: String,
    quantity: i32,
}

/// The JSON body accepted by `POST /widgets`.
#[derive(Debug, Deserialize, Insertable)]
#[diesel(table_name = schema::widgets)]
struct NewWidget {
    name: String,
    #[serde(default)]
    quantity: i32,
}

/// Embedded migrations, applied at startup by `DieselPlugin::with_migrations`.
/// The path is relative to the crate root (`crates/muxa`).
const MIGRATIONS: EmbeddedMigrations = embed_migrations!("examples/diesel_widgets/migrations");

/// An OTel metric (exported to the collector): one count per handled request,
/// tagged by operation. Resolves against the global meter provider that
/// `OtelPlugin` installs at startup; first use happens per-request, well after.
static REQUESTS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("diesel-widgets")
        .u64_counter("widgets.requests")
        .with_description("Number of widget API requests handled.")
        .build()
});

/// `GET /widgets` — list every widget, oldest first.
///
/// `#[instrument]` opens a `tracing` span; with the `tracing-bridge` feature it
/// is exported to the collector as a trace.
#[tracing::instrument(name = "list_widgets", skip_all)]
async fn list_widgets(
    State(pool): State<DieselPool>,
) -> Result<Json<Vec<Widget>>, (StatusCode, String)> {
    REQUESTS.add(1, &[KeyValue::new("op", "list")]);
    let mut conn = pool.get().await.map_err(internal)?;
    let rows = schema::widgets::table
        .select(Widget::as_select())
        .order(schema::widgets::id.asc())
        .load(&mut conn)
        .await
        .map_err(internal)?;
    tracing::info!(count = rows.len(), "listed widgets");
    Ok(Json(rows))
}

/// `POST /widgets` — insert a widget and return the created row.
#[tracing::instrument(name = "create_widget", skip_all, fields(widget.name = %new.name))]
async fn create_widget(
    State(pool): State<DieselPool>,
    Json(new): Json<NewWidget>,
) -> Result<(StatusCode, Json<Widget>), (StatusCode, String)> {
    REQUESTS.add(1, &[KeyValue::new("op", "create")]);
    let mut conn = pool.get().await.map_err(internal)?;
    let row = diesel::insert_into(schema::widgets::table)
        .values(&new)
        .returning(Widget::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(internal)?;
    tracing::info!(id = row.id, "created widget");
    Ok((StatusCode::CREATED, Json(row)))
}

/// Map any error to a 500 with its message (fine for an example).
fn internal(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

/// Build the router. Pulls the Diesel pool out of the state HList and injects
/// it as axum router state so handlers can borrow connections.
fn routes<S>(state: &S) -> Router
where
    S: HasPgExecutorFor<DieselBackend>,
{
    let pool = state.pg_executor().dupe();
    Router::new()
        .route("/widgets", get(list_widgets).post(create_widget))
        .route("/health", get(|| async { "ok" }))
        .with_state(pool)
}

#[tokio::main]
async fn main() -> muxa::Result<()> {
    App::with_config_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/diesel_widgets/muxa.toml"
    ))
    // Observability first so later plugins' logs have a subscriber.
    .with_plugin(OtelPlugin)
    .await?
    // Async Postgres pool; runs pending migrations before publishing the pool.
    .with_plugin(DieselPlugin::new().with_migrations(MigrationsRunner::new(MIGRATIONS)))
    .await?
    // Web last — its routes callback sees the fully-composed state.
    .with_plugin(WebPlugin::new(routes))
    .await?
    .run()
    .await
}
