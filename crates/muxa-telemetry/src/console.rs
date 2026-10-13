//! The wasm output layer: writes events to the JS console.

use core::fmt::Write as _;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Stands in for `fmt` on wasm, which has no stdout and no clock: formats each
/// event like `fmt` does, minus the timestamp (the host adds its own), and
/// hands the line to `emit`.
///
/// `INFO request{id=7}: my_app::sync: synced club=foo slots=12`
pub struct ConsoleLayer<E> {
    emit: E,
}

impl<E> ConsoleLayer<E> {
    pub const fn new(emit: E) -> Self {
        Self { emit }
    }
}

/// The JS console (`console.error` / `warn` / `log` / `debug` by level) —
/// what `wrangler tail`, the Workers dashboard and browser devtools show.
#[cfg(target_family = "wasm")]
pub fn js_console(level: &Level, line: &str) {
    use web_sys::console;

    // Straight to JS: one conversion, no re-formatting of the line.
    let line = wasm_bindgen::JsValue::from_str(line);
    match *level {
        Level::ERROR => console::error_1(&line),
        Level::WARN => console::warn_1(&line),
        Level::INFO => console::log_1(&line),
        Level::DEBUG | Level::TRACE => console::debug_1(&line),
    }
}

/// A span's fields, rendered once when it is created (and on `record`), so
/// every event inside it can print them.
struct SpanFields(String);

impl<S, E> Layer<S> for ConsoleLayer<E>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    E: Fn(&Level, &str) + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = String::new();
        attrs.record(&mut LineVisitor { line: &mut fields });
        span.extensions_mut().insert(SpanFields(fields));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if let Some(fields) = span.extensions_mut().get_mut::<SpanFields>() {
            values.record(&mut LineVisitor {
                line: &mut fields.0,
            });
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // Writing to a String can't fail.
        let mut line = String::new();
        let _ = write!(line, "{} ", metadata.level());
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                line.push_str(span.name());
                if let Some(fields) = span.extensions().get::<SpanFields>()
                    && !fields.0.is_empty()
                {
                    let _ = write!(line, "{{{}}}", fields.0.trim_start());
                }
                line.push_str(": ");
            }
        }
        let _ = write!(line, "{}:", metadata.target());
        event.record(&mut LineVisitor { line: &mut line });

        (self.emit)(metadata.level(), &line);
    }
}

/// Appends fields to a line: the message bare, then `key=value`.
struct LineVisitor<'line> {
    line: &'line mut String,
}

impl Visit for LineVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        // Writing to a String can't fail.
        let _ = if field.name() == "message" {
            write!(self.line, " {value}")
        } else {
            write!(self.line, " {}={value}", field.name())
        };
    }

    fn record_debug(&mut self, field: &Field, value: &dyn core::fmt::Debug) {
        let _ = if field.name() == "message" {
            write!(self.line, " {value:?}")
        } else {
            write!(self.line, " {}={value:?}", field.name())
        };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    #[test]
    fn lines_look_like_fmt_without_the_timestamp() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let layer = ConsoleLayer::new(move |level: &Level, line: &str| {
            sink.lock().unwrap().push(format!("[{level}] {line}"));
        });

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "app", "outside any span");
            let span = tracing::info_span!("request", id = 7);
            let _entered = span.enter();
            span.record("id", 8);
            tracing::info!(target: "app::sync", club = "foo", "synced");
        });

        assert_eq!(
            *lines.lock().unwrap(),
            [
                "[WARN] WARN app: outside any span",
                "[INFO] INFO request{id=7 id=8}: app::sync: synced club=foo",
            ]
        );
    }
}
