use super::*;

#[tokio::test]
async fn database_lock_does_not_block_http_or_cancel_an_admitted_write() {
    use crate::test_helpers::request;
    let fixture = crate::test_helpers::TestApp::new().await;
    let lock = fixture.db.lock_writes().await;
    let state = fixture.state.clone();
    let storage = state.storage.clone();
    let (started, running) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        database_job(&state, async move {
            started.send(()).unwrap();
            Ok(storage.create(b"<p>database lock</p>").await?)
        })
        .await
    });
    running.await.unwrap();
    assert!(!waiter.is_finished());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        fixture.send(request(axum::http::Method::GET, "/", None, None, "")),
    )
    .await
    .unwrap();
    assert_eq!(response.0, axum::http::StatusCode::OK);
    assert_eq!(fixture.db.upload_count().await, 0);
    waiter.abort();
    assert!(waiter.await.err().unwrap().is_cancelled());
    assert_eq!(fixture.state.jobs.available_permits(), 15);
    lock.release().await;
    let drained = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.state.jobs.acquire_many(16),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fixture.db.upload_count().await, 1);
    drop(drained);
}

#[tokio::test]
async fn cancelled_database_waiters_keep_their_job_admitted_until_completion() {
    let fixture = crate::test_helpers::TestApp::new().await;
    let state = fixture.state.clone();
    let jobs = state.jobs.clone();
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let storage = state.storage.clone();
    let waiter = tokio::spawn(async move {
        database_job(&state, async move {
            started.send(()).unwrap();
            released.await.unwrap();
            storage.create(b"<p>detached write</p>").await?;
            Ok(())
        })
        .await
    });
    running.await.unwrap();
    assert_eq!(jobs.available_permits(), 15);
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(jobs.available_permits(), 15);
    release.send(()).unwrap();
    let drained = tokio::time::timeout(std::time::Duration::from_secs(5), jobs.acquire_many(16))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.db.upload_count().await, 1);
    drop(drained);
}

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
    let fixture = crate::test_helpers::TestApp::new().await;
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
