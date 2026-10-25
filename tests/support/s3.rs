use crate::content::{ContentConfig, ContentStorage};
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::put};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU16, Ordering},
};

#[derive(Default)]
pub struct Objects {
    pub bytes: Mutex<HashMap<String, Vec<u8>>>,
    pub failure: AtomicU16,
    pub delay_ms: AtomicU16,
    pub body_delay_ms: AtomicU16,
    pub put_response: AtomicU16,
}

pub struct TestS3 {
    pub config: ContentConfig,
    pub objects: Arc<Objects>,
    task: tokio::task::JoinHandle<()>,
}

impl TestS3 {
    pub async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let objects = Arc::new(Objects::default());
        let app = Router::new()
            .route("/{bucket}/{*key}", put(handle).get(handle))
            .with_state(objects.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = ContentConfig {
            content_storage: ContentStorage::S3,
            s3_endpoint: Some(endpoint),
            s3_bucket: Some("test-bucket".into()),
            s3_region: Some("garage".into()),
            s3_access_key_id: Some("test-access".into()),
            s3_secret_access_key: Some("test-secret".into()),
            s3_allow_http: Some(true),
            ..Default::default()
        };
        Self {
            config,
            objects,
            task,
        }
    }
}

impl Drop for TestS3 {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(
    State(objects): State<Arc<Objects>>,
    Path((_bucket, key)): Path<(String, String)>,
    request: Request,
) -> Response {
    let signed = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.starts_with("AWS4-HMAC-SHA256 "));
    if !signed {
        return StatusCode::FORBIDDEN.into_response();
    }
    let delay = objects.delay_ms.load(Ordering::Relaxed);
    tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
    let failure = objects.failure.load(Ordering::Relaxed);
    if failure != 0 {
        return (
            StatusCode::from_u16(failure).unwrap(),
            "private object store diagnostic",
        )
            .into_response();
    }
    if request.method() == axum::http::Method::PUT {
        let bytes = to_bytes(request.into_body(), 2 * crate::helpers::MAX_HTML_BYTES)
            .await
            .unwrap();
        objects.bytes.lock().unwrap().insert(key, bytes.to_vec());
        let response = objects.put_response.load(Ordering::Relaxed);
        if response != 0 {
            return StatusCode::from_u16(response).unwrap().into_response();
        }
        (
            StatusCode::OK,
            [(header::ETAG, "\"test-etag\"")],
            Body::empty(),
        )
            .into_response()
    } else {
        match objects.bytes.lock().unwrap().get(&key).cloned() {
            Some(bytes) => {
                let length = bytes.len().to_string();
                let delay = objects.body_delay_ms.load(Ordering::Relaxed);
                let body = Body::from_stream(futures_util::stream::once(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
                    Ok::<_, std::io::Error>(bytes)
                }));
                (
                    StatusCode::OK,
                    [
                        (header::LAST_MODIFIED, "Wed, 21 Oct 2015 07:28:00 GMT"),
                        (header::ETAG, "\"test-etag\""),
                        (header::CONTENT_LENGTH, length.as_str()),
                    ],
                    body,
                )
                    .into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
}
