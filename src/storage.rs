use crate::helpers::{
    atomic_write, hash, not_found, random_hex, valid_hash, valid_id, version_for,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("invalid identifier")]
    InvalidId,
    #[error("I/O error: {0}")]
    Io(std::io::Error),
    #[error("corrupt metadata: {0}")]
    Corrupt(serde_json::Error),
    #[error("storage lock poisoned")]
    Locked,
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Corrupt(value)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Version {
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    upload_id: String,
    latest: String,
    versions: Vec<Version>,
}

pub struct Saved {
    pub upload_id: String,
    pub version: Version,
}

pub trait Storage: Send + Sync {
    fn create(&self, html: &[u8]) -> Result<Saved, StoreError>;
    fn update(&self, id: &str, html: &[u8]) -> Result<Saved, StoreError>;
    fn get(&self, id: &str, hash: Option<&str>) -> Result<Vec<u8>, StoreError>;
    fn versions(&self, id: &str) -> Result<Vec<Version>, StoreError>;
}

pub struct FilesystemStorage {
    root: PathBuf,
    writes: Mutex<()>,
}

impl FilesystemStorage {
    pub fn new(root: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir_all(root.join("uploads"))?;
        Ok(Self {
            root,
            writes: Mutex::new(()),
        })
    }

    fn upload_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        if !valid_id(id) {
            return Err(StoreError::InvalidId);
        }
        Ok(self.root.join("uploads").join(id))
    }

    fn read_metadata(&self, id: &str) -> Result<Metadata, StoreError> {
        let path = self.upload_path(id)?.join("metadata.json");
        let data = fs::read(path).map_err(not_found)?;
        Ok(serde_json::from_slice(&data)?)
    }

    fn save_metadata(&self, id: &str, metadata: &Metadata) -> Result<(), StoreError> {
        let path = self.upload_path(id)?.join("metadata.json");
        atomic_write(&path, &serde_json::to_vec(metadata)?)?;
        Ok(())
    }
}

impl Storage for FilesystemStorage {
    fn create(&self, html: &[u8]) -> Result<Saved, StoreError> {
        let _guard = self.writes.lock().map_err(|_| StoreError::Locked)?;
        let (id, dir) = loop {
            let id = random_hex(16)?;
            let dir = self.upload_path(&id)?;
            match fs::create_dir(&dir) {
                Ok(()) => break (id, dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let version = version_for(html);
        atomic_write(&dir.join(format!("{}.html", version.content_hash)), html)?;
        self.save_metadata(
            &id,
            &Metadata {
                upload_id: id.clone(),
                latest: version.content_hash.clone(),
                versions: vec![version.clone()],
            },
        )?;
        Ok(Saved {
            upload_id: id,
            version,
        })
    }

    fn update(&self, id: &str, html: &[u8]) -> Result<Saved, StoreError> {
        let _guard = self.writes.lock().map_err(|_| StoreError::Locked)?;
        let mut metadata = self.read_metadata(id)?;
        let hash = hash(html);
        if metadata.latest == hash {
            let version = metadata
                .versions
                .iter()
                .find(|v| v.content_hash == hash)
                .ok_or(StoreError::NotFound)?
                .clone();
            return Ok(Saved {
                upload_id: id.to_owned(),
                version,
            });
        }
        let version =
            if let Some(existing) = metadata.versions.iter().find(|v| v.content_hash == hash) {
                existing.clone()
            } else {
                let version = version_for(html);
                atomic_write(&self.upload_path(id)?.join(format!("{hash}.html")), html)?;
                metadata.versions.push(version.clone());
                version
            };
        metadata.latest = hash;
        self.save_metadata(id, &metadata)?;
        Ok(Saved {
            upload_id: id.to_owned(),
            version,
        })
    }

    fn get(&self, id: &str, hash: Option<&str>) -> Result<Vec<u8>, StoreError> {
        let metadata = self.read_metadata(id)?;
        let selected = hash.unwrap_or(&metadata.latest);
        if !valid_hash(selected) || !metadata.versions.iter().any(|v| v.content_hash == selected) {
            return Err(StoreError::NotFound);
        }
        fs::read(self.upload_path(id)?.join(format!("{selected}.html"))).map_err(not_found)
    }

    fn versions(&self, id: &str) -> Result<Vec<Version>, StoreError> {
        Ok(self.read_metadata(id)?.versions)
    }
}

#[cfg(test)]
#[path = "../tests/storage.rs"]
mod tests;
