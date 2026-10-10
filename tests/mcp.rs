use crate::test_helpers::{TestApp, request};
use crate::*;
use axum::http::Method;
use serde_json::Value;

#[tokio::test]
async fn mcp_protocol_discovery_and_all_tools() {
    let fixture = TestApp::new().await;
    let (status, initialized) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}))
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
    let fixture = TestApp::new().await;
    let (status, parse_error) = fixture
        .json(request(
            Method::POST,
            "/mcp",
            Some(&fixture.key),
            Some("application/json"),
            "{",
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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
async fn mcp_allows_escaped_html_near_the_content_limit() {
    let fixture = TestApp::new().await;
    let content = format!("<html>{}</html>", "\"".repeat(800_000));
    let result = fixture
        .tool(1, "upload_html", json!({"content":content}))
        .await;
    assert_eq!(result["isError"], false);
    let id = result["structuredContent"]["upload_id"].as_str().unwrap();
    assert!(valid_id(id));
    let (status, _, bytes) = fixture
        .send(request(
            Method::GET,
            &format!("/{id}"),
            None,
            None,
            axum::body::Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, content.as_bytes());
}

#[tokio::test]
async fn mcp_wrong_required_argument_types_and_unknown_versions_are_rejected() {
    let fixture = TestApp::new().await;
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
    let fixture = TestApp::new().await;
    let (_, initialized) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":null,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}))
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
    assert_eq!(fixture.db.upload_count().await, 0);
    for invalid in [
        json!(null),
        json!([]),
        json!({"jsonrpc":"2.0","id":1,"method":2}),
    ] {
        let (_, response) = fixture.mcp(invalid).await;
        assert_eq!(response["error"]["code"], -32600);
    }
}

#[tokio::test]
async fn mcp_rejects_undeclared_tool_arguments() {
    let fixture = TestApp::new().await;
    let (_, response) = fixture.mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"upload_html","arguments":{"content":"<p>x</p>","typo":1}}})).await;
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(fixture.db.upload_count().await, 0);
}

