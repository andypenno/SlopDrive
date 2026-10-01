use super::*;

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

#[test]
fn validates_html_boundaries_and_real_script_text() {
    for invalid in ["", "plain text", "<>", "<html><", "<html>\0</html>"] {
        assert!(validate_html(invalid).is_err(), "{invalid:?}");
    }
    assert!(validate_html(&"x".repeat(MAX_HTML_BYTES + 1)).is_err());
    assert!(validate_html("<html><script>const x = '<>';</script></html>").is_ok());
    assert!(validate_html("<p>fragment</p>").is_ok());
}

#[test]
fn tool_errors_return_client_messages_without_internal_details() {
    for (error, message) in [
        (
            AppError::BadRequest("invalid content".into()),
            "invalid content",
        ),
        (AppError::NotFound, "not found"),
        (AppError::Unauthorized, "unauthorized"),
        (
            AppError::Internal("private storage detail".into()),
            "internal error",
        ),
    ] {
        assert_eq!(
            tool_error(error),
            json!({"content":[{"type":"text","text":message}],"isError":true})
        );
    }
}
