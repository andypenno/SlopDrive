use crate::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

pub(crate) struct TestApp {
    pub(crate) app: Router,
    pub(crate) key: String,
    pub(crate) state: AppState,
    pub(crate) dir: TempDir,
}

impl TestApp {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemStorage::new(dir.path().to_path_buf()).unwrap());
        let keys = Arc::new(KeyStore::new(dir.path().to_path_buf()).unwrap());
        let (_, key) = keys.create().unwrap();
        let state = AppState {
            storage,
            keys,
            jobs: Arc::new(tokio::sync::Semaphore::new(16)),
            base_url: Url::parse("https://pages.example/").unwrap(),
        };
        Self {
            app: router(state.clone()),
            key,
            state,
            dir,
        }
    }

    pub(crate) async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 8 * MAX_HTML_BYTES)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    pub(crate) async fn json(&self, request: Request<Body>) -> (StatusCode, Value) {
        let (status, _, body) = self.send(request).await;
        (status, serde_json::from_slice(&body).unwrap())
    }

    pub(crate) async fn post_html(&self, uri: &str, html: &[u8]) -> (StatusCode, Value) {
        self.json(request(
            Method::POST,
            uri,
            Some(&self.key),
            Some("multipart/form-data; boundary=testboundary"),
            multipart(&[("file", "text/html", html)]),
        ))
        .await
    }

    pub(crate) async fn mcp(&self, payload: Value) -> (StatusCode, Value) {
        self.json(request(
            Method::POST,
            "/mcp",
            Some(&self.key),
            Some("application/json"),
            payload.to_string(),
        ))
        .await
    }

    pub(crate) async fn tool(&self, id: i32, name: &str, arguments: Value) -> Value {
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

pub(crate) fn request(
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

pub(crate) fn multipart(fields: &[(&str, &str, &[u8])]) -> Vec<u8> {
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
