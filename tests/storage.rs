use super::*;
use crate::helpers::hash;
use std::sync::Arc;

#[cfg(unix)]
#[test]
fn creation_preflights_the_upload_parent_before_publishing_a_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let uploads = dir.path().join("uploads");
    fs::set_permissions(&uploads, fs::Permissions::from_mode(0o300)).unwrap();
    let result = store.create(b"<p>x</p>");
    fs::set_permissions(&uploads, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read_dir(uploads).unwrap().count(), 0);
}

#[test]
fn only_one_store_can_own_upload_writes_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let first = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let saved = first.create(b"<p>persisted</p>").unwrap();
    assert!(FilesystemStorage::new(dir.path().to_path_buf()).is_err());
    let keys = crate::keys::KeyStore::new(dir.path().to_path_buf()).unwrap();
    let (_, key) = keys.create().unwrap();
    assert!(keys.valid(&key));
    drop(first);
    let reopened = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    assert_eq!(
        reopened.get(&saved.upload_id, None).unwrap(),
        b"<p>persisted</p>"
    );
}

#[test]
fn existing_version_updates_repair_missing_or_corrupt_content() {
    for historical in [false, true] {
        for missing in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
            let content = b"<p>original</p>";
            let first = store.create(content).unwrap();
            if historical {
                store.update(&first.upload_id, b"<p>newer</p>").unwrap();
            }
            let path = store.upload_path(&first.upload_id).unwrap();
            let content_path = path.join(format!("{}.html", first.version.content_hash));
            if missing {
                fs::rename(&content_path, path.join("hidden.html")).unwrap();
            } else {
                fs::write(&content_path, b"corrupted").unwrap();
            }
            let repaired = store.update(&first.upload_id, content).unwrap();
            let recorded = store.versions(&first.upload_id).unwrap();
            assert_eq!(recorded[0].created_at, first.version.created_at);
            assert_eq!(recorded[0].size_bytes, first.version.size_bytes);
            assert_eq!(
                store
                    .get(&first.upload_id, Some(&first.version.content_hash))
                    .unwrap(),
                content
            );
            assert_eq!(store.get(&first.upload_id, None).unwrap(), content);
            assert_eq!(repaired.version.content_hash, first.version.content_hash);
            assert_eq!(repaired.version.created_at, first.version.created_at);
            assert_eq!(repaired.version.size_bytes, first.version.size_bytes);
            assert_eq!(
                store.versions(&first.upload_id).unwrap().len(),
                1 + usize::from(historical)
            );
        }
    }
}

#[test]
fn versions_survive_restart_and_latest_retry_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let first = store.create(b"<html>one</html>").unwrap();
    let second = store.update(&first.upload_id, b"<html>two</html>").unwrap();
    let recorded = store.versions(&first.upload_id).unwrap();
    for (version, saved, bytes) in [
        (&recorded[0], &first, b"<html>one</html>"),
        (&recorded[1], &second, b"<html>two</html>"),
    ] {
        assert_eq!(saved.version.content_hash, hash(bytes));
        assert_eq!(version.content_hash, saved.version.content_hash);
        assert_eq!(version.size_bytes, bytes.len());
        assert_eq!(version.created_at, saved.version.created_at);
    }
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
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for number in 0..8 {
            let barrier = &barrier;
            let store = Arc::clone(&store);
            let id = &first.upload_id;
            scope.spawn(move || {
                barrier.wait();
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
    drop(store);
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    assert_eq!(store.get(&first.upload_id, None).unwrap(), b"<p>one</p>");
    assert_eq!(store.versions(&first.upload_id).unwrap().len(), 2);
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
    fs::rename(
        path.join("hidden.html"),
        path.join(format!("{}.html", saved.version.content_hash)),
    )
    .unwrap();
    let mut metadata = store.read_metadata(&saved.upload_id).unwrap();
    for latest in ["bad".to_owned(), "0".repeat(64)] {
        fs::write(path.join(format!("{latest}.html")), b"<p>unlisted</p>").unwrap();
        metadata.latest = latest;
        store.save_metadata(&saved.upload_id, &metadata).unwrap();
        assert!(matches!(
            store.get(&saved.upload_id, None),
            Err(StoreError::InvalidMetadata)
        ));
    }
    metadata.latest = saved.version.content_hash.clone();
    metadata.versions.clear();
    store.save_metadata(&saved.upload_id, &metadata).unwrap();
    assert!(matches!(
        store.update(&saved.upload_id, b"<p>x</p>"),
        Err(StoreError::InvalidMetadata)
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
fn failed_content_update_preserves_latest_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let saved = store.create(b"<p>first</p>").unwrap();
    let new_content = b"<p>second</p>";
    let destination = store
        .upload_path(&saved.upload_id)
        .unwrap()
        .join(format!("{}.html", hash(new_content)));
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("sentinel"), b"keep").unwrap();
    assert!(matches!(
        store.update(&saved.upload_id, new_content),
        Err(StoreError::Io(_))
    ));
    assert_eq!(store.get(&saved.upload_id, None).unwrap(), b"<p>first</p>");
    let versions = store.versions(&saved.upload_id).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].content_hash, saved.version.content_hash);
    assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn metadata_save_errors_and_content_read_errors_preserve_existing_data() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let saved = store.create(b"<p>first</p>").unwrap();
    let metadata = store.read_metadata(&saved.upload_id).unwrap();
    assert!(matches!(
        store.save_metadata("../outside", &metadata),
        Err(StoreError::InvalidId)
    ));
    let path = store.upload_path(&saved.upload_id).unwrap();
    let content_path = path.join(format!("{}.html", saved.version.content_hash));
    fs::rename(&content_path, path.join("original.html")).unwrap();
    fs::create_dir(&content_path).unwrap();
    assert!(matches!(
        store.get(&saved.upload_id, None),
        Err(StoreError::Io(_))
    ));
    assert_eq!(
        fs::read(path.join("original.html")).unwrap(),
        b"<p>first</p>"
    );
    let metadata_path = path.join("metadata.json");
    fs::rename(&metadata_path, path.join("original.json")).unwrap();
    fs::create_dir(&metadata_path).unwrap();
    fs::write(metadata_path.join("sentinel"), b"keep").unwrap();
    assert!(matches!(
        store.save_metadata(&saved.upload_id, &metadata),
        Err(StoreError::Io(_))
    ));
    assert_eq!(fs::read(metadata_path.join("sentinel")).unwrap(), b"keep");
    let original: Metadata =
        serde_json::from_slice(&fs::read(path.join("original.json")).unwrap()).unwrap();
    assert_eq!(original.latest, saved.version.content_hash);
}

#[test]
fn concrete_storage_moves_between_threads_without_losing_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let saved = store.create(b"<p>shared</p>").unwrap();
    let store = std::thread::spawn(move || {
        assert_eq!(store.get(&saved.upload_id, None).unwrap(), b"<p>shared</p>");
        store
    })
    .join()
    .unwrap();
    assert!(FilesystemStorage::new(dir.path().to_path_buf()).is_err());
    drop(store);
    assert!(FilesystemStorage::new(dir.path().to_path_buf()).is_ok());
}