#[tokio::test]
async fn mcp_argument_validation_applies_to_every_tool_and_field() {
    let fixture = TestApp::new().await;
    let (_, created) = fixture.post_html("/upload", b"<p>x</p>").await;
    let id = created["upload_id"].as_str().unwrap();
    let tools = [
        ("upload_html", json!({"content":"<p>x</p>"})),
        ("update_html", json!({"upload_id":id,"content":"<p>x</p>"})),
        ("list_versions", json!({"upload_id":id})),
        (
            "get_html_url",
            json!({"upload_id":id,"version_hash":created["content_hash"]}),
        ),
    ];
    for (name, valid) in tools {
        assert_eq!(
            fixture.tool(1, name, valid.clone()).await["isError"],
            false,
            "{name}"
        );
        let mut invalid_cases = vec![json!(null), json!([]), json!(1), json!("bad")];
        let mut extra = valid.clone();
        extra["typo"] = json!("unexpected");
        invalid_cases.push(extra);
        for field in valid.as_object().unwrap().keys() {
            if field != "version_hash" {
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(field);
                invalid_cases.push(missing);
            }
            for value in [json!(null), json!(false), json!(42), json!([]), json!({})] {
                let mut invalid = valid.clone();
                invalid[field] = value;
                invalid_cases.push(invalid);
            }
        }
        for arguments in invalid_cases {
            let (_, response) = fixture.mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
            assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
            assert!(response.get("result").is_none());
        }
    }
    assert_eq!(fixture.db.upload_count().await, 2);
    assert_eq!(fixture.state.storage.versions(id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn mcp_request_id_and_params_validation_accepts_and_rejects_json_types() {
    let fixture = TestApp::new().await;
    for id in [json!(null), json!(1), json!("request")] {
        for params in [json!({}), json!([])] {
            let (_, response) = fixture
                .mcp(json!({"jsonrpc":"2.0","id":id,"method":"ping","params":params}))
                .await;
            assert_eq!(response["id"], id);
            assert_eq!(response["result"], json!({}));
        }
    }
    for id in [json!(false), json!([]), json!({})] {
        let (_, response) = fixture
            .mcp(json!({"jsonrpc":"2.0","id":id,"method":"ping"}))
            .await;
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], -32600);
    }
    for params in [json!(null), json!(false), json!(1), json!("bad")] {
        let (_, response) = fixture
            .mcp(json!({"jsonrpc":"2.0","id":"request","method":"ping","params":params}))
            .await;
        assert_eq!(response["id"], "request");
        assert_eq!(response["error"]["code"], -32600);
    }
}

#[tokio::test]
async fn mcp_batches_use_sse_and_notification_only_posts_have_empty_acknowledgements() {
    let fixture = TestApp::new().await;
    let payload = json!([
        {"jsonrpc":"2.0","id":1,"method":"ping"},
        {"jsonrpc":"2.0","method":"notifications/initialized"},
        {"jsonrpc":"2.0","id":"unknown","method":"missing"}
    ]);
    let (status, headers, bytes) = fixture
        .send(request(
            Method::POST,
            "/mcp",
            Some(&fixture.key),
            Some("application/json"),
            payload.to_string(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
    let event = std::str::from_utf8(&bytes).unwrap();
    let responses: Value = serde_json::from_str(
        event
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(responses.as_array().unwrap().len(), 2);
    assert_eq!(responses[0], json!({"jsonrpc":"2.0","id":1,"result":{}}));
    assert_eq!(responses[1]["id"], "unknown");
    assert_eq!(responses[1]["error"]["code"], -32601);
    let (status, _, bytes) = fixture
        .send(request(
            Method::POST,
            "/mcp",
            Some(&fixture.key),
            Some("application/json"),
            json!([{"jsonrpc":"2.0","method":"notifications/initialized"}]).to_string(),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn mcp_rejects_unsolicited_responses_and_initialization_batches_before_writes() {
    let fixture = TestApp::new().await;
    let response = json!({"jsonrpc":"2.0","id":1,"result":{}});
    let upload = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"upload_html","arguments":{"content":"<p>x</p>"}}});
    for payload in [
        response.clone(),
        json!([response.clone()]),
        json!([upload.clone(), response]),
        json!([upload,{"jsonrpc":"2.0","id":3,"method":"initialize"}]),
        json!({"jsonrpc":"wrong","method":"notifications/initialized"}),
    ] {
        assert_eq!(
            fixture
                .send(request(
                    Method::POST,
                    "/mcp",
                    Some(&fixture.key),
                    Some("application/json"),
                    payload.to_string()
                ))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(fixture.db.upload_count().await, 0);
}

#[tokio::test]
async fn mcp_initialize_requires_typed_protocol_capabilities_and_client_info() {
    let fixture = TestApp::new().await;
    let valid = json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}});
    let mut invalid = vec![json!(null), json!([]), json!({})];
    for field in ["protocolVersion", "capabilities", "clientInfo"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        invalid.push(missing);
        let mut wrong = valid.clone();
        wrong[field] = json!(false);
        invalid.push(wrong);
    }
    for field in ["name", "version"] {
        let mut missing = valid.clone();
        missing["clientInfo"].as_object_mut().unwrap().remove(field);
        invalid.push(missing);
        let mut wrong = valid.clone();
        wrong["clientInfo"][field] = json!(1);
        invalid.push(wrong);
    }
    for params in invalid {
        let (_, response) = fixture
            .mcp(json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":params}))
            .await;
        assert!(response.get("result").is_none(), "{response}");
        assert_eq!(
            response["error"]["code"],
            if params.is_null() { -32600 } else { -32602 }
        );
    }
    let (_, missing) = fixture
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
        .await;
    assert_eq!(missing["error"]["code"], -32602);
    for protocol in ["2025-03-26", "2024-11-05"] {
        let mut params = valid.clone();
        params["protocolVersion"] = json!(protocol);
        let (_, response) = fixture
            .mcp(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":params}))
            .await;
        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
    }
}

#[tokio::test]
async fn mcp_borrowed_arguments_preserve_decoded_content_and_absent_defaults() {
    let fixture = TestApp::new().await;
    let content = "<p>\"quoted\"\n\t\u{00e9}\\slash</p>";
    let saved = fixture
        .tool(1, "upload_html", json!({"content":content}))
        .await;
    let id = saved["structuredContent"]["upload_id"].as_str().unwrap();
    assert_eq!(
        fixture.state.storage.get(id, None).await.unwrap(),
        content.as_bytes()
    );
    for params in [
        json!({"name":"upload_html"}),
        json!({"name":"upload_html","arguments":{}}),
    ] {
        let (_, result) = fixture
            .mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}))
            .await;
        assert_eq!(result["error"]["code"], -32602);
        assert_eq!(result["error"]["message"], "missing content");
    }
}

#[tokio::test]
async fn mcp_discovery_stays_fixed_after_tool_calls() {
    let fixture = TestApp::new().await;
    let discovery = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let (_, before) = fixture.mcp(discovery.clone()).await;
    fixture
        .tool(2, "upload_html", json!({"content":"<p>x</p>"}))
        .await;
    let (_, after) = fixture.mcp(discovery).await;
    assert_eq!(before, after);
    let (_, rejected) = fixture.mcp(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"upload_html","arguments":{"content":"<p>x</p>","extra":"bad"}}})).await;
    assert_eq!(rejected["error"]["code"], -32602);
}

#[tokio::test]
async fn mcp_limits_exact_decoded_utf8_bytes_and_preserves_content_on_rejection() {
    let fixture = TestApp::new().await;
    let content = format!("<p>{}x</p>", "é".repeat((MAX_HTML_BYTES - 8) / 2));
    assert_eq!(content.len(), MAX_HTML_BYTES);
    let saved = fixture
        .tool(1, "upload_html", json!({"content":content}))
        .await;
    assert_eq!(saved["isError"], false);
    let id = saved["structuredContent"]["upload_id"].as_str().unwrap();
    let over = format!("{content}x");
    for (name, args) in [
        ("upload_html", json!({"content":over})),
        ("update_html", json!({"upload_id":id,"content":over})),
    ] {
        assert_eq!(fixture.tool(2, name, args).await["isError"], true);
    }
    let (status, _, bytes) = fixture
        .send(request(
            Method::GET,
            &format!("/{id}"),
            None,
            None,
            axum::body::Body::empty(),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, content.as_bytes());
    assert_eq!(fixture.state.storage.versions(id).await.unwrap().len(), 1);
    assert_eq!(fixture.db.upload_count().await, 1);
}
