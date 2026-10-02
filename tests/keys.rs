use super::*;

#[cfg(unix)]
#[test]
fn revocation_preflights_directory_durability_before_removing_a_key() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let keys = KeyStore::new(dir.path().to_path_buf()).unwrap();
    let (id, key) = keys.create().unwrap();
    fs::set_permissions(&keys.directory, fs::Permissions::from_mode(0o300)).unwrap();
    let result = keys.revoke(&id);
    fs::set_permissions(&keys.directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(keys.valid(&key));
}

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
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let name = std::ffi::OsString::from_vec(b"\xff.json".to_vec());
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

#[test]
fn authentication_faults_are_logged_without_keys_or_record_contents() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let capture = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || Capture(capture.clone()))
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::new(dir.path().to_path_buf()).unwrap();
        let (id, key) = store.create().unwrap();
        let path = store.directory.join(format!("{id}.json"));
        let corrupt = format!("{{\"hash\": {{\"secret\":\"{key}\"}}}}");
        fs::write(&path, &corrupt).unwrap();
        assert!(!store.valid(&key));
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("invalid key record"));
        assert!(logs.contains(&id));
        assert!(!logs.contains(&key));
        assert!(!logs.contains(&corrupt));
        fs::rename(&path, path.with_extension("original")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(!store.valid(&key));
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("key record read failed"));
        store.revoke(&"0".repeat(16)).unwrap_err();
        assert!(!store.valid(&format!("sb_{}_{}", "0".repeat(16), "0".repeat(64))));
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(!logs.contains(&format!("key_id={}", "0".repeat(16))));
    }
}
