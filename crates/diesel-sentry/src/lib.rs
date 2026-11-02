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
//! or per-connection, on an already-open connection (which never reports its URL,
//! so name the backend):
//! `conn.set_instrumentation(SentryInstrumentation::default().with_db_system("postgresql"))`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::hash::{BuildHasher as _, BuildHasherDefault, DefaultHasher};

use diesel::connection::{Instrumentation, InstrumentationEvent};
use tracing::{Level, Span, field, info_span};

/// What diesel's `DebugQuery` `Display` writes between the SQL and its bind
/// values: `"<sql> -- binds: [values]"`.
const BINDS_MARKER: &str = " -- binds:";

/// Past this size the SQL buffer is shrunk back after use, so one huge
/// statement (a bulk insert) doesn't pin its memory on the connection.
const MAX_RETAINED_SQL: usize = 16 * 1024;

/// At most this many query spans are tracked as in flight on one connection.
/// Beyond it the oldest is dropped (closed): a query whose future was dropped
/// never sends its `FinishQuery`, and must not be kept forever.
const MAX_IN_FLIGHT: usize = 32;

/// OpenTelemetry's `db.system` for a backend it has no name for.
const OTHER_SQL: &str = "other_sql";

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
///
/// `db.system` comes from the scheme of the URL the connection is established
/// with (`postgres://` → `postgresql`, `mysql://` → `mysql`, …). Attached to an
/// already-open connection, the instrumentation never sees that URL: name the
/// backend with [`with_db_system`](Self::with_db_system), or it reports
/// `other_sql`.
#[derive(Default)]
pub struct SentryInstrumentation {
    /// In-flight query spans, oldest first, each keyed by a hash of its SQL.
    /// diesel-async pipelines queries on one connection — several `StartQuery`
    /// can come before their `FinishQuery` — so a finish is matched to its
    /// start by SQL rather than assumed to be the latest one.
    queries: VecDeque<(u64, Span)>,
    /// In-flight establish-connection span.
    connect: Option<Span>,
    /// Current transaction nesting depth, stamped onto child query spans.
    /// `0` means no transaction in progress.
    tx_depth: u32,
    /// The `db.system` reported on spans; `None` until known.
    db_system: Option<&'static str>,
    /// Scratch buffer the SQL of each query is rendered into. Reused from one
    /// query to the next, so steady-state queries allocate nothing here.
    sql: String,
}

impl SentryInstrumentation {
    /// Report `db_system` (an OpenTelemetry `db.system` value such as
    /// `"postgresql"`) on every span, instead of deriving it from the URL the
    /// connection is established with.
    #[must_use]
    pub fn with_db_system(mut self, db_system: &'static str) -> Self {
        self.db_system = Some(db_system);
        self
    }

    fn db_system(&self) -> &'static str {
        self.db_system.unwrap_or(OTHER_SQL)
    }

    fn start_query<Q: fmt::Display + ?Sized>(&mut self, query: &Q) {
        // Rendering the SQL costs a query-builder pass: skip it when nobody
        // listens to these spans (`info_span!` would come back disabled).
        if !tracing::span_enabled!(Level::INFO) {
            return;
        }
        // Only the parameterised SQL goes on the span: the bind values are
        // never even formatted, so no PII risk from inlined values.
        write_sql(&mut self.sql, query);
        let sql = self.sql.as_str();
        let op = operation(sql);
        let span = info_span!(
            "db.query",
            "sentry.op" = "db.sql.query",
            "sentry.name" = %sql,
            "db.system" = self.db_system(),
            "db.statement" = %sql,
            "db.operation" = %op,
            "db.transaction.depth" = self.tx_depth,
            "otel.kind" = "client",
            error = field::Empty,
        );
        if self.queries.len() == MAX_IN_FLIGHT {
            self.queries.pop_front();
        }
        self.queries.push_back((sql_key(sql), span));
        self.release_large_buffer();
    }

    fn finish_query<Q, E>(&mut self, query: &Q, error: Option<&E>)
    where
        Q: fmt::Display + ?Sized,
        E: fmt::Display + ?Sized,
    {
        let span = if self.queries.len() <= 1 {
            // Not pipelined: the only query in flight is this one.
            self.queries.pop_front()
        } else {
            write_sql(&mut self.sql, query);
            let key = sql_key(&self.sql);
            self.release_large_buffer();
            // The oldest start with the same SQL. No match — a finish whose
            // SQL renders differently from its start — leaves the queue alone
            // rather than closing another query's span.
            self.queries
                .iter()
                .position(|(known, _)| *known == key)
                .and_then(|index| self.queries.remove(index))
        };
        if let Some((_, span)) = span
            && let Some(err) = error
        {
            span.record("error", field::display(err));
        }
    }

    fn release_large_buffer(&mut self) {
        if self.sql.capacity() > MAX_RETAINED_SQL {
            self.sql.clear();
            self.sql.shrink_to(MAX_RETAINED_SQL);
        }
    }
}

