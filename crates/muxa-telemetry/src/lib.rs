//! muxa-telemetry — the shared telemetry kernel.
//!
//! Owns the global `tracing-subscriber`. The base subscriber is a plain
//! `tracing-subscriber::fmt::layer()` scoped by an `EnvFilter`
//! (`RUST_LOG`, default `info`).
//!
//! ## How plugin layers attach
//!
//! The subscriber is installed once, up front (so early logs work), with a
//! single [`reload`] slot holding a **growable stack of type-erased layers**
//! (`Vec<Box<dyn Layer<Registry>>>`), all sitting directly on the root
//! `Registry`. Plugins push their integration layer during their build phase
//! via [`TelemetryRegistry::add_layer`] — e.g. `muxa-sentry` pushes a
//! `sentry_tracing` layer, `muxa-otel` pushes `tracing-opentelemetry` and the
//! OTel log-appender bridge. `add_layer` `modify`s the reload handle, which
//! invalidates callsite caches so events emitted afterwards reach the new layer.
//!
//! A single boxed-`Vec` slot (rather than one typed `reload` slot per backend)
//! is deliberate: multiple `reload` layers can't all sit directly on `Registry`
//! — each would have to be `Layer` of the *previous* layered subscriber, which
//! doesn't compose. One slot holding many boxed layers does, and lets any
//! number of plugins contribute without muxa-telemetry knowing their concrete
//! layer types (so no OTel/sentry deps leak in here).
//!
//! ## When no subscriber is installed
//!
//! [`TelemetryRegistry::external`] (or a failed `try_init` because the app
//! already installed its own subscriber) leaves the slot unallocated; every
//! `add_layer`/`set_*_layer` call becomes a silent no-op.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use tracing_subscriber::layer::{Layer, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Registry, reload};

/// Targets belonging to the telemetry export path itself. An exporting layer
/// (e.g. OTLP over tonic→h2→hyper) that captured these would re-export the
/// spans/events its own exporting produced — an amplifying feedback loop. They
/// are dropped from every plugin layer's filter. They still reach `fmt`
/// (stdout), so export errors stay visible locally.
const EXPORT_INTERNAL_TARGETS: &[&str] = &[
    "h2",
    "hyper",
    "hyper_util",
    "tower",
    "tonic",
    "reqwest",
    "opentelemetry",
    "opentelemetry_sdk",
    "opentelemetry_otlp",
];

/// A fresh `EnvFilter` for one plugin layer: the `RUST_LOG` directives (so the
/// layer honours the same levels as `fmt`) plus the export-internal targets
/// forced `off` to prevent feedback loops.
fn layer_filter() -> EnvFilter {
    let mut filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    for target in EXPORT_INTERNAL_TARGETS {
        filter = filter.add_directive(
            format!("{target}=off")
                .parse()
                .expect("static off-directive is valid"),
        );
    }
    filter
}

/// A type-erased tracing layer that sits directly on the root `Registry`.
type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;
/// Reload handle over the growable stack of plugin layers.
type LayersHandle = reload::Handle<Vec<BoxedLayer>, Registry>;

/// Telemetry registry — handed to plugins via `BuildCtx::telemetry`.
///
/// Holds the reload handle for the shared subscriber's plugin-layer stack.
/// Plugins attach their layers through [`Self::add_layer`].
pub struct TelemetryRegistry {
    installed: bool,
    layers: Option<LayersHandle>,
}

impl TelemetryRegistry {
    /// Install a global subscriber: a reload slot for plugin layers, then a
    /// plain `fmt::layer()` filtered by `EnvFilter` (`RUST_LOG`, default `info`).
    ///
    /// Silent no-op if a subscriber is already installed (the slot is left
    /// unallocated and `add_layer` becomes a no-op).
    pub fn install() -> Self {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

        let fmt_layer = tracing_subscriber::fmt::layer().with_filter(filter);

        // One reload slot: a stack of boxed plugin layers, sitting directly on
        // Registry. fmt is generic and layers freely on top.
        let (layers_slot, layers_handle) = reload::Layer::new(Vec::<BoxedLayer>::new());

        // Filter the whole slot once, *here* at build time — so the `Filtered`
        // layer is registered with the subscriber and gets a `FilterId`.
        // (A per-layer filter on a reload-*injected* layer panics: it never
        // receives a FilterId.) Honours RUST_LOG and drops export-path targets
        // to prevent feedback loops; applies uniformly to every plugin layer.
        let subscriber = tracing_subscriber::registry()
            .with(layers_slot.with_filter(layer_filter()))
            .with(fmt_layer);

        let installed = subscriber.try_init().is_ok();

        Self {
            installed,
            layers: installed.then_some(layers_handle),
        }
    }

    /// Construct a registry that does **not** install a subscriber.
    ///
    /// Use this when the application installs its own `tracing-subscriber`
    /// before constructing the app. All `add_layer`/`set_*_layer` calls become
    /// silent no-ops.
    pub fn external() -> Self {
        Self {
            installed: false,
            layers: None,
        }
    }

    /// Returns `true` if this registry owns the global subscriber and
    /// `add_layer` calls will be effective.
    pub fn is_installed(&self) -> bool {
        self.installed
    }

    /// Attach a tracing [`Layer`] to the shared subscriber at runtime.
    ///
    /// Called by plugins during their build phase (e.g. `muxa-otel` pushes a
    /// `tracing-opentelemetry` layer + the OTel log bridge). The shared
    /// [`layer_filter`] (RUST_LOG + export-path exclusions) wraps the whole slot
    /// at install time, so it applies here too. No-op if no subscriber is
    /// installed.
    pub fn add_layer<L>(&self, layer: L)
    where
        L: Layer<Registry> + Send + Sync + 'static,
    {
        if let Some(handle) = &self.layers {
            let _ = handle.modify(move |layers| layers.push(Box::new(layer)));
        }
    }

    /// Fill the sentry-tracing slot — a convenience wrapper over [`add_layer`].
    ///
    /// Called by `muxa-sentry` when its `tracing-bridge` feature is enabled.
    ///
    /// [`add_layer`]: Self::add_layer
    #[cfg(feature = "sentry")]
    pub fn set_sentry_layer(&self, layer: sentry_tracing::SentryLayer<Registry>) {
        self.add_layer(layer);
    }
}

impl Default for TelemetryRegistry {
    fn default() -> Self {
        Self::install()
    }
}
