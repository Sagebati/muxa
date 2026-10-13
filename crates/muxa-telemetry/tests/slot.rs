//! The plugin-layer slot is process-wide, so this is one test in its own
//! binary: it installs the global subscriber and walks through the slot's
//! behaviour in order.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use muxa_telemetry::TelemetryRegistry;
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Counts the events that reach it.
struct Counting(Arc<AtomicUsize>);

impl<S: Subscriber> Layer<S> for Counting {
    fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn plugin_layers_share_one_slot() {
    let first = TelemetryRegistry::install();
    assert!(first.is_installed());

    let seen = Arc::new(AtomicUsize::new(0));
    first.add_layer(Counting(Arc::clone(&seen)));
    tracing::info!("reaches the layer");
    assert_eq!(seen.load(Ordering::SeqCst), 1);

    // A second registry — a second app in the same process — is attached to
    // the same slot rather than silently detached.
    let second = TelemetryRegistry::install();
    assert!(second.is_installed());

    // A layer of a type already present replaces it instead of stacking.
    let seen_by_new = Arc::new(AtomicUsize::new(0));
    second.add_layer(Counting(Arc::clone(&seen_by_new)));
    tracing::info!("reaches only the replacement");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 1);

    // Excluded crates never reach plugin layers: the crate and its modules.
    second.exclude_targets(&["noisy"]);
    tracing::info!(target: "noisy", "hidden");
    tracing::info!(target: "noisy::transport", "hidden");
    tracing::info!(target: "noisy_util", "a different crate: still seen");
    tracing::info!(target: "quiet", "a different target: still seen");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 3);

    // A detached registry does nothing.
    let detached = TelemetryRegistry::external();
    assert!(!detached.is_installed());
    detached.add_layer(Counting(Arc::new(AtomicUsize::new(0))));
    tracing::info!("still one layer");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 4);

    // The levels can be changed after install, for hosts with no RUST_LOG.
    tracing::debug!("below the default level");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 4);
    second.set_level("debug").unwrap();
    tracing::debug!("now let through");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 5);

    // Directives that don't parse are an error and change nothing.
    assert!(second.set_level("=[").is_err());
    tracing::debug!("still let through");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 6);
}
