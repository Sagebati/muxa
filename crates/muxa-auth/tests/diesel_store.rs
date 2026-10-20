//! `DieselSessionStore` against a real Postgres.
//!
//! Needs a database: set `MUXA_AUTH_TEST_DATABASE_URL` (any database works;
//! the test only creates a temporary table). Skipped when it's unset.
//!
//! ```text
//! MUXA_AUTH_TEST_DATABASE_URL=postgres://user:pass@localhost/db \
//!   cargo test -p muxa-auth --features diesel-store --test diesel_store
//! ```

#![cfg(feature = "diesel-store")]
#![allow(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "test helpers fail loudly and say when they skip"
)]

use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, SimpleAsyncConnection as _};
use muxa_auth::DieselSessionStore;
use muxa_auth::store::MIGRATION_SQL;
use muxa_auth::tower_sessions::session::{Id, Record};
use muxa_auth::tower_sessions::session_store::{ExpiredDeletion as _, SessionStore as _};
use muxa_diesel::DieselPool;
use time::{Duration, OffsetDateTime};

/// A one-connection pool whose connection has a temporary `auth_sessions`
/// table, which shadows any real one and vanishes with the connection.
async fn store() -> Option<DieselSessionStore> {
    let Ok(url) = std::env::var("MUXA_AUTH_TEST_DATABASE_URL") else {
        eprintln!("MUXA_AUTH_TEST_DATABASE_URL unset; skipping");
        return None;
    };
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(url);
    let pool = Pool::builder(manager).max_size(1).build().unwrap();
    {
        let mut conn = pool.get().await.unwrap();
        let sql = MIGRATION_SQL
            .replace("CREATE TABLE", "CREATE TEMPORARY TABLE")
            .replace("CREATE INDEX auth_sessions_expiry_date_idx", "CREATE INDEX");
        conn.batch_execute(&sql).await.unwrap();
    }
    Some(DieselSessionStore::new(DieselPool(pool)))
}

fn record(expires_in: Duration) -> Record {
    let mut data = std::collections::HashMap::new();
    data.insert("user".to_owned(), serde_json::json!({ "id": 7 }));
    Record {
        id: Id::default(),
        data,
        expiry_date: OffsetDateTime::now_utc() + expires_in,
    }
}

#[tokio::test]
async fn round_trip_update_delete() {
    let Some(store) = store().await else { return };

    let mut session = record(Duration::hours(1));
    store.create(&mut session).await.unwrap();
    let loaded = store.load(&session.id).await.unwrap().unwrap();
    assert_eq!(loaded.data, session.data);
    // Postgres keeps microseconds.
    assert!((loaded.expiry_date - session.expiry_date).abs() < Duration::milliseconds(1));

    session
        .data
        .insert("theme".to_owned(), serde_json::json!("dark"));
    store.save(&session).await.unwrap();
    let loaded = store.load(&session.id).await.unwrap().unwrap();
    assert_eq!(loaded.data["theme"], "dark");

    store.delete(&session.id).await.unwrap();
    assert!(store.load(&session.id).await.unwrap().is_none());
}

#[tokio::test]
async fn create_redraws_a_taken_id() {
    let Some(store) = store().await else { return };

    let mut first = record(Duration::hours(1));
    store.create(&mut first).await.unwrap();
    let mut second = record(Duration::hours(1));
    second.id = first.id;
    store.create(&mut second).await.unwrap();
    assert_ne!(first.id, second.id);
    assert!(store.load(&first.id).await.unwrap().is_some());
    assert!(store.load(&second.id).await.unwrap().is_some());
}

#[tokio::test]
async fn expired_sessions_are_invisible_and_swept() {
    let Some(store) = store().await else { return };

    let mut stale = record(-Duration::minutes(1));
    store.create(&mut stale).await.unwrap();
    assert!(store.load(&stale.id).await.unwrap().is_none());

    let mut fresh = record(Duration::hours(1));
    store.create(&mut fresh).await.unwrap();
    store.delete_expired().await.unwrap();
    assert!(store.load(&fresh.id).await.unwrap().is_some());
}
