//! diesel-sentry — a diesel [`Instrumentation`] that mirrors connection, query,
//! and transaction events as `tracing` spans.
//!
//! Each span is tagged with both **Sentry** (`sentry.op`, `sentry.name`) and
//! **OpenTelemetry** (`db.system`, `db.statement`, `otel.kind`) field
//! conventions, so:
//!
//! - with a `sentry-tracing` layer installed, queries surface as Sentry DB
//!   spans nested under the active request transaction;
//! - with a `tracing-opentelemetry` layer, they become OTEL client spans.
//!
//! The crate is framework-agnostic — it depends only on `diesel` and
//! `tracing`. Install it globally so every diesel(-async) connection is
//! instrumented:
//!
//! ```ignore
//! diesel_sentry::install().expect("set diesel default instrumentation");
//! // … then build your pool; connections established afterwards are instrumented.
//! ```
//!
//! or per-connection: `conn.set_instrumentation(SentryInstrumentation::default())`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use diesel::connection::{Instrumentation, InstrumentationEvent};
use tracing::{Span, field, info_span};

/// Diesel [`Instrumentation`] that mirrors connection events as `info_span!`
/// lifecycles, so a `sentry-tracing` (or `tracing-opentelemetry`) layer turns
/// them into DB spans nested under the request transaction.
///
/// Spans are *stored, never entered*: the tracing→Sentry layer reads the
/// current hub/parent at span construction (`on_new_span`) and finalises on
/// drop (`on_close`), giving correct parent + duration without any `.enter()`
/// — which matters because diesel's events don't bracket an async scope.
///
/// `sentry.op` / `sentry.name` are the fields sentry-tracing reads to populate
/// the span's operation / description and categorise it as a DB span.
#[derive(Default)]
pub struct SentryInstrumentation {
    /// In-flight query span. Diesel emits exactly one `StartQuery` before each
    /// `FinishQuery` (even nested in transactions), so one slot suffices.
    query: Option<Span>,
    /// In-flight establish-connection span.
    connect: Option<Span>,
    /// Current transaction nesting depth, stamped onto child query spans.
    /// `0` means no transaction in progress.
    tx_depth: u32,
}

impl Instrumentation for SentryInstrumentation {
    fn on_connection_event(&mut self, event: InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartEstablishConnection { .. } => {
                self.connect = Some(info_span!(
                    "db.connect",
                    "sentry.op" = "db.connect",
                    "sentry.name" = "establish_connection",
                    "db.system" = "postgresql",
                    "otel.kind" = "client",
                    error = field::Empty,
                ));
            }
            InstrumentationEvent::FinishEstablishConnection { error, .. } => {
                if let Some(span) = self.connect.take()
                    && let Some(err) = error
                {
                    span.record("error", err.to_string().as_str());
                }
            }
            InstrumentationEvent::StartQuery { query, .. } => {
                // `DebugQuery::Display` formats as `"<sql> -- binds: [values]"`.
                // Strip the binds comment so `db.statement` carries only the
                // parameterised SQL — no PII risk from inlined parameter values.
                let full = query.to_string();
                let sql = full.split(" -- binds:").next().unwrap_or(&full).trim_end();
                let op = sql.split_whitespace().next().unwrap_or("").to_uppercase();
                self.query = Some(info_span!(
                    "db.query",
                    "sentry.op" = "db.sql.query",
                    "sentry.name" = %sql,
                    "db.system" = "postgresql",
                    "db.statement" = %sql,
                    "db.operation" = %op,
                    "db.transaction.depth" = self.tx_depth,
                    "otel.kind" = "client",
                    error = field::Empty,
                ));
            }
            InstrumentationEvent::FinishQuery { error, .. } => {
                if let Some(span) = self.query.take()
                    && let Some(err) = error
                {
                    span.record("error", err.to_string().as_str());
                }
            }
            InstrumentationEvent::CacheQuery { sql, .. } => {
                tracing::debug!(target: "diesel_sentry", sql = %sql, "cached prepared statement");
            }
            InstrumentationEvent::BeginTransaction { depth, .. } => {
                self.tx_depth = depth.get();
            }
            InstrumentationEvent::CommitTransaction { depth, .. }
            | InstrumentationEvent::RollbackTransaction { depth, .. } => {
                self.tx_depth = depth.get().saturating_sub(1);
            }
            _ => {}
        }
    }
}

/// Register [`SentryInstrumentation`] as diesel's process-global default
/// instrumentation, so every connection established afterwards is instrumented.
///
/// Call this **before** building your connection pool (diesel(-async) seeds a
/// new connection's instrumentation from this global on establish). Returns the
/// diesel error if the global lock is poisoned.
pub fn install() -> diesel::QueryResult<()> {
    diesel::connection::set_default_instrumentation(|| -> Option<Box<dyn Instrumentation>> {
        Some(Box::new(SentryInstrumentation::default()))
    })
}
