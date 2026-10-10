use crate::helpers::{hash, random_hex, valid_hash};
use crate::storage::{Storage, StoreError};
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub struct KeyStore {
    backend: Arc<dyn Storage>,
}
impl KeyStore {
    pub fn new(backend: Arc<dyn Storage>) -> Self {
        Self { backend }
    }
    pub async fn create(&self) -> Result<(String, String), StoreError> {
        loop {
            let id = random_hex(8)?;
            let key = format!("sb_{id}_{}", random_hex(32)?);
            match self.backend.insert_key(&id, &hash(key.as_bytes())).await {
                Ok(()) => return Ok((id, key)),
                Err(StoreError::Conflict) => continue,
                Err(error) => return Err(error),
            }
        }
    }
    pub async fn list(&self) -> Result<Vec<String>, StoreError> {
        self.backend.key_ids().await
    }
    pub async fn revoke(&self, id: &str) -> Result<(), StoreError> {
        if !valid_key_id(id) {
            return Err(StoreError::InvalidId);
        }
        self.backend.revoke_key(id).await
    }
    pub async fn valid(&self, key: &str) -> Result<bool, StoreError> {
        let Some(id) = key
            .strip_prefix("sb_")
            .and_then(|rest| rest.split_once('_'))
            .map(|(id, _)| id)
        else {
            return Ok(false);
        };
        if !valid_key_id(id) || key.len() != 84 {
            return Ok(false);
        }
        let Some(recorded) = self.backend.key_hash(id).await? else {
            return Ok(false);
        };
        if !valid_hash(&recorded) {
            return Err(StoreError::InvalidMetadata);
        }
        Ok(hash(key.as_bytes())
            .as_bytes()
            .ct_eq(recorded.as_bytes())
            .into())
    }
}
fn valid_key_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "../tests/keys.rs"]
mod tests;
