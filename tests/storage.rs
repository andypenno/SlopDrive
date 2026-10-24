use super::*;
use crate::helpers::{MAX_HTML_BYTES, hash, random_hex};
use crate::test_helpers::TestApp;

#[tokio::test]
async fn constraints_reject_null_identifiers_and_non_integer_metadata() {
    let fixture = TestApp::new().await;
    assert!(fixture.db.rejects_null_key_id().await);
    let saved = fixture
        .state
        .storage
        .create(b"<p>original</p>")
        .await
        .unwrap();
    assert!(fixture.db.rejects_invalid_timestamp(&saved.upload_id).await);
    assert_eq!(
        fixture
            .state
            .storage
            .versions(&saved.upload_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn versions_reopen_retry_and_restore_without_duplicates() {
    let fixture = TestApp::new().await;
    let store = &fixture.state.storage;
    let first = store.create(b"<p>one</p>").await.unwrap();
    let second = store.update(&first.upload_id, b"<p>two</p>").await.unwrap();
    let retry = store.update(&first.upload_id, b"<p>two</p>").await.unwrap();
    assert_eq!(second.version.created_at, retry.version.created_at);
    let reopened = fixture.config.open(fixture.dir.path()).await.unwrap();
    assert_eq!(
        reopened.get(&first.upload_id, None).await.unwrap(),
        b"<p>two</p>"
    );
    assert_eq!(
        reopened
            .get(&first.upload_id, Some(&first.version.content_hash))
            .await
            .unwrap(),
        b"<p>one</p>"
    );
    let restored = reopened
        .update(&first.upload_id, b"<p>one</p>")
        .await
        .unwrap();
    assert_eq!(first.version.created_at, restored.version.created_at);
    assert_eq!(first.version.size_bytes, restored.version.size_bytes);
    let history = reopened.versions(&first.upload_id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content_hash, first.version.content_hash);
    assert_eq!(history[1].content_hash, second.version.content_hash);
    assert_eq!(
        reopened.get(&first.upload_id, None).await.unwrap(),
        b"<p>one</p>"
    );
}

#[tokio::test]
async fn independent_connections_keep_all_concurrent_versions() {
    let fixture = TestApp::new().await;
    let first = fixture
        .state
        .storage
        .create(b"<p>initial</p>")
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for n in 0..8 {
        let store = fixture.config.open(fixture.dir.path()).await.unwrap();
        let id = first.upload_id.clone();
        tasks.push(tokio::spawn(async move {
            store
                .update(&id, format!("<p>{n}</p>").as_bytes())
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let versions = fixture
        .state
        .storage
        .versions(&first.upload_id)
        .await
        .unwrap();
    assert_eq!(versions.len(), 9);
    for n in 0..8 {
        let content = format!("<p>{n}</p>");
        assert_eq!(
            fixture
                .state
                .storage
                .get(&first.upload_id, Some(&hash(content.as_bytes())))
                .await
                .unwrap(),
            content.as_bytes()
        );
    }
}

#[tokio::test]
async fn invalid_missing_and_boundary_inputs_fail_without_writes() {
    let fixture = TestApp::new().await;
    let store = &fixture.state.storage;
    for bytes in [
        &b""[..],
        &b"a\0"[..],
        &b"\xff"[..],
        &vec![b'x'; MAX_HTML_BYTES + 1][..],
    ] {
        assert!(matches!(
            store.create(bytes).await,
            Err(StoreError::InvalidContent)
        ));
    }
    for id in ["../", "bad", &"0".repeat(32)] {
        assert!(store.get(id, None).await.is_err());
        assert!(store.check_content(id, None).await.is_err());
        assert!(store.versions(id).await.is_err());
        assert!(store.update(id, b"<p>x</p>").await.is_err());
    }
    assert_eq!(fixture.db.upload_count().await, 0);
    let maximum = vec![b'x'; MAX_HTML_BYTES];
    let saved = store.create(&maximum).await.unwrap();
    assert_eq!(store.get(&saved.upload_id, None).await.unwrap(), maximum);
    assert!(store.get(&saved.upload_id, Some("invalid")).await.is_err());
    assert!(
        store
            .get(&saved.upload_id, Some(&"0".repeat(64)))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn constraints_rollback_partial_writes_and_reject_newer_schema() {
    let fixture = TestApp::new().await;
    let saved = fixture
        .state
        .storage
        .create(b"<p>original</p>")
        .await
        .unwrap();
    assert!(fixture.db.rejects_duplicate_version(&saved.upload_id).await);
    assert_eq!(
        fixture
            .state
            .storage
            .versions(&saved.upload_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .state
            .storage
            .get(&saved.upload_id, None)
            .await
            .unwrap(),
        b"<p>original</p>"
    );
    assert!(fixture.db.rejects_missing_latest(&saved.upload_id).await);
    fixture.db.set_schema_version(3).await;
    assert!(matches!(
        fixture.config.open(fixture.dir.path()).await,
        Err(StoreError::Configuration(_))
    ));
}

#[tokio::test]
async fn concurrent_schema_initialization_is_idempotent() {
    let fixture = TestApp::new().await;
    let mut fresh = fixture.config.clone();
    let fresh_dir = tempfile::tempdir().unwrap();
    if fresh.provider == crate::database::Provider::Postgres {
        fresh.schema = Some(format!("test_{}", random_hex(8).unwrap()));
    }
    let config = &fresh;
    let root = fresh_dir.path();
    let (a, b, c, d) = tokio::join!(
        config.open(root),
        config.open(root),
        config.open(root),
        config.open(root)
    );
    for store in [a, b, c, d] {
        assert!(store.unwrap().key_ids().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn modified_migrations_are_rejected_without_changing_data() {
    let fixture = TestApp::new().await;
    let saved = fixture
        .state
        .storage
        .create(b"<p>preserve me</p>")
        .await
        .unwrap();
    fixture.db.corrupt_migration_checksum().await;
    assert!(matches!(
        fixture.config.open(fixture.dir.path()).await,
        Err(StoreError::Configuration(_))
    ));
    assert_eq!(
        fixture
            .state
            .storage
            .get(&saved.upload_id, None)
            .await
            .unwrap(),
        b"<p>preserve me</p>"
    );
}

#[tokio::test]
async fn concurrent_identical_updates_preserve_one_version() {
    let fixture = TestApp::new().await;
    let saved = fixture
        .state
        .storage
        .create(b"<p>original</p>")
        .await
        .unwrap();
    let a = fixture.config.open(fixture.dir.path()).await.unwrap();
    let b = fixture.config.open(fixture.dir.path()).await.unwrap();
    let (a, b) = tokio::join!(
        a.update(&saved.upload_id, b"<p>same</p>"),
        b.update(&saved.upload_id, b"<p>same</p>")
    );
    assert_eq!(a.unwrap().version.created_at, b.unwrap().version.created_at);
    assert_eq!(
        fixture
            .state
            .storage
            .versions(&saved.upload_id)
            .await
            .unwrap()
            .len(),
        2
    );
}
