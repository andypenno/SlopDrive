use crate::helpers::{
    atomic_write, durable_directory, not_found, random_hex, valid_hash, valid_id, version_for,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("invalid identifier")]
    InvalidId,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("write published but durability is uncertain: {0}")]
    CommitUncertain(std::io::Error),
    #[error("corrupt metadata: {0}")]
    Corrupt(#[from] serde_json::Error),
    #[error("inconsistent metadata")]
    InvalidMetadata,
    #[error("storage lock poisoned")]
    Locked,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Version {
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    latest: String,
    versions: Vec<Version>,
}

pub struct Saved {
    pub upload_id: String,
    pub version: Version,
}

pub struct FilesystemStorage {
    root: PathBuf,
    writes: Mutex<()>,
    _ownership: File,
    #[cfg(test)]
    pub(crate) metadata_failure: Mutex<Option<StoreError>>,
}

impl FilesystemStorage {
    pub fn new(root: PathBuf) -> Result<Self, StoreError> {
        durable_directory(&root.join("uploads"))?;
        let ownership = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".uploads.lock"))?;
        ownership.try_lock().map_err(std::io::Error::other)?;
        Ok(Self {
            root,
            writes: Mutex::new(()),
            _ownership: ownership,
            #[cfg(test)]
            metadata_failure: Mutex::new(None),
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
        let metadata: Metadata = serde_json::from_slice(&data)?;
        let mut hashes = std::collections::HashSet::new();
        if metadata.versions.is_empty()
            || metadata.versions.iter().any(|version| {
                !valid_hash(&version.content_hash)
                    || version
                        .content_hash
                        .bytes()
                        .any(|byte| byte.is_ascii_uppercase())
                    || !hashes.insert(&version.content_hash)
                    || version.size_bytes == 0
                    || version.size_bytes > crate::helpers::MAX_HTML_BYTES
            })
            || !hashes.contains(&metadata.latest)
        {
            return Err(StoreError::InvalidMetadata);
        }
        Ok(metadata)
    }

    fn save_metadata(&self, id: &str, metadata: &Metadata) -> Result<(), StoreError> {
        #[cfg(test)]
        if let Some(error) = self.metadata_failure.lock().unwrap().take() {
            return Err(error);
        }
        let path = self.upload_path(id)?.join("metadata.json");
        atomic_write(&path, &serde_json::to_vec(metadata)?)?;
        Ok(())
    }
}

impl FilesystemStorage {
    pub fn create(&self, html: &[u8]) -> Result<Saved, StoreError> {
        let _guard = self.writes.lock().map_err(|_| StoreError::Locked)?;
        let parent = File::open(self.root.join("uploads"))?;
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
        let mut metadata_attempted = false;
        let result = (|| {
            atomic_write(&dir.join(format!("{}.html", version.content_hash)), html)?;
            metadata_attempted = true;
            self.save_metadata(
                &id,
                &Metadata {
                    latest: version.content_hash.clone(),
                    versions: vec![version.clone()],
                },
            )?;
            parent.sync_all().map_err(StoreError::CommitUncertain)
        })();
        if let Err(error) = result {
            if (!metadata_attempted || !matches!(error, StoreError::CommitUncertain(_)))
                && let Err(cleanup) = fs::remove_dir_all(&dir).and_then(|()| parent.sync_all())
            {
                tracing::warn!(%cleanup, "unpublished upload cleanup failed");
            }
            return Err(error);
        }
        Ok(Saved {
            upload_id: id,
            version,
        })
    }

    pub fn update(&self, id: &str, html: &[u8]) -> Result<Saved, StoreError> {
        let _guard = self.writes.lock().map_err(|_| StoreError::Locked)?;
        let mut metadata = self.read_metadata(id)?;
        let candidate = version_for(html);
        let hash = candidate.content_hash.clone();
        let existing = metadata
            .versions
            .iter()
            .find(|v| v.content_hash == hash)
            .cloned();
        if existing.is_some() {
            let path = self.upload_path(id)?.join(format!("{hash}.html"));
            match fs::read(&path) {
                Ok(existing) if existing == html => {}
                Ok(_) => atomic_write(&path, html)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    atomic_write(&path, html)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if metadata.latest == hash {
            File::open(self.upload_path(id)?)?
                .sync_all()
                .map_err(StoreError::CommitUncertain)?;
            let version = existing.ok_or(StoreError::NotFound)?;
            return Ok(Saved {
                upload_id: id.to_owned(),
                version,
            });
        }
        let unpublished = existing.is_none();
        let path = self.upload_path(id)?.join(format!("{hash}.html"));
        let version = existing.unwrap_or(candidate);
        let mut metadata_attempted = false;
        let result = (|| {
            if unpublished {
                atomic_write(&path, html)?;
                metadata.versions.push(version.clone());
            }
            metadata.latest = hash;
            metadata_attempted = true;
            self.save_metadata(id, &metadata)
        })();
        if let Err(error) = result {
            if unpublished
                && (!metadata_attempted || !matches!(error, StoreError::CommitUncertain(_)))
                && let Err(cleanup) = fs::remove_file(&path)
                    .and_then(|()| File::open(path.parent().unwrap())?.sync_all())
                && cleanup.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%cleanup, "unpublished content cleanup failed");
            }
            return Err(error);
        }
        Ok(Saved {
            upload_id: id.to_owned(),
            version,
        })
    }

    fn content_path(&self, id: &str, hash: Option<&str>) -> Result<PathBuf, StoreError> {
        let metadata = self.read_metadata(id)?;
        let selected = hash.unwrap_or(&metadata.latest);
        if !valid_hash(selected) || !metadata.versions.iter().any(|v| v.content_hash == selected) {
            return Err(StoreError::NotFound);
        }
        let path = self.upload_path(id)?.join(format!("{selected}.html"));
        if !fs::metadata(&path).map_err(not_found)?.is_file() {
            return Err(std::io::Error::other("content is not a regular file").into());
        }
        Ok(path)
    }

    pub fn get(&self, id: &str, hash: Option<&str>) -> Result<Vec<u8>, StoreError> {
        fs::read(self.content_path(id, hash)?).map_err(not_found)
    }

    pub fn check_content(&self, id: &str, hash: Option<&str>) -> Result<(), StoreError> {
        File::open(self.content_path(id, hash)?).map_err(not_found)?;
        Ok(())
    }

    pub fn versions(&self, id: &str) -> Result<Vec<Version>, StoreError> {
        Ok(self.read_metadata(id)?.versions)
    }
}

#[cfg(test)]
#[path = "../tests/storage.rs"]
mod tests;
