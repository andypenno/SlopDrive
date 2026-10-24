use crate::test_helpers::{TestApp, multipart, request};
use crate::*;
use axum::body::Body;
use axum::http::Method;
use serde_json::Value;

#[tokio::test]
async fn invalid_recorded_timestamps_fail_closed_on_both_transports() {
    let fixture = TestApp::new().await;
    let content = b"<p>preserved</p>";
    let (_, saved) = fixture.post_html("/upload", content).await;
    let id = saved["upload_id"].as_str().unwrap();
    fixture.db.corrupt_timestamp(id).await;
    assert!(matches!(
        fixture.state.storage.versions(id).await,
        Err(StoreError::InvalidMetadata)
    ));
    assert!(matches!(
        fixture.state.storage.update(id, content).await,
        Err(StoreError::InvalidMetadata)
    ));
    let (status, error) = fixture
        .json(request(
            Method::GET,
            &format!("/upload/{id}/versions"),
            Some(&fixture.key),
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"], "internal error");
    for (name, arguments) in [
        ("list_versions", json!({"upload_id":id})),
        (
            "update_html",
            json!({"upload_id":id,"content":"<p>preserved</p>"}),
        ),
    ] {
        let result = fixture.tool(1, name, arguments).await;
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "internal error");
    }
    assert_eq!(
        fixture.post_html(&format!("/upload/{id}"), content).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(fixture.state.storage.get(id, None).await.unwrap(), content);
    assert_eq!(
        fixture
            .state
            .storage
            .get(id, saved["content_hash"].as_str())
            .await
            .unwrap(),
        content
    );
    assert_eq!(fixture.db.upload_count().await, 1);
}

#[tokio::test]
async fn database_write_failures_preserve_latest_and_leave_no_partial_uploads() {
    for mcp in [false, true] {
        let fixture = TestApp::new().await;
        let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
        let id = saved["upload_id"].as_str().unwrap();
        fixture
            .db
            .reject_versions(saved["content_hash"].as_str().unwrap())
            .await;
        if mcp {
            for (name, arguments) in [
                (
                    "update_html",
                    json!({"upload_id":id,"content":"<p>changed</p>"}),
                ),
                ("upload_html", json!({"content":"<p>changed</p>"})),
            ] {
                let result = fixture.tool(1, name, arguments).await;
                assert_eq!(result["isError"], true);
                assert_eq!(result["content"][0]["text"], "internal error");
            }
        } else {
            for path in [format!("/upload/{id}"), "/upload".to_owned()] {
                let (status, error) = fixture.post_html(&path, b"<p>changed</p>").await;
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert_eq!(error["error"], "internal error");
            }
        }
        assert_eq!(fixture.db.upload_count().await, 1);
        assert_eq!(fixture.state.storage.versions(id).await.unwrap().len(), 1);
        assert_eq!(
            fixture.state.storage.get(id, None).await.unwrap(),
            b"<p>original</p>"
        );
    }
}

#[tokio::test]
async fn corrupt_content_fails_closed_and_a_retry_repairs_bytes_without_new_metadata() {
    for mcp in [false, true] {
        let fixture = TestApp::new().await;
        let (_, saved) = fixture.post_html("/upload", b"<p>good</p>").await;
        let id = saved["upload_id"].as_str().unwrap();
        let timestamp = fixture.state.storage.versions(id).await.unwrap()[0].created_at;
        fixture.corrupt_content(id, b"<p>evil</p>").await;
        let (status, error) = fixture
            .json(request(
                Method::GET,
                &format!("/{id}"),
                None,
                None,
                Body::empty(),
            ))
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["error"], "internal error");
        if mcp {
            assert_eq!(
                fixture
                    .tool(
                        1,
                        "update_html",
                        json!({"upload_id":id,"content":"<p>good</p>"})
                    )
                    .await["isError"],
                false
            );
        } else {
            assert_eq!(
                fixture
                    .post_html(&format!("/upload/{id}"), b"<p>good</p>")
                    .await
                    .0,
                StatusCode::OK
            );
        }
        let history = fixture.state.storage.versions(id).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].created_at, timestamp);
        assert_eq!(
            fixture.state.storage.get(id, None).await.unwrap(),
            b"<p>good</p>"
        );
    }
}

