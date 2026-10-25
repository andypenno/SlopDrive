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

pub(super) struct SqliteBackend(
    pub(super) SqlitePool,
    pub(super) Option<crate::content::S3Content>,
);
pub(super) struct PgBackend(
    pub(super) PgPool,
    pub(super) Option<crate::content::S3Content>,
);

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
                    let key = self.1.as_ref().map(|store| store.key(&id, &version.content_hash));
                    if let (Some(store), Some(key)) = (&self.1, &key) {
                        store.put(key, html).await?;
                    }
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
                    sqlx::query::<$db>("INSERT INTO versions (upload_id, content_hash, created_at, size_bytes, version_number, content, object_key) VALUES ($1, $2, $3, $4, 1, $5, $6)").bind(&id).bind(&version.content_hash).bind(version.created_at.timestamp_micros()).bind(html.len() as i64).bind(if key.is_none() { Some(html) } else { None }).bind(&key).execute(&mut *tx).await?;
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
                let key = if let Some(store) = &self.1 {
                    let found = sqlx::query_scalar::<$db, String>("SELECT id FROM uploads WHERE id = $1")
                        .bind(id).fetch_optional(&self.0).await?;
                    if found.is_none() { return Err(StoreError::NotFound); }
                    let prior = sqlx::query_as::<$db, (String, i64, i64, Option<String>)>("SELECT content_hash, created_at, size_bytes, object_key FROM versions WHERE upload_id = $1 AND content_hash = $2")
                        .bind(id).bind(&candidate.content_hash).fetch_optional(&self.0).await?;
                    match prior {
                        Some((digest, timestamp, size, key)) => {
                            let version = recorded_version(digest, timestamp, size)?;
                            if version.size_bytes != html.len() { return Err(StoreError::InvalidMetadata); }
                            key
                        }
                        None => Some(store.key(id, &candidate.content_hash)),
                    }
                } else { None };
                if let Some(key) = &key {
                    let store = self.1.as_ref().ok_or(StoreError::InvalidMetadata)?;
                    if *key != store.key(id, &candidate.content_hash) { return Err(StoreError::InvalidMetadata); }
                    store.put(key, html).await?;
                }
                let mut tx = self.0.begin_with($begin).await?;
                if sqlx::query_scalar::<$db, String>($lock)
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .is_none()
                {
                    return Err(StoreError::NotFound);
                }
                let existing = sqlx::query_as::<$db, (String, i64, i64, Option<String>)>("SELECT content_hash, created_at, size_bytes, object_key FROM versions WHERE upload_id = $1 AND content_hash = $2").bind(id).bind(&candidate.content_hash).fetch_optional(&mut *tx).await?;
                let version = if let Some((digest, timestamp, size, recorded_key)) = existing {
                    let version = recorded_version(digest, timestamp, size)?;
                    if version.size_bytes != html.len() {
                        return Err(StoreError::InvalidMetadata);
                    }
                    if recorded_key != key { return Err(StoreError::InvalidMetadata); }
                    if recorded_key.is_none() {
                        sqlx::query::<$db>("UPDATE versions SET content = $1 WHERE upload_id = $2 AND content_hash = $3")
                            .bind(html).bind(id).bind(&version.content_hash).execute(&mut *tx).await?;
                    }
                    version
                } else {
                    let number = sqlx::query_scalar::<$db, i64>(
                        "SELECT COALESCE(MAX(version_number), 0) + 1 FROM versions WHERE upload_id = $1",
                    )
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
                    sqlx::query::<$db>("INSERT INTO versions (upload_id, content_hash, created_at, size_bytes, version_number, content, object_key) VALUES ($1, $2, $3, $4, $5, $6, $7)").bind(id).bind(&candidate.content_hash).bind(candidate.created_at.timestamp_micros()).bind(html.len() as i64).bind(number).bind(if key.is_none() { Some(html) } else { None }).bind(&key).execute(&mut *tx).await?;
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
                let (inline, key, digest, size) = sqlx::query_as::<$db, (Option<Vec<u8>>, Option<String>, String, i64)>("SELECT v.content, v.object_key, v.content_hash, v.size_bytes FROM uploads u JOIN versions v ON v.upload_id = u.id AND v.content_hash = COALESCE($2, u.latest_hash) WHERE u.id = $1").bind(id).bind(selected).fetch_optional(&self.0).await?.ok_or(StoreError::NotFound)?;
                let bytes = match (inline, key) {
                    (Some(bytes), None) => bytes,
                    (None, Some(key)) => {
                        let store = self.1.as_ref().ok_or(StoreError::InvalidMetadata)?;
                        if key != store.key(id, &digest) { return Err(StoreError::InvalidMetadata); }
                        store.get(&key).await?
                    }
                    _ => return Err(StoreError::InvalidMetadata),
                };
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

macro_rules! bind_content {
    ($backend:ident, $tx:ident, $db:ty) => {{
        let binding = sqlx::query_as::<$db, (String, String)>(
            "SELECT identity, namespace FROM content_store WHERE id = 1",
        )
        .fetch_optional(&mut *$tx)
        .await?;
        match (binding, &mut $backend.1) {
            (Some((identity, namespace)), Some(store))
                if identity == store.identity
                    && valid_id(&namespace)
                    && !namespace.bytes().any(|b| b.is_ascii_uppercase()) =>
            {
                store.namespace = namespace;
            }
            (Some(_), _) => {
                return Err(StoreError::Configuration(
                    "content storage does not match the database binding",
                ));
            }
            (None, Some(store)) => {
                store.namespace = random_hex(16)?;
                sqlx::query::<$db>(
                    "INSERT INTO content_store (id, identity, namespace) VALUES (1, $1, $2)",
                )
                .bind(&store.identity)
                .bind(&store.namespace)
                .execute(&mut *$tx)
                .await?;
            }
            (None, None) => {}
        }
    }};
}

impl SqliteBackend {
    pub async fn initialize(&mut self) -> Result<(), StoreError> {
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let migrated = super::migrations::sqlite(&mut tx).await?;
        if migrated
            && !sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&mut *tx)
                .await?
                .is_empty()
        {
            return Err(StoreError::InvalidMetadata);
        }
        bind_content!(self, tx, sqlx::Sqlite);
        tx.commit().await.map_err(commit_error)
    }
}
impl PgBackend {
    pub async fn initialize(&mut self, schema: &str) -> Result<(), StoreError> {
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
        super::migrations::postgres(&mut tx).await?;
        bind_content!(self, tx, sqlx::Postgres);
        tx.commit().await.map_err(commit_error)
    }
}
