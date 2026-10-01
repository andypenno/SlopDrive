use crate::storage::{StoreError, atomic_write, hash, not_found, random_hex};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use subtle::ConstantTimeEq;

#[derive(Serialize, Deserialize)]
struct KeyRecord {
    hash: String,
}

pub struct KeyStore {
    directory: PathBuf,
}

impl KeyStore {
    pub fn new(data_path: PathBuf) -> Result<Self, StoreError> {
        let directory = data_path.join("keys");
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    pub fn create(&self) -> Result<(String, String), StoreError> {
        loop {
            let id = random_hex(8)?;
            let key = format!("sb_{id}_{}", random_hex(32)?);
            let path = self.directory.join(format!("{id}.json"));
            if path.exists() {
                continue;
            }
            atomic_write(
                &path,
                &serde_json::to_vec(&KeyRecord {
                    hash: hash(key.as_bytes()),
                })?,
            )?;
            return Ok((id, key));
        }
    }

    pub fn list(&self) -> Result<Vec<String>, StoreError> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .filter(|id| valid_key_id(id))
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn revoke(&self, id: &str) -> Result<(), StoreError> {
        if !valid_key_id(id) {
            return Err(StoreError::InvalidId);
        }
        fs::remove_file(self.directory.join(format!("{id}.json"))).map_err(not_found)?;
        Ok(())
    }

    pub fn valid(&self, key: &str) -> bool {
        let Some(id) = key
            .strip_prefix("sb_")
            .and_then(|rest| rest.split_once('_'))
            .map(|(id, _)| id)
        else {
            return false;
        };
        if !valid_key_id(id) || key.len() != 84 {
            return false;
        }
        let Ok(data) = fs::read(self.directory.join(format!("{id}.json"))) else {
            return false;
        };
        let Ok(record) = serde_json::from_slice::<KeyRecord>(&data) else {
            return false;
        };
        let candidate = hash(key.as_bytes());
        candidate.as_bytes().ct_eq(record.hash.as_bytes()).into()
    }
}

fn valid_key_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_revocation_is_immediate() {
        let dir = tempfile::tempdir().unwrap();
        let keys = KeyStore::new(dir.path().to_path_buf()).unwrap();
        let (id, key) = keys.create().unwrap();
        assert!(keys.valid(&key));
        assert!(!keys.valid("sb_invalid"));
        keys.revoke(&id).unwrap();
        assert!(!keys.valid(&key));
    }

    #[test]
    fn persisted_keys_are_hashed_and_malformed_records_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let keys = KeyStore::new(dir.path().to_path_buf()).unwrap();
        let (id, key) = keys.create().unwrap();
        let path = dir.path().join("keys").join(format!("{id}.json"));
        let data = fs::read_to_string(&path).unwrap();
        let record: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(record, serde_json::json!({"hash": hash(key.as_bytes())}));
        assert!(!data.contains(&key));
        assert!(KeyStore::new(dir.path().to_path_buf()).unwrap().valid(&key));
        let wrong_secret = format!("sb_{id}_{}", "0".repeat(64));
        assert!(!keys.valid(&wrong_secret));
        for candidate in [
            "".to_owned(),
            key.replacen("sb_", "xx_", 1),
            format!("sb_{}_{}", "g".repeat(16), "0".repeat(64)),
            key[..83].to_owned(),
            format!("{key}0"),
        ] {
            assert!(!keys.valid(&candidate));
        }
        for corrupt in ["not json", "{}", "{\"hash\":null}", "{\"hash\":\"bad\"}"] {
            fs::write(&path, corrupt).unwrap();
            assert!(!keys.valid(&key));
        }
        keys.revoke(&id).unwrap();
        assert!(!keys.valid(&key));
    }

    #[test]
    fn list_filters_sorts_and_revoke_handles_invalid_missing_and_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let keys = KeyStore::new(dir.path().to_path_buf()).unwrap();
        let mut expected: Vec<_> = (0..3).map(|_| keys.create().unwrap().0).collect();
        expected.sort();
        for name in [
            "notes.txt",
            "bad.json",
            "gggggggggggggggg.json",
            "0000000000000000.json.tmp",
        ] {
            fs::write(keys.directory.join(name), b"ignored").unwrap();
        }
        assert_eq!(
            KeyStore::new(dir.path().to_path_buf())
                .unwrap()
                .list()
                .unwrap(),
            expected
        );
        assert!(matches!(keys.revoke("../keys"), Err(StoreError::InvalidId)));
        assert!(matches!(
            keys.revoke(&"0".repeat(16)),
            Err(StoreError::NotFound)
        ));
        fs::create_dir(keys.directory.join(format!("{}.json", "0".repeat(16)))).unwrap();
        assert!(matches!(
            keys.revoke(&"0".repeat(16)),
            Err(StoreError::Io(_))
        ));
        fs::rename(&keys.directory, dir.path().join("moved")).unwrap();
        fs::write(&keys.directory, b"blocker").unwrap();
        assert!(matches!(keys.list(), Err(StoreError::Io(_))));
        assert!(matches!(keys.create(), Err(StoreError::Io(_))));
        assert!(matches!(
            KeyStore::new(dir.path().to_path_buf()),
            Err(StoreError::Io(_))
        ));
    }
}
