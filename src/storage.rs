use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("invalid identifier")]
    InvalidId,
    #[error("invalid content")]
    InvalidContent,
    #[error("identifier collision")]
    Conflict,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("write durability is uncertain: {0}")]
    CommitUncertain(std::io::Error),
    #[error("inconsistent database record")]
    InvalidMetadata,
    #[error("database operation failed ({0})")]
    Database(String),
    #[error("{0}")]
    Configuration(&'static str),
}

#[derive(Clone, Serialize)]
pub struct Version {
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
}
pub struct Saved {
    pub upload_id: String,
    pub version: Version,
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn close(&self);
    async fn create(&self, html: &[u8]) -> Result<Saved, StoreError>;
    async fn update(&self, id: &str, html: &[u8]) -> Result<Saved, StoreError>;
    async fn get(&self, id: &str, hash: Option<&str>) -> Result<Vec<u8>, StoreError>;
    async fn check_content(&self, id: &str, hash: Option<&str>) -> Result<(), StoreError>;
    async fn versions(&self, id: &str) -> Result<Vec<Version>, StoreError>;
    async fn insert_key(&self, id: &str, hash: &str) -> Result<(), StoreError>;
    async fn key_hash(&self, id: &str) -> Result<Option<String>, StoreError>;
    async fn key_ids(&self) -> Result<Vec<String>, StoreError>;
    async fn revoke_key(&self, id: &str) -> Result<(), StoreError>;
}

#[cfg(test)]
#[path = "../tests/storage.rs"]
mod tests;
