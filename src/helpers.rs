use crate::storage::{Saved, StoreError, Version};
use crate::{AppError, AppState, UploadResponse, VersionResponse};
use axum::Json;
use axum::extract::Multipart;
use axum::http::{HeaderMap, header};
use chrono::SubsecRound;
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const MAX_HTML_BYTES: usize = 1024 * 1024;

pub(crate) fn object_key(prefix: &str, namespace: &str, id: &str, hash: &str) -> String {
    let key = format!("{namespace}/{id}/{hash}.html");
    if prefix.is_empty() {
        key
    } else {
        format!("{prefix}/{key}")
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub(crate) fn random_hex(bytes: usize) -> Result<String, StoreError> {
    let mut data = vec![0u8; bytes];
    getrandom::fill(&mut data).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(hex::encode(data))
}

pub(crate) fn version_for(html: &[u8]) -> Version {
    Version {
        content_hash: hash(html),
        created_at: Utc::now().trunc_subsecs(6),
        size_bytes: html.len(),
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
    state: &AppState,
    operation: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let permit = state
        .jobs
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?
}

pub(crate) async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, key)| key.trim_start_matches(' '))
        .ok_or(AppError::Unauthorized)?
        .to_owned();
    let keys = state.keys.clone();
    if database_job(state, async move { Ok(keys.valid(&key).await?) }).await? {
        if let Some(origin) = headers.get(header::ORIGIN)
            && origin.as_bytes() != state.base_url.origin().ascii_serialization().as_bytes()
        {
            return Err(AppError::Forbidden);
        }
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

pub(crate) async fn database_job<T: Send + 'static>(
    state: &AppState,
    operation: impl std::future::Future<Output = Result<T, AppError>> + Send + 'static,
) -> Result<T, AppError> {
    let permit = state
        .jobs
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Internal("database queue closed".into()))?;
    tokio::spawn(async move {
        let _permit = permit;
        operation.await
    })
    .await
    .map_err(|_| AppError::Internal("database job failed".into()))?
}

pub(crate) fn validate_html(content: &str) -> Result<(), AppError> {
    if content.is_empty() || content.len() > MAX_HTML_BYTES || content.contains('\0') {
        return Err(AppError::BadRequest(
            "HTML must be nonempty, valid UTF-8, and at most 1 MiB".into(),
        ));
    }
    let dom = tl::parse(content, tl::ParserOptions::default())
        .map_err(|_| AppError::BadRequest("invalid HTML".into()))?;
    if content.rfind('<') > content.rfind('>') {
        return Err(AppError::BadRequest("invalid HTML".into()));
    }
    let has_tag = dom
        .nodes()
        .iter()
        .filter_map(|node| node.as_tag())
        .any(|tag| {
            tag.name()
                .as_utf8_str()
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
        });
    if !has_tag {
        return Err(AppError::BadRequest("HTML must contain an element".into()));
    }
    Ok(())
}

pub(crate) fn upload_response(state: &AppState, saved: Saved) -> UploadResponse {
    let url = page_url(state, &saved.upload_id, None);
    let versioned_url = page_url(state, &saved.upload_id, Some(&saved.version.content_hash));
    UploadResponse {
        upload_id: saved.upload_id,
        content_hash: saved.version.content_hash,
        url,
        versioned_url,
    }
}

pub(crate) fn page_url(state: &AppState, id: &str, hash: Option<&str>) -> String {
    let mut url = state.base_url.join(id).expect("validated base URL and ID");
    if let Some(hash) = hash {
        url.query_pairs_mut().append_pair("q", hash);
    }
    url.to_string()
}

pub(crate) fn version_response(state: &AppState, id: &str, version: Version) -> VersionResponse {
    VersionResponse {
        url: page_url(state, id, None),
        versioned_url: page_url(state, id, Some(&version.content_hash)),
        content_hash: version.content_hash,
        created_at: version.created_at,
        size_bytes: version.size_bytes,
    }
}

pub(crate) fn required<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {name}"))
}

pub(crate) async fn multipart_html(mut multipart: Multipart) -> Result<String, AppError> {
    let mut html = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?
    {
        if field.name() != Some("file") || html.is_some() {
            return Err(AppError::BadRequest("expected one file field".into()));
        }
        if field.content_type() != Some("text/html") {
            return Err(AppError::BadRequest(
                "file must have content type text/html".into(),
            ));
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
        html = Some(
            String::from_utf8(bytes.to_vec())
                .map_err(|_| AppError::BadRequest("file must be UTF-8".into()))?,
        );
    }
    html.ok_or_else(|| AppError::BadRequest("missing file field".into()))
}

pub(crate) fn rpc_error(id: Value, code: i32, message: &str) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}

pub(crate) fn tool_error(error: AppError) -> Value {
    let message = error.public_message();
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

#[cfg(test)]
#[path = "../tests/helpers.rs"]
mod tests;
