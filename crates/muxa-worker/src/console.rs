//! A tracing layer that writes events to the Workers console.

use core::fmt::Write as _;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// A `tracing` layer that writes each event to the Workers console
/// (`console.error` / `warn` / `log` / `debug` by level), which is what
/// `wrangler tail` and the dashboard's logs show.
///
/// It stands in for the `fmt` layer muxa installs natively, which can't work
/// on Workers (no stdout, no clock). [`WorkerPlugin`](crate::WorkerPlugin)
/// attaches it; lines look like `INFO my_app::sync: synced club=foo slots=12`.
/// The platform adds its own timestamps.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConsoleLayer;

impl<S: Subscriber> Layer<S> for ConsoleLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut line = format!("{} {}:", metadata.level(), metadata.target());
        event.record(&mut LineVisitor { line: &mut line });

        match *metadata.level() {
            Level::ERROR => worker::console_error!("{line}"),
            Level::WARN => worker::console_warn!("{line}"),
            Level::INFO => worker::console_log!("{line}"),
            Level::DEBUG | Level::TRACE => worker::console_debug!("{line}"),
        }
    }
}

/// Appends an event's fields to a line: the message bare, then `key=value`.
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
