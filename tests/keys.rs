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
