//! Compile-time test: the `HasPgExecutorFor<SqlxBackend>` capability is
//! satisfied for an HList containing `SqlxPool`, with the index inferred.
//!
//! These tests don't connect to a database — they only exercise the type
//! system. If they compile, the capability plumbing works.

use muxa_core::{HCons, HNil, HasPgExecutorFor, State as _};
use muxa_sqlx::{SqlxBackend, SqlxPool};

/// A dummy other resource that lives between the pool and the head.
#[derive(Clone)]
struct OtherResource;

#[test]
fn pool_at_head_via_default_idx() {
    // Build a state HList with SqlxPool at the head.
    // Requires a real PgPool to construct SqlxPool — but we never call
    // anything on it, so just lazy-test the type system without running.
    let _phantom = std::marker::PhantomData::<fn()>;
    // We use a generic helper that *would* call pg_executor; the test is
    // that this compiles.
    fn needs_pool<S: HasPgExecutorFor<SqlxBackend>>(_s: &S) {}

    // Construct a fake type satisfying Selector via direct HCons.
    fn check<P: Send + Sync + 'static>(pool: P) -> HCons<P, HNil> {
        HNil.push(pool)
    }
    // The following line type-checks iff SqlxPool at head satisfies
    // HasPgExecutorFor<SqlxBackend> with default Idx = Here.
    let _: fn(&HCons<SqlxPool, HNil>) = needs_pool::<HCons<SqlxPool, HNil>>;
    let _ = check::<u8>(0); // silence unused
}

#[test]
fn pool_deeper_via_inferred_idx() {
    fn needs_pool<S, I>(_s: &S)
    where
        S: HasPgExecutorFor<SqlxBackend, I>,
    {
    }
    // SqlxPool is at depth 1 here (under OtherResource).
    let _: fn(&HCons<OtherResource, HCons<SqlxPool, HNil>>) =
        needs_pool::<HCons<OtherResource, HCons<SqlxPool, HNil>>, _>;
}
