//! muxa-telemetry — the shared telemetry kernel.
//!
//! Owns the global `tracing-subscriber`. The base subscriber is a plain
//! `tracing-subscriber::fmt::layer()` scoped by an `EnvFilter`
//! (`RUST_LOG`, default `info`). On wasm, where there is no stdout, events go
//! to the JS console instead (`console.log` and friends, by level), so logs
//! work on Cloudflare Workers and other JS hosts with no setup.
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

#[cfg(any(test, target_family = "wasm"))]
mod console;

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
#[cfg(target_family = "wasm")]
type SetLevel = Box<dyn Fn(EnvFilter) -> Result<(), reload::Error> + Send + Sync>;

/// The process-wide plugin-layer slot.
struct Slot {
    layers: LayersHandle,
    /// The type of the layer at each index of the stack — kept in step with
    /// it, both only ever changed under this lock.
    kinds: Mutex<Vec<TypeId>>,
    excluded: Excluded,
    /// One per filtered output: the plugin slot and the JS console.
    #[cfg(target_family = "wasm")]
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

    // Natively the levels are fixed by `RUST_LOG`: plain filters, no lock on
    // the hot path. On wasm there is no `RUST_LOG` to read, so the levels are
    // reloadable for `set_level`.
    #[cfg(not(target_family = "wasm"))]
    let (slot_levels, output_levels) = (level_filter(), level_filter());
    #[cfg(target_family = "wasm")]
    let (slot_levels, output_levels, levels) = {
        let (slot_levels, slot_handle) = reload::Layer::new(level_filter());
        let (output_levels, output_handle) = reload::Layer::new(level_filter());
        let levels: Vec<SetLevel> = vec![
            Box::new(move |filter| slot_handle.reload(filter)),
            Box::new(move |filter| output_handle.reload(filter)),
        ];
        (slot_levels, output_levels, levels)
    };

    // Filter the whole slot once, *here* at build time — so the `Filtered`
    // layer is registered with the subscriber and gets a `FilterId`.
    // (A per-layer filter on a reload-*injected* layer panics: it never
    // receives a FilterId.)
    let subscriber = tracing_subscriber::registry()
        .with(layers_slot.with_filter(plugin_filter(slot_levels, Arc::clone(&excluded))));

    // The output: `fmt` to stdout natively. wasm has no stdout and no clock
    // (`fmt`'s timestamp panics on wasm32-unknown-unknown), so there events
    // go to the JS console instead.
    #[cfg(not(target_family = "wasm"))]
    let output = tracing_subscriber::fmt::layer();
    #[cfg(target_family = "wasm")]
    let output = console::ConsoleLayer::new(console::js_console);

    subscriber
        .with(output.with_filter(output_levels))
        .try_init()
        .ok()?;

    Some(Slot {
        layers,
        kinds: Mutex::new(Vec::new()),
        excluded,
        #[cfg(target_family = "wasm")]
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
    /// On wasm targets the JS console takes the place of `fmt`.
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
    /// still reach the `fmt` output (the JS console on wasm). A target matches a crate when it is the
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

    /// Replace the levels, for the plugin layers and the console alike, with
    /// `RUST_LOG`-style `directives` (e.g. `"info,my_app=debug"`).
    ///
    /// wasm only. Natively the levels come from `RUST_LOG` and are fixed, so
    /// filtering takes no lock. On wasm there is no process environment to
    /// read `RUST_LOG` from — a Worker reads its own `RUST_LOG` var and passes
    /// it here.
    ///
    /// # Errors
    ///
    /// [`SetLevelError::Invalid`] when `directives` don't parse;
    /// [`SetLevelError::Detached`] when this registry isn't attached to the
    /// subscriber (the application installed its own first). Either way the
    /// levels are left unchanged.
    #[cfg(target_family = "wasm")]
    pub fn set_level(&self, directives: &str) -> Result<(), SetLevelError> {
        let parsed = levels::parse(directives).map_err(SetLevelError::Invalid)?;
        let slot = self.slot.ok_or(SetLevelError::Detached)?;
        for set in &slot.levels {
            // Reload only fails if the subscriber is gone, and then there is
            // nothing left to filter.
            let _ = set(levels::filter(&parsed));
        }
        Ok(())
    }
}

/// Why [`TelemetryRegistry::set_level`] left the levels unchanged.
#[cfg(target_family = "wasm")]
#[derive(Debug)]
pub enum SetLevelError {
    /// The directives don't parse.
    Invalid(filter::ParseError),
    /// The registry isn't attached to the subscriber: the application
    /// installed its own, whose levels muxa doesn't control.
    Detached,
}

#[cfg(target_family = "wasm")]
impl core::fmt::Display for SetLevelError {
    fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Invalid(err) => write!(fmt, "invalid log level directives: {err}"),
            Self::Detached => fmt.write_str(
                "the application installed its own tracing subscriber; set its levels there",
            ),
        }
    }
}

#[cfg(target_family = "wasm")]
impl std::error::Error for SetLevelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(err) => Some(err),
            Self::Detached => None,
        }
    }
}

/// Parse `RUST_LOG`-style directives once, then build as many filters from
/// them as there are outputs (`EnvFilter` isn't `Clone`, `Directive` is).
#[cfg(any(test, target_family = "wasm"))]
mod levels {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::{Directive, ParseError};

    /// Split and parse like `EnvFilter`'s own parser: on `,`, skipping
    /// empty entries.
    pub fn parse(directives: &str) -> Result<Vec<Directive>, ParseError> {
        directives
            .split(',')
            .filter(|directive| !directive.is_empty())
            .map(str::parse)
            .collect()
    }

    pub fn filter(directives: &[Directive]) -> EnvFilter {
        directives
            .iter()
            .cloned()
            .fold(EnvFilter::default(), EnvFilter::add_directive)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parsing_once_builds_the_same_filter_as_envfilter() {
            for directives in [
                "",
                "debug",
                "info,my_app=debug",
                "warn,,hyper=off,a::b[span]=trace",
            ] {
                let ours = filter(&parse(directives).unwrap()).to_string();
                let theirs = EnvFilter::try_new(directives).unwrap().to_string();
                assert_eq!(ours, theirs, "{directives:?}");
            }
            assert!(parse("=[").is_err());
        }
    }
}

impl Default for TelemetryRegistry {
    fn default() -> Self {
        Self::install()
    }
}
