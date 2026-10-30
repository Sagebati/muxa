//! `MigrationsRunner` against a real Postgres: concurrent runs are serialized.
//!
//! Needs a database: set `MUXA_DIESEL_TEST_DATABASE_URL` to a role allowed to
//! create databases. Each test creates its own throwaway database and drops it
//! afterwards. Skipped when it's unset.
//!
//! ```text
//! MUXA_DIESEL_TEST_DATABASE_URL=postgres://user:pass@localhost/postgres \
//!   cargo test -p muxa-diesel --features migrations --test migrations
//! ```

#![cfg(feature = "migrations")]
#![allow(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "test helpers fail loudly and say when they skip"
)]

use std::time::{SystemTime, UNIX_EPOCH};

use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
use muxa_diesel::{EmbeddedMigrations, MigrationsRunner, embed_migrations};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("tests/migrations");
const MIGRATION_COUNT: usize = 2;

/// A freshly created, empty database; dropped again by [`TempDb::drop_db`].
struct TempDb {
    admin_url: String,
    name: String,
    url: String,
}

impl TempDb {
    async fn create() -> Option<Self> {
        let Ok(admin_url) = std::env::var("MUXA_DIESEL_TEST_DATABASE_URL") else {
            eprintln!("MUXA_DIESEL_TEST_DATABASE_URL unset; skipping");
            return None;
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("muxa_diesel_test_{}_{nanos}", std::process::id());
        let mut conn = AsyncPgConnection::establish(&admin_url).await.unwrap();
        conn.batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        let url = with_database(&admin_url, &name);
        Some(Self {
            admin_url,
            name,
            url,
        })
    }

    async fn drop_db(self) {
        let mut conn = AsyncPgConnection::establish(&self.admin_url).await.unwrap();
        conn.batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", self.name))
            .await
            .unwrap();
    }
}

/// `url` with its database name replaced by `name`.
fn with_database(url: &str, name: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(base, query)| (base, Some(query)));
    let (scheme, rest) = base.split_once("://").unwrap();
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _)| authority);
    match query {
        Some(query) => format!("{scheme}://{authority}/{name}?{query}"),
        None => format!("{scheme}://{authority}/{name}"),
    }
}

#[test]
fn with_database_swaps_the_path() {
    assert_eq!(
        with_database("postgres://u:p@h:5432/postgres", "x"),
        "postgres://u:p@h:5432/x"
    );
    assert_eq!(
        with_database("postgres://u:p@h/postgres?sslmode=disable", "x"),
        "postgres://u:p@h/x?sslmode=disable"
    );
    assert_eq!(with_database("postgres://h", "x"), "postgres://h/x");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_runs_both_succeed() {
    let Some(db) = TempDb::create().await else {
        return;
    };

    let (first, second) = tokio::join!(
        MigrationsRunner::new(MIGRATIONS).run(&db.url),
        MigrationsRunner::new(MIGRATIONS).run(&db.url),
    );
    let mut applied = [first.unwrap(), second.unwrap()];
    applied.sort_unstable();
    assert_eq!(applied, [0, MIGRATION_COUNT]);

    db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn many_concurrent_runs_apply_once() {
    let Some(db) = TempDb::create().await else {
        return;
    };

    let runs = (0..8).map(|_| {
        let url = db.url.clone();
        tokio::spawn(async move { MigrationsRunner::new(MIGRATIONS).run(&url).await })
    });
    let mut applied = Vec::new();
    for run in runs.collect::<Vec<_>>() {
        applied.push(run.await.unwrap().unwrap());
    }
    applied.sort_unstable();
    assert_eq!(applied, [0, 0, 0, 0, 0, 0, 0, MIGRATION_COUNT]);

    // Up to date: a later run has nothing to do.
    assert_eq!(
        MigrationsRunner::new(MIGRATIONS)
            .run(&db.url)
            .await
            .unwrap(),
        0
    );

    db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn distinct_lock_keys_do_not_wait_on_each_other() {
    let Some(db) = TempDb::create().await else {
        return;
    };

    // Holding the default key in another session must not block a runner that
    // uses its own key.
    let mut holder = AsyncPgConnection::establish(&db.url).await.unwrap();
    holder
        .batch_execute(&format!(
            "SELECT pg_advisory_lock({})",
            MigrationsRunner::DEFAULT_LOCK_KEY
        ))
        .await
        .unwrap();

    let applied = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        MigrationsRunner::new(MIGRATIONS)
            .with_lock_key(42)
            .run(&db.url),
    )
    .await
    .expect("a runner with its own key waited on the default key")
    .unwrap();
    assert_eq!(applied, MIGRATION_COUNT);

    drop(holder);
    db.drop_db().await;
}
