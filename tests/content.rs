use super::*;
use crate::test_helpers::TestApp;
use crate::test_s3::TestS3;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn configuration_rejects_unsafe_and_incomplete_settings() {
    let server = TestS3::new().await;
    for endpoint in [
        "ftp://example.com",
        "https://user:secret@example.com",
        "https://example.com/path",
        "https://example.com?secret",
        "https://example.com#fragment",
    ] {
        let mut config = server.config.clone();
        config.s3_endpoint = Some(endpoint.into());
        assert!(matches!(config.open(), Err(StoreError::Configuration(_))));
    }
    let mut config = server.config.clone();
    config.s3_allow_http = None;
    assert!(config.open().is_err());
    config = server.config.clone();
    config.s3_secret_access_key = None;
    assert!(config.open().is_err());
    config = server.config.clone();
    config.content_storage = ContentStorage::Database;
    assert!(config.open().is_err());
    for prefix in ["../escape", "trailing/", "/absolute", "a//b", "a?b"] {
        config = server.config.clone();
        config.s3_prefix = Some(prefix.into());
        assert!(config.open().is_err());
    }
    for limit in [0, 5001] {
        config = server.config.clone();
        config.s3_timeout_ms = Some(limit);
        assert!(config.open().is_err());
    }
}

#[tokio::test]
async fn s3_round_trip_missing_corrupt_and_oversized_objects() {
    let server = TestS3::new().await;
    let fixture = TestApp::with_content(server.config.clone()).await;
    let store = &fixture.state.storage;
    let saved = store.create(b"<p>one</p>").await.unwrap();
    assert_eq!(
        store.get(&saved.upload_id, None).await.unwrap(),
        b"<p>one</p>"
    );
    let history = store.versions(&saved.upload_id).await.unwrap();
    let key = server
        .objects
        .bytes
        .lock()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    server.objects.bytes.lock().unwrap().remove(&key);
    assert!(store.get(&saved.upload_id, None).await.is_err());
    assert!(store.check_content(&saved.upload_id, None).await.is_ok());
    assert_eq!(store.versions(&saved.upload_id).await.unwrap().len(), 1);
    store.update(&saved.upload_id, b"<p>one</p>").await.unwrap();
    server
        .objects
        .bytes
        .lock()
        .unwrap()
        .insert(key.clone(), b"<p>bad</p>".to_vec());
    assert!(matches!(
        store.get(&saved.upload_id, None).await,
        Err(StoreError::InvalidMetadata)
    ));
    store.update(&saved.upload_id, b"<p>one</p>").await.unwrap();
    assert_eq!(
        store.versions(&saved.upload_id).await.unwrap()[0].created_at,
        history[0].created_at
    );
    server
        .objects
        .bytes
        .lock()
        .unwrap()
        .insert(key, vec![b'x'; MAX_HTML_BYTES + 1]);
    assert!(matches!(
        store.get(&saved.upload_id, None).await,
        Err(StoreError::InvalidMetadata)
    ));
    let max = vec![b'x'; MAX_HTML_BYTES];
    let saved = store.create(&max).await.unwrap();
    assert_eq!(store.get(&saved.upload_id, None).await.unwrap(), max);
}

#[tokio::test]
async fn s3_failures_are_redacted_on_both_transports_and_preserve_latest() {
    let server = TestS3::new().await;
    let fixture = TestApp::with_content(server.config.clone()).await;
    let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
    let id = saved["upload_id"].as_str().unwrap();
    server.objects.failure.store(403, Ordering::Relaxed);
    for path in ["/upload".to_owned(), format!("/upload/{id}")] {
        let (status, error) = fixture.post_html(&path, b"<p>changed</p>").await;
        assert_eq!(status, crate::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["error"], "internal error");
    }
    for (name, arguments) in [
        (
            "upload_html",
            serde_json::json!({"content":"<p>changed</p>"}),
        ),
        (
            "update_html",
            serde_json::json!({"upload_id": id, "content":"<p>changed</p>"}),
        ),
    ] {
        let error = fixture.tool(1, name, arguments).await;
        assert_eq!(error["isError"], true);
        assert_eq!(error["content"][0]["text"], "internal error");
    }
    assert_eq!(fixture.db.upload_count().await, 1);
    assert_eq!(fixture.state.storage.versions(id).await.unwrap().len(), 1);
    server.objects.failure.store(0, Ordering::Relaxed);
    fixture
        .db
        .reject_versions(saved["content_hash"].as_str().unwrap())
        .await;
    assert!(
        fixture
            .state
            .storage
            .update(id, b"<p>orphan</p>")
            .await
            .is_err()
    );
    assert_eq!(server.objects.bytes.lock().unwrap().len(), 2);
    assert_eq!(
        fixture.state.storage.get(id, None).await.unwrap(),
        b"<p>original</p>"
    );
    assert_eq!(fixture.state.storage.versions(id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn s3_deadlines_cover_body_and_retries_without_publication() {
    let server = TestS3::new().await;
    let mut config = server.config.clone();
    config.s3_timeout_ms = Some(30);
    let fixture = TestApp::with_content(config).await;
    server.objects.delay_ms.store(250, Ordering::Relaxed);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        fixture.state.storage.create(b"<p>timeout</p>"),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(StoreError::ObjectStorage)));
    assert_eq!(fixture.db.upload_count().await, 0);
    server.objects.delay_ms.store(0, Ordering::Relaxed);
    let saved = fixture
        .state
        .storage
        .create(b"<p>read deadline</p>")
        .await
        .unwrap();
    server.objects.body_delay_ms.store(250, Ordering::Relaxed);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        fixture.state.storage.get(&saved.upload_id, None),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(StoreError::ObjectStorage)));
}

