//! Compile-time test: `PgmqPlugin<B, _>` can be instantiated against any
//! state HList that contains the matching backend pool. No DB connection
//! is opened — these tests prove the capability-trait machinery wires the
//! right pool to the right `PgmqPlugin` instance at the type level.
//!
//! Each test body is gated on the feature that activates its backend's
//! `Plugin` impl. Run with both features to exercise the full surface:
//!
//! ```text
//! cargo test -p muxa-pgmq --features sqlx,diesel-async
//! ```

#[cfg(feature = "sqlx")]
mod sqlx_tests {
    use muxa_core::{HCons, HNil, Plugin, State};
    use muxa_pgmq::PgmqPlugin;
    use muxa_sqlx::{SqlxBackend, SqlxPool};

    /// SqlxPool at the head (Idx defaults to Here).
    #[test]
    fn pgmq_against_sqlx_at_head() {
        fn check<P, S>()
        where
            S: State,
            P: Plugin<S>,
        {
        }
        check::<PgmqPlugin<SqlxBackend>, HCons<SqlxPool, HNil>>();
    }

    /// SqlxPool buried under another resource — Idx inferred.
    #[test]
    fn pgmq_against_sqlx_deeper() {
        #[derive(Clone)]
        struct OtherResource;

        fn check<P, S, I>()
        where
            S: State,
            P: Plugin<S>,
            I: 'static,
            PgmqPlugin<SqlxBackend, I>: Plugin<S>,
        {
        }
        check::<
            PgmqPlugin<SqlxBackend, muxa_core::There<muxa_core::Here>>,
            HCons<OtherResource, HCons<SqlxPool, HNil>>,
            muxa_core::There<muxa_core::Here>,
        >();
    }
}

#[cfg(feature = "diesel-async")]
mod diesel_tests {
    use muxa_core::{HCons, HNil, Plugin, State};
    use muxa_diesel::{DieselBackend, DieselPool};
    use muxa_pgmq::PgmqPlugin;

    /// **The moneyshot**: the same `PgmqPlugin` shape works against the
    /// Diesel pool with only a backend marker swap.
    #[test]
    fn pgmq_against_diesel_at_head() {
        fn check<P, S>()
        where
            S: State,
            P: Plugin<S>,
        {
        }
        check::<PgmqPlugin<DieselBackend>, HCons<DieselPool, HNil>>();
    }
}