#[tokio::test]
async fn key_revocation_and_database_outages_fail_closed_on_both_transports() {
    let fixture = TestApp::new().await;
    let id = &fixture.key[3..19];
    fixture.state.keys.revoke(id).await.unwrap();
    assert_eq!(
        fixture.post_html("/upload", b"<p>x</p>").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .mcp(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let fixture = TestApp::new().await;
    let (_, saved) = fixture.post_html("/upload", b"<p>x</p>").await;
    fixture.state.storage.close().await;
    let (status, error) = fixture
        .json(request(
            Method::GET,
            &format!("/{}", saved["upload_id"].as_str().unwrap()),
            None,
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"], "internal error");
    let (status, error) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"], "internal error");
}

#[tokio::test]
async fn protected_transports_accept_only_the_configured_browser_origin() {
    let fixture = TestApp::new().await;
    let (_, saved) = fixture.post_html("/upload", b"<p>x</p>").await;
    let id = saved["upload_id"].as_str().unwrap();
    for (method, uri, body) in [
        (Method::GET, format!("/upload/{id}/versions"), String::new()),
        (
            Method::POST,
            "/mcp".to_owned(),
            json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string(),
        ),
    ] {
        for origin in [
            None,
            Some("https://pages.example"),
            Some("https://untrusted.example"),
            Some("null"),
            Some("https://pages.example/extra"),
        ] {
            let mut req = request(
                method.clone(),
                &uri,
                Some(&fixture.key),
                Some("application/json"),
                body.clone(),
            );
            if let Some(origin) = origin {
                req.headers_mut()
                    .insert(header::ORIGIN, HeaderValue::from_static(origin));
            }
            let (status, _, _) = fixture.send(req).await;
            let expected = if origin.is_none() || origin == Some("https://pages.example") {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(status, expected, "{uri} {origin:?}");
        }
    }
}

#[tokio::test]
async fn bearer_scheme_and_challenges_follow_http_rules_on_both_transports() {
    let fixture = TestApp::new().await;
    let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
    let id = saved["upload_id"].as_str().unwrap();
    for (method, uri, body) in [
        (Method::GET, format!("/upload/{id}/versions"), String::new()),
        (
            Method::POST,
            "/mcp".to_owned(),
            json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string(),
        ),
    ] {
        for scheme in ["Bearer", "bearer", "bEaReR", "Bearer  "] {
            let mut req = request(
                method.clone(),
                &uri,
                None,
                Some("application/json"),
                body.clone(),
            );
            req.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("{scheme} {}", fixture.key)).unwrap(),
            );
            assert_eq!(fixture.send(req).await.0, StatusCode::OK, "{scheme} {uri}");
        }
        let (status, headers, _) = fixture
            .send(request(method, &uri, None, Some("application/json"), body))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(headers.get(header::WWW_AUTHENTICATE).unwrap(), "Bearer");
    }
}

#[tokio::test]
async fn uncertain_commit_has_a_shared_redacted_public_message() {
    let make_error = || {
        AppError::from(StoreError::CommitUncertain(std::io::Error::other(
            "private disk detail",
        )))
    };
    let response = make_error().into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let http: Value = serde_json::from_slice(&bytes).unwrap();
    let mcp = helpers::tool_error(make_error());
    assert_eq!(
        http["error"],
        "write may have committed; retry the same content"
    );
    assert_eq!(mcp["content"][0]["text"], http["error"]);
    assert_eq!(mcp["isError"], true);
}

#[tokio::test]
async fn body_limits_reject_without_writes_and_authentication_still_runs_first() {
    let fixture = TestApp::new().await;
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
    assert_eq!(fixture.db.upload_count().await, 0);
}

#[tokio::test]
async fn every_application_error_has_one_public_message_on_both_transports() {
    for index in 0..6 {
        let error = || match index {
            0 => AppError::BadRequest("invalid content".into()),
            1 => AppError::Unauthorized,
            2 => AppError::Forbidden,
            3 => AppError::NotFound,
            4 => AppError::Internal("private disk path".into()),
            _ => AppError::CommitUncertain("private disk path".into()),
        };
        let response = error().into_response();
        assert_eq!(
            response.status(),
            [
                StatusCode::BAD_REQUEST,
                StatusCode::UNAUTHORIZED,
                StatusCode::FORBIDDEN,
                StatusCode::NOT_FOUND,
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::SERVICE_UNAVAILABLE
            ][index]
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let http: Value = serde_json::from_slice(&bytes).unwrap();
        let mcp = helpers::tool_error(error());
        assert_eq!(mcp["content"][0]["text"], http["error"], "variant {index}");
        assert!(!std::str::from_utf8(&bytes).unwrap().contains("private"));
    }
}

#[tokio::test]
async fn successful_tools_match_http_metadata_and_their_text_results() {
    let fixture = TestApp::new().await;
    let created = fixture
        .tool(1, "upload_html", json!({"content":"<p>first</p>"}))
        .await;
    let id = created["structuredContent"]["upload_id"].as_str().unwrap();
    let updated = fixture
        .tool(
            2,
            "update_html",
            json!({"upload_id":id,"content":"<p>second</p>"}),
        )
        .await;
    for (tool, content) in [
        (&created, b"<p>first</p>".as_slice()),
        (&updated, b"<p>second</p>".as_slice()),
    ] {
        let (_, http) = fixture.post_html(&format!("/upload/{id}"), content).await;
        assert_eq!(tool["structuredContent"], http);
    }
    let history = fixture
        .tool(3, "list_versions", json!({"upload_id":id}))
        .await;
    let (_, http) = fixture
        .json(request(
            Method::GET,
            &format!("/upload/{id}/versions"),
            Some(&fixture.key),
            None,
            Body::empty(),
        ))
        .await;
    assert_eq!(history["structuredContent"], http);
    let persisted = fixture.state.storage.versions(id).await.unwrap();
    assert_eq!(persisted.len(), 2);
    for (actual, version) in http["versions"].as_array().unwrap().iter().zip(&persisted) {
        assert_eq!(
            *actual,
            json!({"content_hash":version.content_hash,"created_at":version.created_at,"size_bytes":version.size_bytes,"url":format!("https://pages.example/{id}"),"versioned_url":format!("https://pages.example/{id}?q={}",version.content_hash)})
        );
    }
    let url = fixture
        .tool(
            4,
            "get_html_url",
            json!({"upload_id":id,"version_hash":persisted[0].content_hash}),
        )
        .await;
    assert_eq!(
        url["structuredContent"]["url"],
        http["versions"][0]["versioned_url"]
    );
    for result in [created, updated, history, url] {
        assert_eq!(result["isError"], false);
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            result["structuredContent"]
        );
    }
}

#[tokio::test]
async fn publisher_keys_have_instance_wide_authority_on_both_transports() {
    for mcp in [false, true] {
        let mut fixture = TestApp::new().await;
        let (_, saved) = fixture
            .post_html("/upload", b"<p>first publisher</p>")
            .await;
        let id = saved["upload_id"].as_str().unwrap();
        let (_, second_key) = fixture.state.keys.create().await.unwrap();
        fixture.key = second_key;
        if mcp {
            assert_eq!(
                fixture
                    .tool(
                        1,
                        "update_html",
                        json!({"upload_id":id,"content":"<p>second publisher</p>"})
                    )
                    .await["isError"],
                false
            );
            assert_eq!(
                fixture
                    .tool(2, "list_versions", json!({"upload_id":id}))
                    .await["structuredContent"]["versions"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        } else {
            assert_eq!(
                fixture
                    .post_html(&format!("/upload/{id}"), b"<p>second publisher</p>")
                    .await
                    .0,
                StatusCode::OK
            );
            let (status, history) = fixture
                .json(request(
                    Method::GET,
                    &format!("/upload/{id}/versions"),
                    Some(&fixture.key),
                    None,
                    Body::empty(),
                ))
                .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(history["versions"].as_array().unwrap().len(), 2);
        }
    }
}