#[tokio::test]
async fn accepted_put_with_failed_acknowledgement_never_publishes_or_deletes() {
    let server = TestS3::new().await;
    let fixture = TestApp::with_content(server.config.clone()).await;
    let saved = fixture
        .state
        .storage
        .create(b"<p>original</p>")
        .await
        .unwrap();
    server.objects.put_response.store(503, Ordering::Relaxed);
    assert!(
        fixture
            .state
            .storage
            .update(&saved.upload_id, b"<p>retry</p>")
            .await
            .is_err()
    );
    assert_eq!(server.objects.bytes.lock().unwrap().len(), 2);
    assert_eq!(
        fixture
            .state
            .storage
            .get(&saved.upload_id, None)
            .await
            .unwrap(),
        b"<p>original</p>"
    );
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
    server.objects.put_response.store(0, Ordering::Relaxed);
    let first = fixture
        .state
        .storage
        .update(&saved.upload_id, b"<p>retry</p>")
        .await
        .unwrap();
    let retry = fixture
        .state
        .storage
        .update(&saved.upload_id, b"<p>retry</p>")
        .await
        .unwrap();
    assert_eq!(first.version.created_at, retry.version.created_at);
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

#[tokio::test]
async fn binding_rejects_different_locations_and_credentials_can_rotate() {
    let server = TestS3::new().await;
    let fixture = TestApp::with_content(server.config.clone()).await;
    let mut config = fixture.config.clone();
    config.content.s3_bucket = Some("other-bucket".into());
    assert!(matches!(
        config.open(fixture.dir.path()).await,
        Err(StoreError::Configuration(_))
    ));
    config.content = ContentConfig::default();
    assert!(matches!(
        config.open(fixture.dir.path()).await,
        Err(StoreError::Configuration(_))
    ));
    config = fixture.config.clone();
    config.content.s3_secret_access_key = Some("rotated-secret".into());
    let reopened = config.open(fixture.dir.path()).await.unwrap();
    assert!(reopened.key_ids().await.is_ok());
    server.objects.failure.store(403, Ordering::Relaxed);
    let (_, key) = fixture.state.keys.create().await.unwrap();
    assert!(fixture.state.keys.valid(&key).await.unwrap());
}

#[tokio::test]
async fn inline_history_stays_inline_when_s3_is_enabled() {
    let server = TestS3::new().await;
    let fixture = TestApp::with_content(ContentConfig::default()).await;
    let first = fixture
        .state
        .storage
        .create(b"<p>inline</p>")
        .await
        .unwrap();
    let mut config = fixture.config.clone();
    config.content = server.config.clone();
    let store = config.open(fixture.dir.path()).await.unwrap();
    store
        .update(&first.upload_id, b"<p>inline</p>")
        .await
        .unwrap();
    assert!(server.objects.bytes.lock().unwrap().is_empty());
    let second = store
        .update(&first.upload_id, b"<p>external</p>")
        .await
        .unwrap();
    assert_eq!(server.objects.bytes.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .get(&first.upload_id, Some(&first.version.content_hash))
            .await
            .unwrap(),
        b"<p>inline</p>"
    );
    assert_eq!(
        store.get(&first.upload_id, None).await.unwrap(),
        b"<p>external</p>"
    );
    assert_ne!(first.version.content_hash, second.version.content_hash);
}
