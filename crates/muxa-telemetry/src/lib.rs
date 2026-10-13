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
/// Swaps in a new level filter for one of the subscriber's outputs.
type SetLevel = Box<dyn Fn(EnvFilter) -> Result<(), reload::Error> + Send + Sync>;

/// The process-wide plugin-layer slot.
struct Slot {
    layers: LayersHandle,
    /// The type of the layer at each index of the stack — kept in step with
    /// it, both only ever changed under this lock.
    kinds: Mutex<Vec<TypeId>>,
    excluded: Excluded,
    /// One per filtered output: the plugin slot, and `fmt` where it exists.
    levels: Vec<SetLevel>,
}

/// Decided by the first [`TelemetryRegistry::install`]: the slot, or `None`
/// if the application had already installed a subscriber of its own.
static SLOT: OnceLock<Option<Slot>> = OnceLock::new();

/// `RUST_LOG`, defaulting to `info`.
fn level_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// `true` if `target` belongs to one of the `excluded` crates.
fn is_excluded(excluded: &Excluded, target: &str) -> bool {
    excluded
        .read()
        .is_ok_and(|excluded| excluded.iter().any(|krate| in_crate(target, krate)))
}

/// `true` if `target` is `krate` itself or one of its modules: `hyper` covers
/// `hyper` and `hyper::client`, but not `hyper_util` — and `tower` doesn't
/// swallow `tower_http`, whose request spans are what an exporter wants most.
fn in_crate(target: &str, krate: &str) -> bool {
    target
        .strip_prefix(krate)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

/// The filter in front of every plugin layer: the levels (so the layers
/// honour the same levels as `fmt`) minus the excluded targets.
fn plugin_filter(levels: impl Filter<Registry>, excluded: Excluded) -> impl Filter<Registry> {
    levels.and(filter::filter_fn(move |metadata| {
        !is_excluded(&excluded, metadata.target())
    }))
}

/// Install the global subscriber. `None` if one is already installed.
fn install_subscriber() -> Option<Slot> {
    let excluded = Excluded::default();

    // One reload slot: a stack of boxed plugin layers, sitting directly on
    // Registry. fmt is generic and layers freely on top.
    let (layers_slot, layers) = reload::Layer::new(Vec::<BoxedLayer>::new());
    // The levels are reloadable so `set_level` can change them after install
    // — the only way to set them where there is no `RUST_LOG` (wasm).
    let (slot_levels, slot_levels_handle) = reload::Layer::new(level_filter());
    let levels: Vec<SetLevel> = vec![Box::new(move |filter| slot_levels_handle.reload(filter))];

    // Filter the whole slot once, *here* at build time — so the `Filtered`
    // layer is registered with the subscriber and gets a `FilterId`.
    // (A per-layer filter on a reload-*injected* layer panics: it never
    // receives a FilterId.)
    let subscriber = tracing_subscriber::registry()
        .with(layers_slot.with_filter(plugin_filter(slot_levels, Arc::clone(&excluded))));

    // wasm has no stdout and no clock (`fmt`'s timestamp panics on
    // wasm32-unknown-unknown), so there the subscriber is the plugin slot
    // alone; the host plugin (e.g. muxa-worker's console layer) adds the
    // output layer through `add_layer`.
    #[cfg(not(target_family = "wasm"))]
    let (subscriber, levels) = {
        let (fmt_levels, fmt_levels_handle) = reload::Layer::new(level_filter());
        let mut levels = levels;
        levels.push(Box::new(move |filter| fmt_levels_handle.reload(filter)));
        let subscriber = subscriber.with(tracing_subscriber::fmt::layer().with_filter(fmt_levels));
        (subscriber, levels)
    };

    subscriber.try_init().ok()?;

    Some(Slot {
        layers,
        kinds: Mutex::new(Vec::new()),
        excluded,
        levels,
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
    /// On wasm targets the `fmt` layer is left out (no stdout, no clock).
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

    /// Hide events and spans of these crates from every plugin layer. They
    /// still reach the `fmt` output — except on wasm, which has no `fmt`
    /// layer, so there they are hidden everywhere. A target matches a crate when it is the
    /// crate or one of its modules: `"hyper"` covers `hyper::client`, but not
    /// `hyper_util` — name each crate on the path.
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
            for &target in targets {
                if !excluded.iter().any(|known| known == target) {
                    excluded.push(target.to_owned());
                }
            }
        }
        // Callsites cache whether the plugin layers want them; the answer just
        // changed, so make them ask again. (After the write guard is dropped —
        // rebuilding calls the filter, which takes the read lock.)
        tracing::callsite::rebuild_interest_cache();
    }

    /// Replace the levels, for the plugin layers and `fmt` alike, with
    /// `RUST_LOG`-style `directives` (e.g. `"info,my_app=debug"`).
    ///
    /// At install the levels come from `RUST_LOG` (default `info`). Use this
    /// where there is no process environment to read it from — a Worker
    /// reads its own `RUST_LOG` var and passes it here.
    ///
    /// No-op when no subscriber is installed.
    ///
    /// # Errors
    ///
    /// When `directives` don't parse; the levels are then left unchanged.
    pub fn set_level(&self, directives: &str) -> Result<(), filter::ParseError> {
        // Parse once up front so a bad string changes nothing.
        EnvFilter::try_new(directives)?;
        let Some(slot) = self.slot else { return Ok(()) };
        for set in &slot.levels {
            // `EnvFilter` isn't `Clone`: parse a fresh one per output. Reload
            // only fails if the subscriber is gone, and then there is nothing
            // left to filter.
            if let Ok(filter) = EnvFilter::try_new(directives) {
                let _ = set(filter);
            }
        }
        Ok(())
    }
}

impl Default for TelemetryRegistry {
    fn default() -> Self {
        Self::install()
    }
}
