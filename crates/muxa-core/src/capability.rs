//! Capability traits — the cross-plugin interface layer.
//!
//! Pool crates (`muxa-sqlx`, `muxa-diesel`, …) define a per-crate
//! [`PgmqBackend`] marker (one type per crate). A *single* blanket
//! implementation of [`HasPgExecutorFor`] lives here in `muxa-core` and
//! covers every backend, so adding a new pool only requires the backend
//! marker — no per-crate orphan-rule gymnastics.
//!
//! Consumer plugins (e.g. `muxa-pgmq`) carry both the backend `B` and an
//! `Idx` phantom; the user writes `PgmqPlugin::<SqlxBackend, _>::…` and
//! the compiler infers `Idx`. When the consumer is placed immediately
//! after the pool plugin in the chain, `Idx` defaults to [`Here`] and the
//! `_` may be omitted.

use dupe::Dupe;

use crate::state::{Here, Selector};

/// Marker trait for any database pool usable as a pgmq backend.
///
/// The real per-call surface (queue operations) is defined by the pgmq crate's
/// executor trait, which each pool type implements separately. This trait is
/// the minimum cross-pool guarantee — `Dupe + Send + Sync + 'static` — so
/// plugins can clone the pool freely (it's an Arc bump). `Dupe` is Meta's
/// marker for cheap clones; see the [`dupe`](https://docs.rs/dupe) crate.
pub trait PgmqPool: Dupe + Send + Sync + 'static {}

/// Per-backend type marker (one per pool crate).
///
/// A pool plugin like `muxa-sqlx` defines `struct SqlxBackend;` and impls
/// `PgmqBackend for SqlxBackend { type Pool = SqlxPool; }`. The `B` parameter
/// flows through `PgmqPlugin::<B, _>` and disambiguates the blanket impl.
pub trait PgmqBackend: Send + Sync + 'static {
    /// The concrete pool type this backend exposes.
    type Pool: PgmqPool;
}

/// Capability: "the state HList contains the pool for backend `B` at
/// some position `Idx`".
///
/// The `Idx` phantom is required to satisfy Rust's
/// [E0207](https://doc.rust-lang.org/error_codes/E0207.html) (unconstrained
/// type parameter) rule when blanket-impl'ing over an HList. Consumers
/// usually let it be inferred via `<_>`.
#[diagnostic::on_unimplemented(
    message = "no Postgres pool available for backend `{B}` in the app state",
    label = "add the matching pool plugin (e.g. SqlxPlugin or DieselPlugin) before this plugin in the App::default()...with_plugin() chain"
)]
pub trait HasPgExecutorFor<B: PgmqBackend, Idx = Here> {
    /// Borrow the pool for backend `B`.
    fn pg_executor(&self) -> &B::Pool;
}

/// Blanket: any state HList containing `B::Pool` at any position
/// satisfies `HasPgExecutorFor<B, Idx>` for the matching `Idx` phantom.
impl<S, B, Idx> HasPgExecutorFor<B, Idx> for S
where
    B: PgmqBackend,
    S: Selector<B::Pool, Idx>,
{
    fn pg_executor(&self) -> &B::Pool {
        self.select()
    }
}
