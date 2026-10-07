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

    // Excluded targets never reach plugin layers. Matching is by prefix.
    second.exclude_targets(&["noisy"]);
    tracing::info!(target: "noisy", "hidden");
    tracing::info!(target: "noisy::transport", "hidden");
    tracing::info!(target: "noisy_util", "hidden: same prefix");
    tracing::info!(target: "quiet", "a different target: still seen");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 2);

    // A detached registry does nothing.
    let detached = TelemetryRegistry::external();
    assert!(!detached.is_installed());
    detached.add_layer(Counting(Arc::new(AtomicUsize::new(0))));
    tracing::info!("still one layer");
    assert_eq!(seen_by_new.load(Ordering::SeqCst), 3);
}
