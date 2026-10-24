use crate::helpers::{MAX_HTML_BYTES, hash, random_hex, valid_hash, valid_id, version_for};
use crate::storage::{Saved, Storage, StoreError, Version};
use async_trait::async_trait;
use chrono::DateTime;
use sqlx::{PgPool, SqlitePool};

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(error) = &error {
            if error.is_unique_violation() {
                return Self::Conflict;
            }
            return Self::Database(
                error
                    .code()
                    .map(|code| code.into_owned())
                    .unwrap_or_else(|| "unknown".into()),
            );
        }
        Self::Database(
            match error {
                sqlx::Error::PoolTimedOut => "pool timeout",
                sqlx::Error::PoolClosed => "pool closed",
                sqlx::Error::Io(_) => "connection I/O",
                sqlx::Error::Tls(_) => "TLS connection",
                _ => "driver error",
            }
            .into(),
        )
    }
}

fn recorded_version(hash: String, timestamp: i64, size: i64) -> Result<Version, StoreError> {
    if !valid_hash(&hash)
        || hash.bytes().any(|b| b.is_ascii_uppercase())
        || !(1..=MAX_HTML_BYTES as i64).contains(&size)
    {
        return Err(StoreError::InvalidMetadata);
    }
    Ok(Version {
        content_hash: hash,
        created_at: DateTime::from_timestamp_micros(timestamp)
            .ok_or(StoreError::InvalidMetadata)?,
        size_bytes: size as usize,
    })
}

fn check_input(html: &[u8]) -> Result<(), StoreError> {
    if html.is_empty()
        || html.len() > MAX_HTML_BYTES
        || html.contains(&0)
        || std::str::from_utf8(html).is_err()
    {
        return Err(StoreError::InvalidContent);
    }
    Ok(())
}
fn commit_error(_: sqlx::Error) -> StoreError {
    StoreError::CommitUncertain(std::io::Error::other(
        "database commit acknowledgement failed",
    ))
}

pub(super) struct SqliteBackend(pub(super) SqlitePool);
pub(super) struct PgBackend(pub(super) PgPool);

