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

use std::borrow::Cow;
use std::fmt;

use diesel::connection::{Instrumentation, InstrumentationEvent};
use tracing::{Span, field, info_span};

/// What diesel's `DebugQuery` `Display` writes between the SQL and its bind
/// values: `"<sql> -- binds: [values]"`.
const BINDS_MARKER: &str = " -- binds:";

/// Past this size the SQL buffer is released after use instead of kept, so one
/// huge statement (a bulk insert) doesn't pin its memory on the connection.
const MAX_RETAINED_SQL: usize = 16 * 1024;

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
    /// Scratch buffer the SQL of each query is rendered into. Reused from one
    /// query to the next, so steady-state queries allocate nothing here.
    sql: String,
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
                    span.record("error", field::display(err));
                }
            }
            InstrumentationEvent::StartQuery { query, .. } => {
                // Only the parameterised SQL goes on the span: the bind values
                // are never even formatted, so no PII risk from inlined values.
                write_sql(&mut self.sql, query);
                let sql = self.sql.as_str();
                let op = operation(sql);
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
                if self.sql.capacity() > MAX_RETAINED_SQL {
                    self.sql = String::new();
                }
            }
            InstrumentationEvent::FinishQuery { error, .. } => {
                if let Some(span) = self.query.take()
                    && let Some(err) = error
                {
                    span.record("error", field::display(err));
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

/// Render `query`'s SQL into `buf` (cleared first), without its bind values.
///
/// Formatting is cut off at [`BINDS_MARKER`], so the binds are never rendered.
/// A query whose `Display` fails leaves whatever it wrote so far (possibly
/// nothing) instead of panicking like `to_string` would.
fn write_sql<Q: fmt::Display + ?Sized>(buf: &mut String, query: &Q) {
    buf.clear();
    // An error here is either the cut-off or a failed render; both are fine.
    let _ = fmt::write(&mut UntilBinds(buf), format_args!("{query}"));
    let end = buf.trim_end().len();
    buf.truncate(end);
}

/// A `fmt::Write` sink that stops the formatting (with `fmt::Error`) as soon as
/// [`BINDS_MARKER`] has been written, keeping only what came before it.
struct UntilBinds<'buf>(&'buf mut String);

impl fmt::Write for UntilBinds<'_> {
    fn write_str(&mut self, chunk: &str) -> fmt::Result {
        // The marker may straddle two chunks, so look back one marker length.
        let from = self.0.len().saturating_sub(BINDS_MARKER.len());
        self.0.push_str(chunk);
        let found = self.0.as_bytes()[from..]
            .windows(BINDS_MARKER.len())
            .position(|window| window == BINDS_MARKER.as_bytes());
        match found {
            Some(at) => {
                // The marker starts with an ASCII space: a char boundary.
                self.0.truncate(from + at);
                Err(fmt::Error)
            }
            None => Ok(()),
        }
    }
}

/// The statement's leading keyword (`SELECT`, `INSERT`, …), upper-cased.
/// Borrowed when it already is upper case, which is how diesel emits it.
fn operation(sql: &str) -> Cow<'_, str> {
    let op = sql.split_whitespace().next().unwrap_or("");
    if op.bytes().any(|byte| byte.is_ascii_lowercase()) {
        Cow::Owned(op.to_ascii_uppercase())
    } else {
        Cow::Borrowed(op)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for diesel's bind list: formatting it is a test failure.
    struct UnformattableBinds;

    impl fmt::Debug for UnformattableBinds {
        fn fmt(&self, _formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            panic!("bind values must never be formatted");
        }
    }

    /// Formats like diesel's `DebugQuery`: `"{sql} -- binds: {binds:?}"`.
    struct LikeDebugQuery(&'static str);

    impl fmt::Display for LikeDebugQuery {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let sql = self.0;
            let binds = UnformattableBinds;
            write!(formatter, "{sql} -- binds: {binds:?}")
        }
    }

    /// A query whose rendering fails, as diesel's does when `to_sql` errors.
    struct FailingQuery;

    impl fmt::Display for FailingQuery {
        fn fmt(&self, _formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            Err(fmt::Error)
        }
    }

    #[test]
    fn sql_stops_before_the_binds() {
        let mut buf = String::new();
        write_sql(
            &mut buf,
            &LikeDebugQuery("SELECT \"id\" FROM \"widgets\" WHERE \"id\" = $1"),
        );
        assert_eq!(buf, "SELECT \"id\" FROM \"widgets\" WHERE \"id\" = $1");
    }

    #[test]
    fn marker_split_across_writes_is_found() {
        struct Split;
        impl fmt::Display for Split {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("SELECT 1 -- bi")?;
                formatter.write_str("nds: ")?;
                write!(formatter, "{:?}", UnformattableBinds)
            }
        }
        let mut buf = String::new();
        write_sql(&mut buf, &Split);
        assert_eq!(buf, "SELECT 1");
    }

    #[test]
    fn raw_sql_without_binds_is_kept_whole_and_buffer_is_reused() {
        let mut buf = String::from("stale contents from the previous query");
        write_sql(&mut buf, "BEGIN  ");
        assert_eq!(buf, "BEGIN");
    }

    #[test]
    fn failing_render_yields_empty_sql_instead_of_panicking() {
        let mut buf = String::new();
        write_sql(&mut buf, &FailingQuery);
        assert_eq!(buf, "");
    }

    #[test]
    fn operation_is_upper_cased_and_borrowed_when_already_upper() {
        assert!(matches!(operation("SELECT 1"), Cow::Borrowed("SELECT")));
        assert_eq!(operation("insert into t values (1)"), "INSERT");
        assert_eq!(operation(""), "");
    }
}
