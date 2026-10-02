use crate::test_helpers::{TestApp, multipart, request};
use crate::*;
use axum::body::Body;
use axum::http::Method;
use serde_json::Value;

#[tokio::test]
async fn protected_transports_accept_only_the_configured_browser_origin() {
    let fixture = TestApp::new();
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
    let fixture = TestApp::new();
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
async fn failed_content_writes_leave_no_temporary_files_over_both_transports() {
    for mcp in [false, true] {
        let fixture = TestApp::new();
        let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
        let id = saved["upload_id"].as_str().unwrap();
        let directory = fixture.dir.path().join("uploads").join(id);
        let content = "<p>replacement</p>";
        std::fs::create_dir(directory.join(format!("{}.html", helpers::hash(content.as_bytes()))))
            .unwrap();
        if mcp {
            let result = fixture
                .tool(1, "update_html", json!({"upload_id":id,"content":content}))
                .await;
            assert_eq!(result["isError"], true);
        } else {
            assert_eq!(
                fixture
                    .post_html(&format!("/upload/{id}"), content.as_bytes())
                    .await
                    .0,
                StatusCode::INTERNAL_SERVER_ERROR
            );
        }
        let names: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            names
                .iter()
                .all(|name| !name.to_string_lossy().contains(".tmp-")),
            "{names:?}"
        );
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

#[cfg(unix)]
#[tokio::test]
async fn directory_access_failure_preserves_latest_over_both_transports() {
    use std::os::unix::fs::PermissionsExt;
    for mcp in [false, true] {
        let fixture = TestApp::new();
        let (_, first) = fixture.post_html("/upload", b"<p>first</p>").await;
        let id = first["upload_id"].as_str().unwrap();
        fixture
            .post_html(&format!("/upload/{id}"), b"<p>second</p>")
            .await;
        let directory = fixture.dir.path().join("uploads").join(id);
        let metadata = std::fs::read(directory.join("metadata.json")).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o300)).unwrap();
        let failed = if mcp {
            fixture
                .tool(
                    1,
                    "update_html",
                    json!({"upload_id":id,"content":"<p>first</p>"}),
                )
                .await["isError"]
                == true
        } else {
            fixture
                .post_html(&format!("/upload/{id}"), b"<p>first</p>")
                .await
                .0
                == StatusCode::INTERNAL_SERVER_ERROR
        };
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(failed);
        assert_eq!(
            std::fs::read(directory.join("metadata.json")).unwrap(),
            metadata
        );
        let (status, _, content) = fixture
            .send(request(
                Method::GET,
                &format!("/{id}"),
                None,
                None,
                Body::empty(),
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content, b"<p>second</p>");
    }
}

#[tokio::test]
async fn retries_and_restores_repair_missing_content_over_both_transports() {
    for mcp in [false, true] {
        for historical in [false, true] {
            let fixture = TestApp::new();
            let content = b"<p>original</p>";
            let (_, created) = fixture.post_html("/upload", content).await;
            let id = created["upload_id"].as_str().unwrap();
            if historical {
                fixture
                    .post_html(&format!("/upload/{id}"), b"<p>newer</p>")
                    .await;
            }
            let path = fixture.dir.path().join("uploads").join(id);
            let hash = created["content_hash"].as_str().unwrap();
            std::fs::rename(path.join(format!("{hash}.html")), path.join("hidden.html")).unwrap();
            if mcp {
                let result = fixture
                    .tool(
                        1,
                        "update_html",
                        json!({"upload_id":id,"content":"<p>original</p>"}),
                    )
                    .await;
                assert_eq!(result["isError"], false);
                assert_eq!(result["structuredContent"], created);
            } else {
                let (status, updated) = fixture.post_html(&format!("/upload/{id}"), content).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(updated, created);
            }
            for uri in [format!("/{id}"), format!("/{id}?q={hash}")] {
                let (status, _, bytes) = fixture
                    .send(request(Method::GET, &uri, None, None, Body::empty()))
                    .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(bytes, content);
            }
            let (_, versions) = fixture
                .json(request(
                    Method::GET,
                    &format!("/upload/{id}/versions"),
                    Some(&fixture.key),
                    None,
                    Body::empty(),
                ))
                .await;
            assert_eq!(
                versions["versions"].as_array().unwrap().len(),
                1 + usize::from(historical)
            );
        }
    }
}

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
    std::fs::rename(&content_path, path.join("hidden.html")).unwrap();
    let metadata_path = path.join("metadata.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
    for latest in [
        created["content_hash"].clone(),
        json!("bad"),
        json!("0".repeat(64)),
    ] {
        if latest != created["content_hash"] {
            std::fs::rename(path.join("hidden.html"), &content_path).ok();
            std::fs::write(
                path.join(format!("{}.html", latest.as_str().unwrap())),
                b"<p>unlisted</p>",
            )
            .unwrap();
        }
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
        let missing = metadata["latest"] == created["content_hash"];
        let message = if missing {
            "not found"
        } else {
            "internal error"
        };
        assert_eq!(
            status,
            if missing {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        );
        assert_eq!(body, json!({"error":message}));
        let result = fixture
            .tool(1, "get_html_url", json!({"upload_id":id}))
            .await;
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], message);
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

#[tokio::test]
async fn inconsistent_metadata_is_rejected_before_listing_or_mutation() {
    for mutation in 0..7 {
        let fixture = TestApp::new();
        let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
        let id = saved["upload_id"].as_str().unwrap();
        let path = fixture
            .dir
            .path()
            .join("uploads")
            .join(id)
            .join("metadata.json");
        let mut metadata: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match mutation {
            0 => metadata["latest"] = json!("0".repeat(64)),
            1 => metadata["versions"][0]["content_hash"] = json!("not-a-hash"),
            2 => {
                let duplicate = metadata["versions"][0].clone();
                metadata["versions"].as_array_mut().unwrap().push(duplicate);
            }
            3 => metadata["versions"] = json!([]),
            4 => metadata["versions"][0]["size_bytes"] = json!(0),
            5 => metadata["versions"][0]["size_bytes"] = json!(MAX_HTML_BYTES + 1),
            _ => {
                let upper = saved["content_hash"].as_str().unwrap().to_ascii_uppercase();
                metadata["latest"] = json!(upper);
                metadata["versions"][0]["content_hash"] = json!(upper);
            }
        }
        let corrupt = metadata.to_string();
        std::fs::write(&path, &corrupt).unwrap();
        let (status, body) = fixture
            .json(request(
                Method::GET,
                &format!("/upload/{id}/versions"),
                Some(&fixture.key),
                None,
                Body::empty(),
            ))
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "mutation {mutation}"
        );
        assert_eq!(body["error"], "internal error");
        for (name, args) in [
            ("list_versions", json!({"upload_id":id})),
            (
                "update_html",
                json!({"upload_id":id,"content":"<p>replacement</p>"}),
            ),
        ] {
            let result = fixture.tool(1, name, args).await;
            assert_eq!(result["isError"], true, "{mutation} {name}");
            assert_eq!(result["content"][0]["text"], "internal error");
        }
        assert_eq!(
            fixture
                .post_html(&format!("/upload/{id}"), b"<p>replacement</p>")
                .await
                .0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn slow_key_reads_do_not_block_the_async_runtime() {
    use std::io::Write;
    use std::time::Duration;
    for mcp in [false, true] {
        let fixture = TestApp::new();
        let path = fixture
            .dir
            .path()
            .join("keys")
            .join(format!("{}.json", &fixture.key[3..19]));
        let record = std::fs::read(&path).unwrap();
        std::fs::rename(&path, path.with_extension("original")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let (heartbeat, receiver) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let responsive = receiver.recv_timeout(Duration::from_secs(1)).is_ok();
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .write_all(&record)
                .unwrap();
            responsive
        });
        let req = if mcp {
            request(
                Method::POST,
                "/mcp",
                Some(&fixture.key),
                Some("application/json"),
                json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string(),
            )
        } else {
            request(
                Method::POST,
                "/upload",
                Some(&fixture.key),
                Some("multipart/form-data; boundary=testboundary"),
                multipart(&[("file", "text/html", b"<p>x</p>")]),
            )
        };
        let (response, ()) = tokio::join!(fixture.send(req), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = heartbeat.send(());
        });
        assert_eq!(response.0, StatusCode::OK);
        assert!(writer.join().unwrap(), "authentication blocked the runtime");
    }
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
    let fixture = TestApp::new();
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
    let persisted = fixture.state.storage.versions(id).unwrap();
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
        let mut fixture = TestApp::new();
        let (_, saved) = fixture
            .post_html("/upload", b"<p>first publisher</p>")
            .await;
        let id = saved["upload_id"].as_str().unwrap();
        let (_, second_key) = fixture.state.keys.create().unwrap();
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

#[cfg(unix)]
#[tokio::test]
async fn latest_retries_still_preflight_and_confirm_directory_durability() {
    use std::os::unix::fs::PermissionsExt;
    for mcp in [false, true] {
        let fixture = TestApp::new();
        let (_, saved) = fixture.post_html("/upload", b"<p>same</p>").await;
        let id = saved["upload_id"].as_str().unwrap();
        let path = fixture.dir.path().join("uploads").join(id);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o300)).unwrap();
        let failed = if mcp {
            fixture
                .tool(
                    1,
                    "update_html",
                    json!({"upload_id":id,"content":"<p>same</p>"}),
                )
                .await["isError"]
                == true
        } else {
            fixture
                .post_html(&format!("/upload/{id}"), b"<p>same</p>")
                .await
                .0
                == StatusCode::INTERNAL_SERVER_ERROR
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(failed, "retry acknowledged without confirming durability");
        assert_eq!(fixture.state.storage.get(id, None).unwrap(), b"<p>same</p>");
    }
}

#[tokio::test]
async fn metadata_publication_failures_clean_unreachable_data_on_both_transports() {
    for mcp in [false, true] {
        let fixture = TestApp::new();
        let fail = || {
            StoreError::Io(std::io::Error::other(
                "injected metadata publication failure",
            ))
        };
        *fixture.state.storage.metadata_failure.lock().unwrap() = Some(fail());
        if mcp {
            assert_eq!(
                fixture
                    .tool(1, "upload_html", json!({"content":"<p>failed</p>"}))
                    .await["isError"],
                true
            );
        } else {
            assert_eq!(
                fixture.post_html("/upload", b"<p>failed</p>").await.0,
                StatusCode::INTERNAL_SERVER_ERROR
            );
        }
        assert_eq!(
            std::fs::read_dir(fixture.dir.path().join("uploads"))
                .unwrap()
                .count(),
            0
        );
        let (_, saved) = fixture.post_html("/upload", b"<p>original</p>").await;
        let id = saved["upload_id"].as_str().unwrap();
        *fixture.state.storage.metadata_failure.lock().unwrap() = Some(fail());
        if mcp {
            assert_eq!(
                fixture
                    .tool(
                        2,
                        "update_html",
                        json!({"upload_id":id,"content":"<p>unpublished</p>"})
                    )
                    .await["isError"],
                true
            );
        } else {
            assert_eq!(
                fixture
                    .post_html(&format!("/upload/{id}"), b"<p>unpublished</p>")
                    .await
                    .0,
                StatusCode::INTERNAL_SERVER_ERROR
            );
        }
        let directory = fixture.dir.path().join("uploads").join(id);
        assert!(
            !directory
                .join(format!("{}.html", helpers::hash(b"<p>unpublished</p>")))
                .exists()
        );
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 2);
        assert_eq!(
            fixture.state.storage.get(id, None).unwrap(),
            b"<p>original</p>"
        );
        assert_eq!(fixture.state.storage.versions(id).unwrap().len(), 1);
    }
}