impl Instrumentation for SentryInstrumentation {
    fn on_connection_event(&mut self, event: InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartEstablishConnection { url, .. } => {
                self.db_system = self.db_system.or_else(|| Some(db_system(url)));
                self.connect = Some(info_span!(
                    "db.connect",
                    "sentry.op" = "db.connect",
                    "sentry.name" = "establish_connection",
                    "db.system" = self.db_system(),
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
            InstrumentationEvent::StartQuery { query, .. } => self.start_query(query),
            InstrumentationEvent::FinishQuery { query, error, .. } => {
                self.finish_query(query, error);
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

/// The OpenTelemetry `db.system` for the backend `url` points at. Only the
/// scheme is read: the rest of a connection URL can carry credentials.
fn db_system(url: &str) -> &'static str {
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    let is = |name: &str| scheme.eq_ignore_ascii_case(name);
    if is("postgres") || is("postgresql") {
        "postgresql"
    } else if is("mysql") || is("mariadb") {
        "mysql"
    } else if is("sqlite") || is("file") {
        "sqlite"
    } else {
        OTHER_SQL
    }
}

/// Key a query's SQL for matching its `FinishQuery` to its `StartQuery`.
fn sql_key(sql: &str) -> u64 {
    BuildHasherDefault::<DefaultHasher>::default().hash_one(sql)
}

/// Render `query`'s SQL into `buf` (cleared first), without its bind values.
///
/// Formatting is cut off at [`BINDS_MARKER`], so the binds are never rendered.
/// A query whose `Display` fails leaves whatever it wrote so far (possibly
/// nothing) instead of panicking like `to_string` would.
fn write_sql<Q: fmt::Display + ?Sized>(buf: &mut String, query: &Q) {
    buf.clear();
    // An error here is either the cut-off or a failed render; both are fine.
    let _ = fmt::write(&mut UntilBinds { buf, cut: false }, format_args!("{query}"));
    let end = buf.trim_end().len();
    buf.truncate(end);
}

/// A `fmt::Write` sink that stops the formatting (with `fmt::Error`) as soon as
/// [`BINDS_MARKER`] has been written, keeping only what came before it.
struct UntilBinds<'buf> {
    buf: &'buf mut String,
    /// Set once the marker is found. From then on every write is refused, so
    /// the binds stay out even if a `Display` ignores the error and goes on.
    cut: bool,
}

impl fmt::Write for UntilBinds<'_> {
    fn write_str(&mut self, chunk: &str) -> fmt::Result {
        if self.cut {
            return Err(fmt::Error);
        }
        // The marker may straddle two chunks, so look back one marker length.
        let from = self.buf.len().saturating_sub(BINDS_MARKER.len());
        self.buf.push_str(chunk);
        let found = self.buf.as_bytes()[from..]
            .windows(BINDS_MARKER.len())
            .position(|window| window == BINDS_MARKER.as_bytes());
        match found {
            Some(at) => {
                // The marker starts with an ASCII space: a char boundary.
                self.buf.truncate(from + at);
                self.cut = true;
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

    /// Writes the binds even after its first write was refused, as a
    /// `Display` that ignores write errors would.
    struct IgnoresWriteErrors;

    impl fmt::Display for IgnoresWriteErrors {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let _ = formatter.write_str("SELECT 1 -- binds: ");
            let _ = formatter.write_str("[\"alice@example.com\"]");
            Ok(())
        }
    }

    #[test]
    fn nothing_is_written_after_the_cut() {
        let mut buf = String::new();
        write_sql(&mut buf, &IgnoresWriteErrors);
        assert_eq!(buf, "SELECT 1");
    }

    /// The marker is diesel's private `Display` format: if diesel changes it,
    /// this fails instead of bind values silently reaching the spans.
    #[test]
    fn real_diesel_debug_query_loses_its_binds() {
        use diesel::sql_types::Text;

        let query = diesel::sql_query("SELECT id FROM users WHERE email = $1")
            .bind::<Text, _>("alice@example.com");
        let debug = diesel::debug_query::<diesel::pg::Pg, _>(&query);
        assert!(debug.to_string().contains("alice@example.com"));

        let mut buf = String::new();
        write_sql(&mut buf, &debug);
        assert_eq!(buf, "SELECT id FROM users WHERE email = $1");
    }

    #[test]
    fn db_system_comes_from_the_url_scheme_only() {
        assert_eq!(db_system("postgres://user:secret@db/app"), "postgresql");
        assert_eq!(db_system("PostgreSQL://db/app"), "postgresql");
        assert_eq!(db_system("mysql://db/app"), "mysql");
        assert_eq!(db_system("file:app.db"), "sqlite");
        assert_eq!(db_system("app.db"), OTHER_SQL);
    }

    /// Runs `body` with a subscriber that enables every span.
    fn with_spans(body: impl FnOnce()) {
        tracing::subscriber::with_default(tracing_subscriber::registry(), body);
    }

    #[test]
    fn pipelined_finishes_close_their_own_spans() {
        with_spans(|| {
            let mut instr = SentryInstrumentation::default();
            instr.start_query(&LikeDebugQuery("SET TIME ZONE 'UTC'"));
            instr.start_query(&LikeDebugQuery("SET CLIENT_ENCODING TO 'UTF8'"));
            assert_eq!(instr.queries.len(), 2);

            // The second finishes first; diesel-async reports a finish by its bare SQL.
            instr.finish_query("SET CLIENT_ENCODING TO 'UTF8'", None::<&str>);
            let remaining: Vec<u64> = instr.queries.iter().map(|(key, _)| *key).collect();
            assert_eq!(remaining, [sql_key("SET TIME ZONE 'UTC'")]);

            instr.finish_query("SET TIME ZONE 'UTC'", None::<&str>);
            assert!(instr.queries.is_empty());
        });
    }

    #[test]
    fn unfinished_queries_are_not_kept_forever() {
        with_spans(|| {
            let mut instr = SentryInstrumentation::default();
            for _ in 0..MAX_IN_FLIGHT + 5 {
                instr.start_query(&LikeDebugQuery("SELECT 1"));
            }
            assert_eq!(instr.queries.len(), MAX_IN_FLIGHT);
        });
    }

    #[test]
    fn disabled_spans_skip_rendering() {
        // No subscriber: `db.query` spans are disabled, so nothing is tracked.
        let mut instr = SentryInstrumentation::default();
        instr.start_query(&LikeDebugQuery("SELECT 1"));
        assert!(instr.queries.is_empty());
        assert!(instr.sql.is_empty());
    }

    #[test]
    fn large_buffers_are_shrunk_not_dropped() {
        let mut instr = SentryInstrumentation::default();
        instr.sql.reserve(4 * MAX_RETAINED_SQL);
        instr.sql.push_str("SELECT 1");
        instr.release_large_buffer();
        assert!(instr.sql.is_empty());
        assert!(instr.sql.capacity() >= MAX_RETAINED_SQL);
        assert!(instr.sql.capacity() < 4 * MAX_RETAINED_SQL);
    }

    #[test]
    fn operation_is_upper_cased_and_borrowed_when_already_upper() {
        assert!(matches!(operation("SELECT 1"), Cow::Borrowed("SELECT")));
        assert_eq!(operation("insert into t values (1)"), "INSERT");
        assert_eq!(operation(""), "");
    }
}
