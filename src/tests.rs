use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

struct TestApp {
    app: Router,
    key: String,
    state: AppState,
    dir: TempDir,
}

impl TestApp {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(dir.path().to_path_buf()).unwrap());
        let keys = Arc::new(KeyStore::new(dir.path().to_path_buf()).unwrap());
        let (_, key) = keys.create().unwrap();
        let state = AppState {
            storage,
            keys,
            base_url: Url::parse("https://pages.example/").unwrap(),
        };
        Self {
            app: router(state.clone()),
            key,
            state,
            dir,
        }
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 8 * MAX_HTML_BYTES)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    async fn json(&self, request: Request<Body>) -> (StatusCode, Value) {
        let (status, _, body) = self.send(request).await;
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn post_html(&self, uri: &str, html: &[u8]) -> (StatusCode, Value) {
        self.json(request(
            Method::POST,
            uri,
            Some(&self.key),
            Some("multipart/form-data; boundary=testboundary"),
            multipart(&[("file", "text/html", html)]),
        ))
        .await
    }

    async fn mcp(&self, payload: Value) -> (StatusCode, Value) {
        self.json(request(
            Method::POST,
            "/mcp",
            Some(&self.key),
            Some("application/json"),
            payload.to_string(),
        ))
        .await
    }

    async fn tool(&self, id: i32, name: &str, arguments: Value) -> Value {
        let (status, body) = self
            .mcp(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .await;
        assert_eq!(status, StatusCode::OK);
        body["result"].clone()
    }
}

fn request(
    method: Method,
    uri: &str,
    key: Option<&str>,
    content_type: Option<&str>,
    body: impl Into<Body>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder.body(body.into()).unwrap()
}

fn multipart(fields: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, content_type, content) in fields {
        body.extend_from_slice(
            format!(
                "--testboundary\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"page.html\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--testboundary--\r\n");
    body
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

#[tokio::test]
async fn auth_precedes_body_parsing_and_public_reads_need_no_key() {
    let fixture = TestApp::new();
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
    let fixture = TestApp::new();
    let first = b"<html>first</html>";
    let second = b"<html>second</html>";
    let (status, created) = fixture.post_html("/upload", first).await;
    assert_eq!(status, StatusCode::OK);
    let id = created["upload_id"].as_str().unwrap();
    let hash = created["content_hash"].as_str().unwrap();
    assert!(valid_id(id));
    assert_eq!(hash, storage::hash(first));
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
    let fixture = TestApp::new();
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
async fn mcp_protocol_discovery_and_all_tools() {
    let fixture = TestApp::new();
    let (status, initialized) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "slopbox");
    let (_, ping) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":2,"method":"ping"}))
        .await;
    assert_eq!(ping["result"], json!({}));
    let (_, listed) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
        .await;
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "upload_html",
            "update_html",
            "list_versions",
            "get_html_url"
        ]
    );
    for tool in listed["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    let upload = fixture
        .tool(4, "upload_html", json!({"content":"<html>first</html>"}))
        .await;
    assert_eq!(upload["isError"], false);
    let first = &upload["structuredContent"];
    let id = first["upload_id"].as_str().unwrap();
    assert_eq!(first["url"], format!("https://pages.example/{id}"));
    assert_eq!(
        serde_json::from_str::<Value>(upload["content"][0]["text"].as_str().unwrap()).unwrap(),
        *first
    );
    let updated = fixture
        .tool(
            5,
            "update_html",
            json!({"upload_id":id,"content":"<html>second</html>"}),
        )
        .await;
    assert_eq!(updated["isError"], false);
    assert_ne!(
        updated["structuredContent"]["content_hash"],
        first["content_hash"]
    );
    let versions = fixture
        .tool(6, "list_versions", json!({"upload_id":id}))
        .await;
    assert_eq!(
        versions["structuredContent"]["versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let latest_url = fixture
        .tool(7, "get_html_url", json!({"upload_id":id}))
        .await;
    assert_eq!(latest_url["structuredContent"]["url"], first["url"]);
    let historical_url = fixture
        .tool(
            8,
            "get_html_url",
            json!({"upload_id":id,"version_hash":first["content_hash"]}),
        )
        .await;
    assert_eq!(
        historical_url["structuredContent"]["url"],
        first["versioned_url"]
    );
}

#[tokio::test]
async fn mcp_protocol_and_tool_errors_are_reported() {
    let fixture = TestApp::new();
    let (status, parse_error) = fixture
        .json(request(
            Method::POST,
            "/mcp",
            Some(&fixture.key),
            Some("application/json"),
            "{",
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_error["error"]["code"], -32700);
    let (_, invalid) = fixture.mcp(json!({"id":1,"method":"ping"})).await;
    assert_eq!(invalid["error"]["code"], -32600);
    let (_, missing_method) = fixture.mcp(json!({"jsonrpc":"2.0","id":1})).await;
    assert_eq!(missing_method["error"]["code"], -32600);
    let (_, unknown) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"missing"}))
        .await;
    assert_eq!(unknown["error"]["code"], -32601);
    let (status, _, body) = fixture
        .send(request(
            Method::POST,
            "/mcp",
            Some(&fixture.key),
            Some("application/json"),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body.is_empty());
    let (_, missing_name) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}}))
        .await;
    assert_eq!(missing_name["error"]["code"], -32602);
    let (_, unknown_tool) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_all"}}))
        .await;
    assert_eq!(unknown_tool["error"]["code"], -32602);
    let (_, missing_content) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"upload_html"}}))
        .await;
    assert_eq!(missing_content["error"]["code"], -32602);
    let bad_html = fixture
        .tool(3, "upload_html", json!({"content":"plain text"}))
        .await;
    assert_eq!(bad_html["isError"], true);
    let missing_id = "0".repeat(32);
    for name in ["update_html", "list_versions", "get_html_url"] {
        let mut arguments = json!({"upload_id":missing_id});
        if name == "update_html" {
            arguments["content"] = json!("<html>x</html>");
        }
        let result = fixture.tool(4, name, arguments).await;
        assert_eq!(result["isError"], true, "{name}");
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
async fn mcp_allows_escaped_html_near_the_content_limit() {
    let fixture = TestApp::new();
    let content = format!("<html>{}</html>", "\"".repeat(800_000));
    let result = fixture
        .tool(1, "upload_html", json!({"content":content}))
        .await;
    assert_eq!(result["isError"], false);
    assert!(valid_id(
        result["structuredContent"]["upload_id"].as_str().unwrap()
    ));
}

#[tokio::test]
async fn exact_content_limit_counts_utf8_bytes_and_is_publicly_readable() {
    let fixture = TestApp::new();
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
    assert_eq!(
        std::fs::read_dir(fixture.dir.path().join("uploads"))
            .unwrap()
            .count(),
        1
    );
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
async fn malformed_multipart_headers_and_truncated_fields_are_rejected() {
    let fixture = TestApp::new();
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
    let fixture = TestApp::new();
    for value in [
        HeaderValue::from_static("Basic key"),
        HeaderValue::from_str(&format!("bearer {}", fixture.key)).unwrap(),
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
async fn mcp_wrong_required_argument_types_and_unknown_versions_are_rejected() {
    let fixture = TestApp::new();
    for (name, arguments) in [
        ("upload_html", json!(null)),
        ("upload_html", json!([])),
        ("upload_html", json!({"content":1})),
        ("update_html", json!({"content":"<p>x</p>"})),
        (
            "update_html",
            json!({"upload_id":false,"content":"<p>x</p>"}),
        ),
        ("update_html", json!({"upload_id":"0".repeat(32)})),
        ("list_versions", json!({"upload_id":null})),
        ("get_html_url", json!({"upload_id":[]})),
    ] {
        let (_, response) = fixture.mcp(json!({"jsonrpc":"2.0","id":"typed","method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
        assert_eq!(response["id"], "typed");
        assert_eq!(response["error"]["code"], -32602, "{name}");
        assert!(response.get("result").is_none());
    }
    let (_, created) = fixture.post_html("/upload", b"<p>x</p>").await;
    for version_hash in ["bad".to_owned(), "0".repeat(64)] {
        let result = fixture
            .tool(
                2,
                "get_html_url",
                json!({"upload_id":created["upload_id"],"version_hash":version_hash}),
            )
            .await;
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "not found");
    }
}

#[tokio::test]
async fn mcp_discovery_advertises_complete_schemas_and_notifications_do_not_write() {
    let fixture = TestApp::new();
    let (_, initialized) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":null,"method":"initialize"}))
        .await;
    assert_eq!(initialized["id"], Value::Null);
    assert_eq!(
        initialized["result"]["capabilities"],
        json!({"tools":{"listChanged":false}})
    );
    assert_eq!(
        initialized["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    let (_, listed) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await;
    let required = [
        json!(["content"]),
        json!(["upload_id", "content"]),
        json!(["upload_id"]),
        json!(["upload_id"]),
    ];
    let properties = [
        json!({"content":{"type":"string"}}),
        json!({"upload_id":{"type":"string"},"content":{"type":"string"}}),
        json!({"upload_id":{"type":"string"}}),
        json!({"upload_id":{"type":"string"},"version_hash":{"type":"string"}}),
    ];
    for (index, tool) in listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(tool["inputSchema"]["required"], required[index]);
        assert_eq!(tool["inputSchema"]["properties"], properties[index]);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    let (status, _, body) = fixture.send(request(Method::POST, "/mcp", Some(&fixture.key), Some("application/json"), json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"upload_html","arguments":{"content":"<p>x</p>"}}}).to_string())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body.is_empty());
    assert_eq!(
        std::fs::read_dir(fixture.dir.path().join("uploads"))
            .unwrap()
            .count(),
        0
    );
    for invalid in [
        json!(null),
        json!([]),
        json!({"jsonrpc":"2.0","id":1,"method":2}),
    ] {
        let (_, response) = fixture.mcp(invalid).await;
        assert_eq!(response["error"]["code"], -32600);
    }
}
