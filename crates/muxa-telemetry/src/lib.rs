//! muxa-telemetry — the shared telemetry kernel.
//!
//! Owns the global `tracing-subscriber`. The base subscriber is a plain
//! `tracing-subscriber::fmt::layer()` scoped by an `EnvFilter`
//! (`RUST_LOG`, default `info`).
//!
//! ## How plugin layers attach
//!
//! The subscriber is installed once per process, up front (so early logs
//! work), with a single [`reload`] slot holding a **growable stack of
//! type-erased layers** (`Vec<Box<dyn Layer<Registry>>>`), all sitting
//! directly on the root `Registry`. Plugins push their integration layer
//! during their build phase via [`TelemetryRegistry::add_layer`]. `add_layer`
//! `modify`s the reload handle, which invalidates callsite caches so events
//! emitted afterwards reach the new layer.
//!
//! A single boxed-`Vec` slot (rather than one typed `reload` slot per backend)
//! is deliberate: multiple `reload` layers can't all sit directly on `Registry`
//! — each would have to be `Layer` of the *previous* layered subscriber, which
//! doesn't compose. One slot holding many boxed layers does, and lets any
//! number of plugins contribute without this crate knowing their concrete
//! layer types — it has no dependency on, and no knowledge of, any of them.
//!
//! ## One slot per process
//!
//! A process has one global subscriber, so it has one slot. Every
//! [`TelemetryRegistry::install`] after the first returns a handle to that
//! same slot: building a second app in the same process (tests, a rebuild
//! after a failed start) keeps working. The slot holds **one layer per
//! type** — adding a layer whose type is already present replaces the old one
//! instead of stacking a duplicate.
//!
//! ## Keeping an exporter out of its own way
//!
//! A layer that ships events over the network would, left alone, also capture
//! the events its own transport emits and export those too — an amplifying
//! loop. The plugin that owns such a layer names the crates on its export path
//! with [`TelemetryRegistry::exclude_targets`]; those targets are then hidden
//! from every plugin layer. They still reach `fmt`, so export errors stay
//! visible locally.
//!
//! ## When no subscriber is installed
//!
//! [`TelemetryRegistry::external`] (or a failed `try_init` because the app
//! already installed its own subscriber) leaves the registry detached; every
//! `add_layer`/`exclude_targets` call becomes a silent no-op.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use core::any::TypeId;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use tracing_subscriber::filter::{self, FilterExt as _};
use tracing_subscriber::layer::{Filter, Layer, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Registry, reload};

/// A type-erased tracing layer that sits directly on the root `Registry`.
type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;
/// Reload handle over the growable stack of plugin layers.
type LayersHandle = reload::Handle<Vec<BoxedLayer>, Registry>;
/// Targets hidden from plugin layers; shared with the slot's filter.
type Excluded = Arc<RwLock<Vec<String>>>;

/// The process-wide plugin-layer slot.
struct Slot {
    layers: LayersHandle,
    /// The type of the layer at each index of the stack — kept in step with
    /// it, both only ever changed under this lock.
    kinds: Mutex<Vec<TypeId>>,
    excluded: Excluded,
}

/// Decided by the first [`TelemetryRegistry::install`]: the slot, or `None`
/// if the application had already installed a subscriber of its own.
static SLOT: OnceLock<Option<Slot>> = OnceLock::new();

/// `RUST_LOG`, defaulting to `info`.
fn level_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// `true` if `target` starts with one of the `excluded` prefixes — the same
/// matching as a `RUST_LOG` target directive (`hyper` also covers
/// `hyper::client` and `hyper_util`).
fn is_excluded(excluded: &Excluded, target: &str) -> bool {
    excluded.read().is_ok_and(|excluded| {
        excluded
            .iter()
            .any(|prefix| target.starts_with(prefix.as_str()))
    })
}

/// The filter in front of every plugin layer: the `RUST_LOG` levels (so the
/// layers honour the same levels as `fmt`) minus the excluded targets.
fn plugin_filter(excluded: Excluded) -> impl Filter<Registry> {
    level_filter().and(filter::filter_fn(move |metadata| {
        !is_excluded(&excluded, metadata.target())
    }))
}