// Both drivers execute the same operations; only locking and schema syntax differ.
macro_rules! backend {
    ($name:ident, $db:ty, $begin:literal, $lock:literal) => {
        #[async_trait]
        impl Storage for $name {
            async fn close(&self) {
                self.0.close().await;
            }
            async fn create(&self, html: &[u8]) -> Result<Saved, StoreError> {
                check_input(html)?;
                let version = version_for(html);
                loop {
                    let id = random_hex(16)?;
                    let mut tx = self.0.begin_with($begin).await?;
                    let inserted = sqlx::query::<$db>(
                        "INSERT INTO uploads (id, latest_hash) VALUES ($1, $2)",
                    )
                    .bind(&id)
                    .bind(&version.content_hash)
                    .execute(&mut *tx)
                    .await;
                    if inserted
                        .as_ref()
                        .is_err_and(|e| matches!(e, sqlx::Error::Database(e) if e.is_unique_violation()))
                    {
                        tx.rollback().await?;
                        continue;
                    }
                    inserted?;
                    sqlx::query::<$db>("INSERT INTO versions (upload_id, content_hash, created_at, size_bytes, version_number, content) VALUES ($1, $2, $3, $4, 1, $5)").bind(&id).bind(&version.content_hash).bind(version.created_at.timestamp_micros()).bind(html.len() as i64).bind(html).execute(&mut *tx).await?;
                    tx.commit().await.map_err(commit_error)?;
                    return Ok(Saved {
                        upload_id: id,
                        version,
                    });
                }
            }
            async fn update(&self, id: &str, html: &[u8]) -> Result<Saved, StoreError> {
                if !valid_id(id) {
                    return Err(StoreError::InvalidId);
                }
                check_input(html)?;
                let candidate = version_for(html);
                let mut tx = self.0.begin_with($begin).await?;
                if sqlx::query_scalar::<$db, String>($lock)
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .is_none()
                {
                    return Err(StoreError::NotFound);
                }
                let existing = sqlx::query_as::<$db, (String, i64, i64)>("SELECT content_hash, created_at, size_bytes FROM versions WHERE upload_id = $1 AND content_hash = $2").bind(id).bind(&candidate.content_hash).fetch_optional(&mut *tx).await?;
                let version = if let Some((digest, timestamp, size)) = existing {
                    let version = recorded_version(digest, timestamp, size)?;
                    if version.size_bytes != html.len() {
                        return Err(StoreError::InvalidMetadata);
                    }
                    sqlx::query::<$db>(
                        "UPDATE versions SET content = $1 WHERE upload_id = $2 AND content_hash = $3",
                    )
                    .bind(html)
                    .bind(id)
                    .bind(&version.content_hash)
                    .execute(&mut *tx)
                    .await?;
                    version
                } else {
                    let number = sqlx::query_scalar::<$db, i64>(
                        "SELECT COALESCE(MAX(version_number), 0) + 1 FROM versions WHERE upload_id = $1",
                    )
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
                    sqlx::query::<$db>("INSERT INTO versions (upload_id, content_hash, created_at, size_bytes, version_number, content) VALUES ($1, $2, $3, $4, $5, $6)").bind(id).bind(&candidate.content_hash).bind(candidate.created_at.timestamp_micros()).bind(html.len() as i64).bind(number).bind(html).execute(&mut *tx).await?;
                    candidate
                };
                sqlx::query::<$db>("UPDATE uploads SET latest_hash = $1 WHERE id = $2")
                    .bind(&version.content_hash)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await.map_err(commit_error)?;
                Ok(Saved {
                    upload_id: id.to_owned(),
                    version,
                })
            }
            async fn get(&self, id: &str, selected: Option<&str>) -> Result<Vec<u8>, StoreError> {
                if !valid_id(id) {
                    return Err(StoreError::InvalidId);
                }
                if selected.is_some_and(|s| !valid_hash(s)) {
                    return Err(StoreError::NotFound);
                }
                let (bytes, digest, size) = sqlx::query_as::<$db, (Vec<u8>, String, i64)>("SELECT v.content, v.content_hash, v.size_bytes FROM uploads u JOIN versions v ON v.upload_id = u.id AND v.content_hash = COALESCE($2, u.latest_hash) WHERE u.id = $1").bind(id).bind(selected).fetch_optional(&self.0).await?.ok_or(StoreError::NotFound)?;
                if bytes.len() as i64 != size || hash(&bytes) != digest || check_input(&bytes).is_err() {
                    return Err(StoreError::InvalidMetadata);
                }
                Ok(bytes)
            }
            async fn check_content(&self, id: &str, selected: Option<&str>) -> Result<(), StoreError> {
                if !valid_id(id) {
                    return Err(StoreError::InvalidId);
                }
                if selected.is_some_and(|s| !valid_hash(s)) {
                    return Err(StoreError::NotFound);
                }
                sqlx::query_scalar::<$db, i64>("SELECT v.size_bytes FROM uploads u JOIN versions v ON v.upload_id = u.id AND v.content_hash = COALESCE($2, u.latest_hash) WHERE u.id = $1").bind(id).bind(selected).fetch_optional(&self.0).await?.ok_or(StoreError::NotFound)?;
                Ok(())
            }
            async fn versions(&self, id: &str) -> Result<Vec<Version>, StoreError> {
                if !valid_id(id) {
                    return Err(StoreError::InvalidId);
                }
                let rows = sqlx::query_as::<$db, (String, i64, i64, String)>("SELECT v.content_hash, v.created_at, v.size_bytes, u.latest_hash FROM uploads u JOIN versions v ON v.upload_id = u.id WHERE u.id = $1 ORDER BY v.version_number").bind(id).fetch_all(&self.0).await?;
                if rows.is_empty() {
                    return Err(StoreError::NotFound);
                }
                if !rows.iter().any(|(digest, _, _, latest)| digest == latest) {
                    return Err(StoreError::InvalidMetadata);
                }
                rows.into_iter()
                    .map(|(digest, timestamp, size, _)| recorded_version(digest, timestamp, size))
                    .collect()
            }
            async fn insert_key(&self, id: &str, digest: &str) -> Result<(), StoreError> {
                sqlx::query::<$db>("INSERT INTO api_keys (id, secret_hash) VALUES ($1, $2)")
                    .bind(id)
                    .bind(digest)
                    .execute(&self.0)
                    .await?;
                Ok(())
            }
            async fn key_hash(&self, id: &str) -> Result<Option<String>, StoreError> {
                Ok(sqlx::query_scalar::<$db, String>(
                    "SELECT secret_hash FROM api_keys WHERE id = $1",
                )
                .bind(id)
                .fetch_optional(&self.0)
                .await?)
            }
            async fn key_ids(&self) -> Result<Vec<String>, StoreError> {
                Ok(
                    sqlx::query_scalar::<$db, String>("SELECT id FROM api_keys ORDER BY id")
                        .fetch_all(&self.0)
                        .await?,
                )
            }
            async fn revoke_key(&self, id: &str) -> Result<(), StoreError> {
                if sqlx::query::<$db>("DELETE FROM api_keys WHERE id = $1")
                    .bind(id)
                    .execute(&self.0)
                    .await?
                    .rows_affected()
                    == 0
                {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            }
        }
    }
}

backend!(
    SqliteBackend,
    sqlx::Sqlite,
    "BEGIN IMMEDIATE",
    "SELECT id FROM uploads WHERE id = $1"
);
backend!(
    PgBackend,
    sqlx::Postgres,
    "BEGIN",
    "SELECT id FROM uploads WHERE id = $1 FOR UPDATE"
);

macro_rules! migration {
    ($tx:ident, $db:ty, $sqls:expr) => {{
        sqlx::query::<$db>("CREATE TABLE IF NOT EXISTS schema_migrations (version BIGINT PRIMARY KEY, checksum TEXT NOT NULL)").execute(&mut *$tx).await?;
        let rows = sqlx::query_as::<$db, (i64, String)>("SELECT version, checksum FROM schema_migrations ORDER BY version").fetch_all(&mut *$tx).await?;
        let scripts = $sqls;
        if rows.len() > scripts.len() || rows.iter().enumerate().any(|(i, (version, checksum))| *version != (i + 1) as i64 || *checksum != hash(scripts[i].as_bytes())) {
            return Err(StoreError::Configuration("unsupported or modified database schema"));
        }
        let migrated = rows.len() < scripts.len();
        for (index, sql) in scripts.iter().enumerate().skip(rows.len()) {
            sqlx::raw_sql(sql).execute(&mut *$tx).await?;
            sqlx::query::<$db>("INSERT INTO schema_migrations (version, checksum) VALUES ($1, $2)")
                .bind((index + 1) as i64).bind(hash(sql.as_bytes())).execute(&mut *$tx).await?;
        }
        migrated
    }};
}

impl SqliteBackend {
    pub async fn initialize(&self) -> Result<(), StoreError> {
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let migrated = migration!(
            tx,
            sqlx::Sqlite,
            [
                include_str!("../../migrations/sqlite/001_initial.sql"),
                include_str!("../../migrations/sqlite/002_content_storage.sql")
            ]
        );
        if migrated
            && !sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&mut *tx)
                .await?
                .is_empty()
        {
            return Err(StoreError::InvalidMetadata);
        }
        tx.commit().await.map_err(commit_error)
    }
}
impl PgBackend {
    pub async fn initialize(&self, schema: &str) -> Result<(), StoreError> {
        let mut tx = self.0.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(schema)
            .execute(&mut *tx)
            .await?;
        sqlx::raw_sql(&format!(
            "CREATE SCHEMA IF NOT EXISTS \"{schema}\"; SET LOCAL search_path TO \"{schema}\";"
        ))
        .execute(&mut *tx)
        .await?;
        migration!(
            tx,
            sqlx::Postgres,
            [
                include_str!("../../migrations/postgres/001_initial.sql"),
                include_str!("../../migrations/postgres/002_content_storage.sql")
            ]
        );
        tx.commit().await.map_err(commit_error)
    }
}
