use crate::test_helpers::{TestApp, multipart, request};
use crate::*;
use axum::body::Body;
use axum::http::Method;
use serde_json::Value;

#[tokio::test]
async fn key_revocation_and_corrupt_storage_fail_closed() {
    let fixture = TestApp::new();
    let (status, created) = fixture.post_html("/upload", b"<html>x</html>").await;
    assert_eq!(status, StatusCode::OK);
    let id = created["upload_id"].as_str().unwrap();
    let metadata = fixture
        .dir
        .path()
        .join("uploads")
        .join(id)
        .join("metadata.json");
    std::fs::write(metadata, b"not json").unwrap();
    let (status, body) = fixture
        .json(request(
            Method::GET,
            &format!("/{id}"),
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "internal error");
    let result = fixture
        .tool(1, "get_html_url", json!({"upload_id":id}))
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], "internal error");

    let key_id = fixture.key[3..19].to_owned();
    fixture.state.keys.revoke(&key_id).unwrap();
    let (status, _) = fixture.post_html("/upload", b"<html>x</html>").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = fixture
        .json(request(
            Method::POST,
            "/mcp",
            Some(&fixture.key),
            Some("application/json"),
            "{}",
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn body_limits_reject_without_writes_and_authentication_still_runs_first() {
    let fixture = TestApp::new();
    for (uri, content_type, size) in [
        (
            "/upload",
            "multipart/form-data; boundary=testboundary",
            MAX_HTML_BYTES + 32 * 1024,
        ),
        ("/mcp", "application/json", 6 * MAX_HTML_BYTES + 32 * 1024),
    ] {
        for key in [None, Some(fixture.key.as_str())] {
            let body = if uri == "/upload" {
                let mut body = multipart(&[("file", "text/html", b"<p>x</p>")]);
                let index = body
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .unwrap();
                body.splice(
                    index..index,
                    format!("\r\nX-Padding: {}", "x".repeat(size)).bytes(),
                );
                body
            } else {
                "x".repeat(size).into_bytes()
            };
            let (status, _, response) = fixture
                .send(request(Method::POST, uri, key, Some(content_type), body))
                .await;
            if key.is_none() {
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
            } else if uri == "/mcp" {
                assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            } else {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                let error: Value = serde_json::from_slice(&response).unwrap();
                assert_ne!(
                    error["error"],
                    "HTML must be nonempty, valid UTF-8, and at most 1 MiB"
                );
            }
        }
    }
    assert_eq!(
        std::fs::read_dir(fixture.dir.path().join("uploads"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn missing_content_and_invalid_latest_metadata_fail_as_not_found() {
    let fixture = TestApp::new();
    let (_, created) = fixture.post_html("/upload", b"<p>x</p>").await;
    let id = created["upload_id"].as_str().unwrap();
    let path = fixture.dir.path().join("uploads").join(id);
    let content_path = path.join(format!(
        "{}.html",
        created["content_hash"].as_str().unwrap()
    ));
    std::fs::rename(content_path, path.join("hidden.html")).unwrap();
    let metadata_path = path.join("metadata.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
    for latest in [
        created["content_hash"].clone(),
        json!("bad"),
        json!("0".repeat(64)),
    ] {
        metadata["latest"] = latest;
        std::fs::write(&metadata_path, metadata.to_string()).unwrap();
        let (status, body) = fixture
            .json(request(
                Method::GET,
                &format!("/{id}"),
                None,
                None,
                Body::empty(),
            ))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, json!({"error":"not found"}));
        let result = fixture
            .tool(1, "get_html_url", json!({"upload_id":id}))
            .await;
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "not found");
    }
}

#[tokio::test]
async fn storage_write_and_read_failures_are_redacted_over_http_and_mcp() {
    let fixture = TestApp::new();
    let uploads = fixture.dir.path().join("uploads");
    std::fs::rename(&uploads, fixture.dir.path().join("original-uploads")).unwrap();
    std::fs::write(&uploads, b"keep").unwrap();
    let (status, response) = fixture.post_html("/upload", b"<p>x</p>").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response, json!({"error":"internal error"}));
    let response = fixture
        .tool(1, "upload_html", json!({"content":"<p>x</p>"}))
        .await;
    assert_eq!(response["isError"], true);
    assert_eq!(response["content"][0]["text"], "internal error");
    assert_eq!(std::fs::read(&uploads).unwrap(), b"keep");

    let fixture = TestApp::new();
    let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
    let id = saved["upload_id"].as_str().unwrap();
    let path = fixture.dir.path().join("uploads").join(id);
    let content_path = path.join(format!("{}.html", saved["content_hash"].as_str().unwrap()));
    std::fs::rename(&content_path, path.join("original.html")).unwrap();
    std::fs::create_dir(&content_path).unwrap();
    let (status, response) = fixture
        .json(request(
            Method::GET,
            &format!("/{id}"),
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response, json!({"error":"internal error"}));
    let response = fixture
        .tool(2, "get_html_url", json!({"upload_id":id}))
        .await;
    assert_eq!(response["isError"], true);
    assert_eq!(response["content"][0]["text"], "internal error");
    assert_eq!(
        std::fs::read(path.join("original.html")).unwrap(),
        b"<p>original</p>"
    );
}
