use crate::helpers::{atomic_write, durable_directory, hash, not_found, random_hex};
use crate::storage::StoreError;
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
        durable_directory(&directory)?;
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
        let directory = fs::File::open(&self.directory)?;
        fs::remove_file(self.directory.join(format!("{id}.json"))).map_err(not_found)?;
        directory.sync_all().map_err(StoreError::CommitUncertain)
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
        let data = match fs::read(self.directory.join(format!("{id}.json"))) {
            Ok(data) => data,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(key_id = id, kind = ?error.kind(), "key record read failed");
                }
                return false;
            }
        };
        let record = match serde_json::from_slice::<KeyRecord>(&data) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(
                    key_id = id,
                    line = error.line(),
                    column = error.column(),
                    "invalid key record"
                );
                return false;
            }
        };
        let candidate = hash(key.as_bytes());
        candidate.as_bytes().ct_eq(record.hash.as_bytes()).into()
    }
}

fn valid_key_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "../tests/keys.rs"]
mod tests;
