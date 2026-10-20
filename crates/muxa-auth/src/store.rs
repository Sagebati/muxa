//! A tower-sessions [`SessionStore`] over muxa-diesel's async Postgres pool.
//!
//! The app owns the table; create it with [`MIGRATION_SQL`] in one of its
//! migrations.

use std::fmt;

use async_trait::async_trait;
use axum_login::tower_sessions::session::{Id, Record};
use axum_login::tower_sessions::session_store::{self, ExpiredDeletion, SessionStore};
use diesel::upsert::excluded;
use diesel::{ExpressionMethods as _, OptionalExtension as _, QueryDsl as _};
use diesel_async::RunQueryDsl as _;
use muxa_diesel::DieselPool;
use serde_json::Value;
use time::OffsetDateTime;

diesel::table! {
    /// Session records, one row per session.
    auth_sessions (id) {
        /// The session id, as tower-sessions prints it.
        id -> Text,
        /// The session's key/value data.
        data -> Jsonb,
        /// When the session expires.
        expiry_date -> Timestamptz,
    }
}

/// The table [`DieselSessionStore`] reads and writes.
pub const MIGRATION_SQL: &str = "\
CREATE TABLE auth_sessions (
    id          text PRIMARY KEY,
    data        jsonb NOT NULL,
    expiry_date timestamptz NOT NULL
);
CREATE INDEX auth_sessions_expiry_date_idx ON auth_sessions (expiry_date);
";

/// Sessions in Postgres, through the app's diesel-async pool.
#[derive(Clone)]
pub struct DieselSessionStore {
    pool: DieselPool,
}

impl DieselSessionStore {
    /// A store over `pool`.
    #[must_use]
    pub fn new(pool: DieselPool) -> Self {
        Self { pool }
    }

    async fn conn(
        &self,
    ) -> session_store::Result<
        diesel_async::pooled_connection::deadpool::Object<diesel_async::AsyncPgConnection>,
    > {
        self.pool.get().await.map_err(backend)
    }
}

impl fmt::Debug for DieselSessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DieselSessionStore").finish_non_exhaustive()
    }
}

fn backend(err: impl fmt::Display) -> session_store::Error {
    session_store::Error::Backend(err.to_string())
}

fn encode(record: &Record) -> session_store::Result<Value> {
    serde_json::to_value(&record.data).map_err(|err| session_store::Error::Encode(err.to_string()))
}

#[async_trait]
impl SessionStore for DieselSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        use crate::store::auth_sessions::dsl::{auth_sessions, data, expiry_date, id};

        let value = encode(record)?;
        let mut conn = self.conn().await?;
        // Ids are random 128-bit values; on the off chance one is taken, draw again.
        loop {
            let inserted = diesel::insert_into(auth_sessions)
                .values((
                    id.eq(record.id.to_string()),
                    data.eq(&value),
                    expiry_date.eq(record.expiry_date),
                ))
                .on_conflict_do_nothing()
                .execute(&mut conn)
                .await
                .map_err(backend)?;
            if inserted == 1 {
                return Ok(());
            }
            record.id = Id::default();
        }
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        use crate::store::auth_sessions::dsl::{auth_sessions, data, expiry_date, id};

        let value = encode(record)?;
        let mut conn = self.conn().await?;
        diesel::insert_into(auth_sessions)
            .values((
                id.eq(record.id.to_string()),
                data.eq(&value),
                expiry_date.eq(record.expiry_date),
            ))
            .on_conflict(id)
            .do_update()
            .set((
                data.eq(excluded(data)),
                expiry_date.eq(excluded(expiry_date)),
            ))
            .execute(&mut conn)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        use crate::store::auth_sessions::dsl::{auth_sessions, data, expiry_date, id};

        let mut conn = self.conn().await?;
        let row: Option<(Value, OffsetDateTime)> = auth_sessions
            .filter(id.eq(session_id.to_string()))
            .filter(expiry_date.gt(OffsetDateTime::now_utc()))
            .select((data, expiry_date))
            .first(&mut conn)
            .await
            .optional()
            .map_err(backend)?;
        row.map(|(value, expires)| {
            Ok(Record {
                id: *session_id,
                data: serde_json::from_value(value)
                    .map_err(|err| session_store::Error::Decode(err.to_string()))?,
                expiry_date: expires,
            })
        })
        .transpose()
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        use crate::store::auth_sessions::dsl::{auth_sessions, id};

        let mut conn = self.conn().await?;
        diesel::delete(auth_sessions.filter(id.eq(session_id.to_string())))
            .execute(&mut conn)
            .await
            .map_err(backend)?;
        Ok(())
    }
}

#[async_trait]
impl ExpiredDeletion for DieselSessionStore {
    async fn delete_expired(&self) -> session_store::Result<()> {
        use crate::store::auth_sessions::dsl::{auth_sessions, expiry_date};

        let mut conn = self.conn().await?;
        diesel::delete(auth_sessions.filter(expiry_date.lt(OffsetDateTime::now_utc())))
            .execute(&mut conn)
            .await
            .map_err(backend)?;
        Ok(())
    }
}
