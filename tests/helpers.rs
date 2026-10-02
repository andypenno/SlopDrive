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
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
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

#[cfg(unix)]
#[test]
fn atomic_write_checks_directory_access_before_replacing_content() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("content");
    fs::write(&path, b"original").unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o300)).unwrap();
    let result = atomic_write(&path, b"replacement");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read(path).unwrap(), b"original");
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
        (AppError::Unauthorized, "invalid API key"),
        (
            AppError::Internal("private storage detail".into()),
            "internal error",
        ),
        (
            AppError::CommitUncertain("private storage detail".into()),
            "write may have committed; retry the same content",
        ),
    ] {
        assert_eq!(
            tool_error(error),
            json!({"content":[{"type":"text","text":message}],"isError":true})
        );
    }
}

#[tokio::test]
async fn cancelling_a_waiter_keeps_its_running_blocking_job_admitted() {
    let fixture = crate::test_helpers::TestApp::new();
    let state = fixture.state;
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        blocking(&task_state, move || {
            started.send(()).unwrap();
            finish
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            Ok(())
        })
        .await
    });
    ready.await.unwrap();
    task.abort();
    assert_eq!(state.jobs.available_permits(), 15);
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state.jobs.available_permits() != 16 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn element_detection_ignores_comments_and_accepts_later_elements() {
    for invalid in [
        "<!-- <p>comment</p> -->",
        "<!doctype html>",
        "<123>text</123>",
    ] {
        assert!(validate_html(invalid).is_err(), "{invalid}");
    }
    assert!(validate_html("<!-- first --><p>element</p><!-- last -->").is_ok());
}

#[cfg(unix)]
#[test]
fn directory_initialization_rechecks_existing_parent_durability() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("keys");
    durable_directory(&path).unwrap();
    let parent = path.parent().unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o300)).unwrap();
    let result = durable_directory(&path);
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    durable_directory(&path).unwrap();
    assert!(path.is_dir());
}