/// Install the global subscriber. `None` if one is already installed.
fn install_subscriber() -> Option<Slot> {
    let excluded = Excluded::default();

    // One reload slot: a stack of boxed plugin layers, sitting directly on
    // Registry. fmt is generic and layers freely on top.
    let (layers_slot, layers) = reload::Layer::new(Vec::<BoxedLayer>::new());

    // Filter the whole slot once, *here* at build time — so the `Filtered`
    // layer is registered with the subscriber and gets a `FilterId`.
    // (A per-layer filter on a reload-*injected* layer panics: it never
    // receives a FilterId.)
    let subscriber = tracing_subscriber::registry()
        .with(layers_slot.with_filter(plugin_filter(Arc::clone(&excluded))))
        .with(tracing_subscriber::fmt::layer().with_filter(level_filter()));

    subscriber.try_init().ok()?;

    Some(Slot {
        layers,
        kinds: Mutex::new(Vec::new()),
        excluded,
    })
}

/// Telemetry registry — handed to plugins via `BuildCtx::telemetry`.
///
/// A handle to the process-wide plugin-layer slot of the shared subscriber.
/// Plugins attach their layers through [`Self::add_layer`].
#[derive(Clone, Copy)]
pub struct TelemetryRegistry {
    slot: Option<&'static Slot>,
}

impl TelemetryRegistry {
    /// Get the process's plugin-layer slot, installing the global subscriber
    /// on the first call: a reload slot for plugin layers, then a plain
    /// `fmt::layer()` filtered by `EnvFilter` (`RUST_LOG`, default `info`).
    ///
    /// If the application installed its own subscriber first, the registry is
    /// detached and `add_layer` becomes a silent no-op.
    pub fn install() -> Self {
        Self {
            slot: SLOT.get_or_init(install_subscriber).as_ref(),
        }
    }

    /// Construct a registry that does **not** install a subscriber.
    ///
    /// Use this when the application installs its own `tracing-subscriber`
    /// before constructing the app. All `add_layer`/`exclude_targets` calls
    /// become silent no-ops.
    pub fn external() -> Self {
        Self { slot: None }
    }

    /// `true` if this registry is attached to the shared subscriber's slot.
    pub fn is_installed(&self) -> bool {
        self.slot.is_some()
    }

    /// Attach a tracing layer to the shared subscriber.
    ///
    /// The layer sits directly on the root `Registry`, behind the slot's
    /// filter (`RUST_LOG` levels, minus [excluded targets](Self::exclude_targets)).
    ///
    /// The slot holds one layer per type: if a layer of type `L` is already
    /// attached, it is replaced. That keeps a second app built in the same
    /// process from stacking a duplicate of every layer.
    ///
    /// No-op when no subscriber is installed.
    pub fn add_layer<L>(&self, layer: L)
    where
        L: Layer<Registry> + Send + Sync + 'static,
    {
        let Some(slot) = self.slot else { return };
        let Ok(mut kinds) = slot.kinds.lock() else {
            return;
        };

        let kind = TypeId::of::<L>();
        let layer: BoxedLayer = Box::new(layer);
        match kinds.iter().position(|known| *known == kind) {
            Some(index) => {
                let _ = slot.layers.modify(move |layers| {
                    if let Some(current) = layers.get_mut(index) {
                        *current = layer;
                    }
                });
            }
            None => {
                if slot.layers.modify(move |layers| layers.push(layer)).is_ok() {
                    kinds.push(kind);
                }
            }
        }
    }

    /// Hide events and spans whose target starts with one of these prefixes
    /// from every plugin layer. They still reach the `fmt` output. Matching is
    /// by prefix, like a `RUST_LOG` target directive: `"hyper"` also covers
    /// `hyper::client` and `hyper_util`.
    ///
    /// For a plugin whose layer exports over the network: name the crates on
    /// its export path (its HTTP/gRPC client, the exporter SDK), so the layer
    /// never re-exports what its own exporting emits.
    ///
    /// No-op when no subscriber is installed.
    pub fn exclude_targets(&self, targets: &[&str]) {
        let Some(slot) = self.slot else { return };
        {
            let Ok(mut excluded) = slot.excluded.write() else {
                return;
            };
            for target in targets {
                if !excluded.iter().any(|known| known == target) {
                    excluded.push((*target).to_owned());
                }
            }
        }
        // Callsites cache whether the plugin layers want them; the answer just
        // changed, so make them ask again. (After the write guard is dropped —
        // rebuilding calls the filter, which takes the read lock.)
        tracing::callsite::rebuild_interest_cache();
    }
}

impl Default for TelemetryRegistry {
    fn default() -> Self {
        Self::install()
    }
}
