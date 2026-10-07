//! Capability traits — the cross-plugin interface layer.
//!
//! A capability says "the application state contains resource X", so a plugin
//! can require a resource built by an earlier plugin without naming that
//! plugin's crate.
//!
//! The one capability today is a Postgres pool. Pool crates (`muxa-sqlx`,
//! `muxa-diesel`, …) each define a [`PgBackend`] marker (one type per crate)
//! naming their pool type. A *single* blanket implementation of
//! [`HasPgExecutorFor`] lives here in `muxa-core` and covers every backend, so
//! adding a new pool only requires the backend marker — no per-crate
//! orphan-rule gymnastics. Nothing here knows who consumes the pool.
//!
//! Consumer plugins (e.g. `muxa-pgmq`) carry both the backend `B` and an
//! `Idx` phantom; the user writes `PgmqPlugin::<SqlxBackend, _>::…` and
//! the compiler infers `Idx`. When the consumer is placed immediately
//! after the pool plugin in the chain, `Idx` defaults to [`Here`] and the
//! `_` may be omitted.

use dupe::Dupe;

use crate::state::{Here, Selector};

/// Per-backend type marker for a Postgres pool (one per pool crate).
///
/// A pool plugin like `muxa-sqlx` defines `struct SqlxBackend;` and impls
/// `PgBackend for SqlxBackend { type Pool = SqlxPool; }`. A consumer plugin is
/// generic over `B: PgBackend`, which says which pool it wants.
pub trait PgBackend: Send + Sync + 'static {
    /// The concrete pool type this backend exposes.
    ///
    /// `Dupe` is Meta's marker for cheap clones (see the
    /// [`dupe`](https://docs.rs/dupe) crate): consumers clone the pool freely,
    /// it's an `Arc` bump. What can be *done* with the pool is defined by the
    /// consumer's own driver traits, which each pool type implements.
    type Pool: Dupe + Send + Sync + 'static;
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
pub trait HasPgExecutorFor<B: PgBackend, Idx = Here> {
    /// Borrow the pool for backend `B`.
    fn pg_executor(&self) -> &B::Pool;
}

/// Blanket: any state HList containing `B::Pool` at any position
/// satisfies `HasPgExecutorFor<B, Idx>` for the matching `Idx` phantom.
impl<S, B, Idx> HasPgExecutorFor<B, Idx> for S
where
    B: PgBackend,
    S: Selector<B::Pool, Idx>,
{
    fn pg_executor(&self) -> &B::Pool {
        self.select()
    }
}
