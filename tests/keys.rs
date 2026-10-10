use super::*;
use crate::test_helpers::TestApp;

#[tokio::test]
async fn keys_are_hashed_revoked_across_connections_and_listed_in_order() {
    let fixture = TestApp::new().await;
    let keys = &fixture.state.keys;
    let (id, key) = keys.create().await.unwrap();
    assert_eq!(key.len(), 84);
    assert!(keys.valid(&key).await.unwrap());
    assert_eq!(
        fixture.state.storage.key_hash(&id).await.unwrap().unwrap(),
        hash(key.as_bytes())
    );
    let reopened = KeyStore::new(fixture.config.open(fixture.dir.path()).await.unwrap());
    assert!(reopened.valid(&key).await.unwrap());
    for wrong in ["bad", "sb_invalid", &format!("sb_{id}_{}", "0".repeat(64))] {
        assert!(!reopened.valid(wrong).await.unwrap());
    }
    let listed = keys.list().await.unwrap();
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted);
    reopened.revoke(&id).await.unwrap();
    assert!(!keys.valid(&key).await.unwrap());
    assert!(matches!(keys.revoke(&id).await, Err(StoreError::NotFound)));
    assert!(matches!(
        keys.revoke("../").await,
        Err(StoreError::InvalidId)
    ));
    assert!(
        fixture
            .state
            .storage
            .insert_key("bad", &"0".repeat(64))
            .await
            .is_err()
    );
}
