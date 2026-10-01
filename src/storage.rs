use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
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

pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn random_hex(bytes: usize) -> Result<String, StoreError> {
    let mut data = vec![0u8; bytes];
    getrandom::fill(&mut data).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(hex::encode(data))
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StoreError> {
    let temp = path.with_extension(format!("tmp-{}", random_hex(8)?));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().ok_or(StoreError::NotFound)?)?.sync_all()?;
    Ok(())
}

fn version_for(html: &[u8]) -> Version {
    Version {
        content_hash: hash(html),
        created_at: Utc::now(),
        size_bytes: html.len(),
    }
}

fn not_found(error: std::io::Error) -> StoreError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StoreError::NotFound
    } else {
        StoreError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn versions_survive_restart_and_latest_retry_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        let first = store.create(b"<html>one</html>").unwrap();
        let second = store.update(&first.upload_id, b"<html>two</html>").unwrap();
        let retry = store.update(&first.upload_id, b"<html>two</html>").unwrap();
        assert_eq!(second.version.content_hash, retry.version.content_hash);
        assert_eq!(second.version.created_at, retry.version.created_at);
        assert_eq!(second.version.size_bytes, retry.version.size_bytes);
        drop(store);
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.versions(&first.upload_id).unwrap().len(), 2);
        assert_eq!(
            store.get(&first.upload_id, None).unwrap(),
            b"<html>two</html>"
        );
        assert_eq!(
            store
                .get(&first.upload_id, Some(&first.version.content_hash))
                .unwrap(),
            b"<html>one</html>"
        );
    }

    #[test]
    fn concurrent_updates_keep_every_distinct_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FilesystemStorage::new(dir.path().to_path_buf()).unwrap());
        let first = store.create(b"<html>initial</html>").unwrap();
        std::thread::scope(|scope| {
            for number in 0..8 {
                let store = Arc::clone(&store);
                let id = &first.upload_id;
                scope.spawn(move || {
                    store
                        .update(id, format!("<html>{number}</html>").as_bytes())
                        .unwrap();
                });
            }
        });
        let versions = store.versions(&first.upload_id).unwrap();
        let mut expected = vec![b"<html>initial</html>".to_vec()];
        expected.extend((0..8).map(|n| format!("<html>{n}</html>").into_bytes()));
        let actual: std::collections::BTreeSet<_> =
            versions.iter().map(|v| v.content_hash.clone()).collect();
        assert_eq!(actual, expected.iter().map(|bytes| hash(bytes)).collect());
        assert_eq!(versions.len(), expected.len());
        for bytes in expected {
            assert_eq!(
                store.get(&first.upload_id, Some(&hash(&bytes))).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn historical_revert_preserves_version_metadata_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        let first = store.create(b"<p>one</p>").unwrap();
        store.update(&first.upload_id, b"<p>two</p>").unwrap();
        let reverted = store.update(&first.upload_id, b"<p>one</p>").unwrap();
        assert_eq!(reverted.version.content_hash, first.version.content_hash);
        assert_eq!(reverted.version.created_at, first.version.created_at);
        assert_eq!(reverted.version.size_bytes, first.version.size_bytes);
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.get(&first.upload_id, None).unwrap(), b"<p>one</p>");
        assert_eq!(store.versions(&first.upload_id).unwrap().len(), 2);
    }

    #[test]
    fn identifiers_hashes_and_known_sha256_vector() {
        for id in [
            "",
            "../",
            &"a".repeat(31),
            &"a".repeat(33),
            &"g".repeat(32),
            &"é".repeat(16),
        ] {
            assert!(!valid_id(id), "{id}");
        }
        assert!(valid_id(&"aF".repeat(16)));
        for value in [
            "",
            &"a".repeat(63),
            &"a".repeat(65),
            &"g".repeat(64),
            &"é".repeat(32),
        ] {
            assert!(!valid_hash(value), "{value}");
        }
        assert!(valid_hash(&"aF".repeat(32)));
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for bytes in [0, 8, 16, 32] {
            let value = random_hex(bytes).unwrap();
            assert_eq!(value.len(), bytes * 2);
            assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn invalid_and_missing_uploads_reject_every_storage_operation() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        for id in ["../keys", "not-an-id"] {
            assert!(matches!(store.update(id, b"x"), Err(StoreError::InvalidId)));
            assert!(matches!(store.get(id, None), Err(StoreError::InvalidId)));
            assert!(matches!(store.versions(id), Err(StoreError::InvalidId)));
        }
        let id = "0".repeat(32);
        assert!(matches!(store.update(&id, b"x"), Err(StoreError::NotFound)));
        assert!(matches!(store.get(&id, None), Err(StoreError::NotFound)));
        assert!(matches!(store.versions(&id), Err(StoreError::NotFound)));
    }

    #[test]
    fn corrupt_metadata_and_missing_content_return_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        let saved = store.create(b"<p>x</p>").unwrap();
        let path = store.upload_path(&saved.upload_id).unwrap();
        fs::rename(
            path.join(format!("{}.html", saved.version.content_hash)),
            path.join("hidden.html"),
        )
        .unwrap();
        assert!(matches!(
            store.get(&saved.upload_id, None),
            Err(StoreError::NotFound)
        ));
        let mut metadata = store.read_metadata(&saved.upload_id).unwrap();
        for latest in ["bad".to_owned(), "0".repeat(64)] {
            metadata.latest = latest;
            store.save_metadata(&saved.upload_id, &metadata).unwrap();
            assert!(matches!(
                store.get(&saved.upload_id, None),
                Err(StoreError::NotFound)
            ));
        }
        metadata.latest = saved.version.content_hash.clone();
        metadata.versions.clear();
        store.save_metadata(&saved.upload_id, &metadata).unwrap();
        assert!(matches!(
            store.update(&saved.upload_id, b"<p>x</p>"),
            Err(StoreError::NotFound)
        ));
        fs::write(path.join("metadata.json"), b"not json").unwrap();
        assert!(matches!(
            store.get(&saved.upload_id, None),
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store.update(&saved.upload_id, b"x"),
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store.versions(&saved.upload_id),
            Err(StoreError::Corrupt(_))
        ));
    }

    #[test]
    fn filesystem_failures_and_poisoned_lock_propagate() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, b"existing").unwrap();
        assert!(matches!(
            FilesystemStorage::new(blocker),
            Err(StoreError::Io(_))
        ));
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        fs::rename(dir.path().join("uploads"), dir.path().join("moved")).unwrap();
        fs::write(dir.path().join("uploads"), b"blocker").unwrap();
        assert!(matches!(store.create(b"x"), Err(StoreError::Io(_))));
        assert!(matches!(
            store.versions(&"0".repeat(32)),
            Err(StoreError::Io(_))
        ));
        let _ = std::panic::catch_unwind(|| {
            let _guard = store.writes.lock().unwrap();
            panic!("poison owned test lock");
        });
        assert!(matches!(store.create(b"x"), Err(StoreError::Locked)));
        assert!(matches!(
            store.update(&"0".repeat(32), b"x"),
            Err(StoreError::Locked)
        ));
    }

    #[test]
    fn atomic_write_replaces_content_and_preserves_destination_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let destination = dir.path().join("directory");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("existing"), b"keep").unwrap();
        assert!(matches!(
            atomic_write(&destination, b"replacement"),
            Err(StoreError::Io(_))
        ));
        assert_eq!(fs::read(destination.join("existing")).unwrap(), b"keep");
        assert!(matches!(
            atomic_write(&dir.path().join("missing/content"), b"x"),
            Err(StoreError::Io(_))
        ));
    }

    #[test]
    fn shared_io_error_mapping_preserves_kind_and_message() {
        use std::io::{Error, ErrorKind};
        assert!(matches!(
            not_found(Error::from(ErrorKind::NotFound)),
            StoreError::NotFound
        ));
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidInput,
            ErrorKind::Other,
        ] {
            let StoreError::Io(error) = not_found(Error::new(kind, "original detail")) else {
                panic!("non-missing errors must retain their cause");
            };
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "original detail");
        }
    }
}
