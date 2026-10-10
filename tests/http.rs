use crate::test_helpers::{TestApp, multipart, request};
use crate::*;

#[tokio::test]
async fn published_pages_use_an_opaque_origin_with_scripts_enabled() {
    let fixture = TestApp::new().await;
    let (_, saved) = fixture
        .post_html(
            "/upload",
            b"<script>document.body.textContent='interactive'</script>",
        )
        .await;
    let id = saved["upload_id"].as_str().unwrap();
    let hash = saved["content_hash"].as_str().unwrap();
    for uri in [format!("/{id}"), format!("/{id}?q={hash}")] {
        let (status, headers, _) = fixture
            .send(request(Method::GET, &uri, None, None, Body::empty()))
            .await;
        assert_eq!(status, StatusCode::OK);
        let policy = headers
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        let tokens: Vec<_> = policy.split_ascii_whitespace().collect();
        assert_eq!(tokens.first(), Some(&"sandbox"));
        assert!(tokens.contains(&"allow-scripts"));
        assert!(!tokens.contains(&"allow-same-origin"));
    }
}
use axum::body::Body;
use axum::http::Method;

#[tokio::test]
async fn auth_precedes_body_parsing_and_public_reads_need_no_key() {
    let fixture = TestApp::new().await;
    let (status, _) = fixture
        .json(request(
            Method::POST,
            "/upload",
            None,
            Some("application/json"),
            "{}",
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = fixture
        .json(request(
            Method::POST,
            "/upload",
            Some("wrong"),
            Some("application/json"),
            "{}",
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = fixture
        .json(request(
            Method::GET,
            "/upload/00000000000000000000000000000000/versions",
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = fixture
        .json(request(
            Method::POST,
            "/mcp",
            None,
            Some("application/json"),
            "{}",
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, root) = fixture
        .send(request(Method::GET, "/", None, None, Body::empty()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8(root).unwrap().contains("SlopBox"));
    let (status, _) = fixture.post_html("/upload", b"<html>public</html>").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn http_upload_update_retry_versions_and_public_retrieval() {
    let fixture = TestApp::new().await;
    let first = b"<html>first</html>";
    let second = b"<html>second</html>";
    let (status, created) = fixture.post_html("/upload", first).await;
    assert_eq!(status, StatusCode::OK);
    let id = created["upload_id"].as_str().unwrap();
    let hash = created["content_hash"].as_str().unwrap();
    assert!(valid_id(id));
    assert_eq!(hash, helpers::hash(first));
    assert_eq!(created["url"], format!("https://pages.example/{id}"));
    assert_eq!(
        created["versioned_url"],
        format!("https://pages.example/{id}?q={hash}")
    );

    let (status, retried) = fixture.post_html(&format!("/upload/{id}"), first).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retried["content_hash"], hash);
    let (status, updated) = fixture.post_html(&format!("/upload/{id}"), second).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(updated["content_hash"], hash);
    let (status, retried) = fixture.post_html(&format!("/upload/{id}"), second).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retried["content_hash"], updated["content_hash"]);

    let (status, list) = fixture
        .json(request(
            Method::GET,
            &format!("/upload/{id}/versions"),
            Some(&fixture.key),
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["upload_id"], id);
    let versions = list["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["content_hash"], hash);
    assert_eq!(versions[0]["size_bytes"], first.len());
    assert!(versions[0]["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(versions[0]["url"], created["url"]);
    assert_eq!(versions[0]["versioned_url"], created["versioned_url"]);
    assert_eq!(versions[1]["versioned_url"], updated["versioned_url"]);

    let (status, headers, body) = fixture
        .send(request(
            Method::GET,
            &format!("/{id}"),
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, second);
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    let (status, headers, body) = fixture
        .send(request(
            Method::GET,
            &format!("/{id}?q={hash}"),
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, first);
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
}

#[tokio::test]
async fn http_rejects_unknown_pages_versions_and_bad_uploads() {
    let fixture = TestApp::new().await;
    let missing_id = "0".repeat(32);
    for uri in [
        "/not-an-id".to_owned(),
        format!("/{missing_id}"),
        format!("/{missing_id}?q={}", "0".repeat(64)),
    ] {
        let (status, _) = fixture
            .json(request(Method::GET, &uri, None, None, Body::empty()))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let (status, _) = fixture
        .post_html(&format!("/upload/{missing_id}"), b"<html>x</html>")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = fixture
        .json(request(
            Method::GET,
            &format!("/upload/{missing_id}/versions"),
            Some(&fixture.key),
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, created) = fixture.post_html("/upload", b"<html>x</html>").await;
    let id = created["upload_id"].as_str().unwrap();
    for uri in [
        format!("/{id}?q=bad"),
        format!("/{id}?q={}", "0".repeat(64)),
    ] {
        let (status, _) = fixture
            .json(request(Method::GET, &uri, None, None, Body::empty()))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }

    let bad_fields: Vec<Vec<u8>> = vec![
        multipart(&[]),
        multipart(&[("other", "text/html", b"<html>x</html>")]),
        multipart(&[("file", "text/plain", b"<html>x</html>")]),
        multipart(&[
            ("file", "text/html", b"<html>x</html>"),
            ("file", "text/html", b"<html>y</html>"),
        ]),
        multipart(&[("file", "text/html", &[0xff, 0xfe])]),
        multipart(&[("file", "text/html", b"")]),
        multipart(&[("file", "text/html", b"not html")]),
        multipart(&[("file", "text/html", b"<html><")]),
    ];
    for body in bad_fields {
        let (status, _) = fixture
            .json(request(
                Method::POST,
                "/upload",
                Some(&fixture.key),
                Some("multipart/form-data; boundary=testboundary"),
                body,
            ))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let too_large = format!("<html>{}</html>", "x".repeat(MAX_HTML_BYTES));
    let (status, _) = fixture.post_html("/upload", too_large.as_bytes()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn exact_content_limit_counts_utf8_bytes_and_is_publicly_readable() {
    let fixture = TestApp::new().await;
    let content = format!("<p>{}x</p>", "é".repeat((MAX_HTML_BYTES - 8) / 2));
    assert_eq!(content.len(), MAX_HTML_BYTES);
    let (status, created) = fixture.post_html("/upload", content.as_bytes()).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = fixture
        .send(request(
            Method::GET,
            &format!("/{}", created["upload_id"].as_str().unwrap()),
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(body, content.as_bytes());
    let (status, _) = fixture
        .post_html("/upload", format!("{content}x").as_bytes())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(fixture.db.upload_count().await, 1);
}

#[tokio::test]
async fn malformed_multipart_headers_and_truncated_fields_are_rejected() {
    let fixture = TestApp::new().await;
    for body in [
        b"--testboundary\r\nnot-a-header\r\n".as_slice(),
        b"--testboundary\r\nContent-Disposition: form-data; name=\"file\"\r\nContent-Type: text/html\r\n\r\n<p>x</p>".as_slice(),
        b"--testboundary\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\n<p>x</p>\r\n--testboundary--\r\n".as_slice(),
    ] {
        let (status, _) = fixture.json(request(Method::POST, "/upload", Some(&fixture.key), Some("multipart/form-data; boundary=testboundary"), body.to_vec())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _, _) = fixture
        .send(request(
            Method::POST,
            "/upload",
            Some(&fixture.key),
            Some("multipart/form-data"),
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn malformed_authorization_and_absent_listing_routes_reject_access() {
    let fixture = TestApp::new().await;
    for value in [
        HeaderValue::from_static("Basic key"),
        HeaderValue::from_str(&format!("Bearer {} ", fixture.key)).unwrap(),
        HeaderValue::from_bytes(b"Bearer \xff").unwrap(),
    ] {
        for (method, uri) in [
            (Method::POST, "/upload"),
            (Method::POST, "/upload/bad"),
            (Method::POST, "/mcp"),
            (Method::GET, "/upload/bad/versions"),
        ] {
            let mut req = request(method, uri, None, None, Body::empty());
            req.headers_mut()
                .insert(header::AUTHORIZATION, value.clone());
            let (status, _) = fixture.json(req).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        }
    }
    for uri in ["/upload", "/uploads", "/versions"] {
        let (status, _, _) = fixture
            .send(request(
                Method::GET,
                uri,
                Some(&fixture.key),
                None,
                Body::empty(),
            ))
            .await;
        assert!(
            matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn malformed_updates_preserve_latest_content_and_history() {
    let fixture = TestApp::new().await;
    let content = b"<p>original</p>";
    let (_, saved) = fixture.post_html("/upload", content).await;
    let id = saved["upload_id"].as_str().unwrap();
    for fields in [
        vec![("file", "text/html", &[0xff][..])],
        vec![("file", "text/plain", content.as_slice())],
    ] {
        let (status, _) = fixture
            .json(request(
                Method::POST,
                &format!("/upload/{id}"),
                Some(&fixture.key),
                Some("multipart/form-data; boundary=testboundary"),
                multipart(&fields),
            ))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(fixture.state.storage.get(id, None).await.unwrap(), content);
        assert_eq!(fixture.state.storage.versions(id).await.unwrap().len(), 1);
    }
}