#[test]
fn failed_metadata_publication_cleans_only_unpublished_content() {
    for uncertain in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
        let fail = || {
            if uncertain {
                StoreError::CommitUncertain(std::io::Error::other("injected sync failure"))
            } else {
                StoreError::Io(std::io::Error::other("injected publication failure"))
            }
        };
        *store.metadata_failure.lock().unwrap() = Some(fail());
        assert!(store.create(b"<p>failed</p>").is_err());
        assert_eq!(
            fs::read_dir(dir.path().join("uploads")).unwrap().count(),
            usize::from(uncertain)
        );
        let first = store.create(b"<p>first</p>").unwrap();
        let path = store.upload_path(&first.upload_id).unwrap();
        let metadata = fs::read(path.join("metadata.json")).unwrap();
        let new = b"<p>new</p>";
        *store.metadata_failure.lock().unwrap() = Some(fail());
        assert!(store.update(&first.upload_id, new).is_err());
        assert_eq!(path.join(format!("{}.html", hash(new))).exists(), uncertain);
        assert_eq!(fs::read(path.join("metadata.json")).unwrap(), metadata);
        store.update(&first.upload_id, new).unwrap();
        *store.metadata_failure.lock().unwrap() = Some(fail());
        assert!(store.update(&first.upload_id, b"<p>first</p>").is_err());
        assert_eq!(
            store
                .get(&first.upload_id, Some(&first.version.content_hash))
                .unwrap(),
            b"<p>first</p>"
        );
    }
}

#[test]
fn error_conversions_preserve_the_original_failure() {
    let io: StoreError =
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "disk detail").into();
    assert!(
        matches!(io, StoreError::Io(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied && error.to_string() == "disk detail")
    );
    let json = serde_json::from_slice::<Metadata>(b"invalid")
        .err()
        .unwrap();
    let line = json.line();
    let column = json.column();
    let converted: StoreError = json.into();
    assert!(
        matches!(converted, StoreError::Corrupt(ref error) if error.line() == line && error.column() == column)
    );
}

#[test]
fn metadata_is_readable_with_or_without_a_legacy_upload_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemStorage::new(dir.path().to_path_buf()).unwrap();
    let saved = store.create(b"<p>first</p>").unwrap();
    let path = store
        .upload_path(&saved.upload_id)
        .unwrap()
        .join("metadata.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("upload_id");
    fs::write(&path, metadata.to_string()).unwrap();
    assert_eq!(store.get(&saved.upload_id, None).unwrap(), b"<p>first</p>");
    metadata["upload_id"] = serde_json::json!(saved.upload_id);
    fs::write(&path, metadata.to_string()).unwrap();
    store.update(&saved.upload_id, b"<p>second</p>").unwrap();
    let rewritten: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(rewritten.get("upload_id").is_none());
    assert_eq!(store.versions(&saved.upload_id).unwrap().len(), 2);
}
