use crate::storage::{Saved, StoreError, Version};
use crate::{AppError, AppState, UploadResponse, VersionResponse};
use axum::Json;
use axum::extract::Multipart;
use axum::http::{HeaderMap, header};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(crate) const MAX_HTML_BYTES: usize = 1024 * 1024;

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

pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StoreError> {
    let directory = File::open(path.parent().ok_or(StoreError::NotFound)?)?;
    let temp = path.with_extension(format!("tmp-{}", random_hex(8)?));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        directory.sync_all().map_err(StoreError::CommitUncertain)
    })();
    drop(file);
    if result.is_err()
        && let Err(error) = fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "temporary write cleanup failed");
    }
    result
}

pub(crate) fn durable_directory(path: &Path) -> Result<(), StoreError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if path.is_dir() {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(path);
        return File::open(parent)?
            .sync_all()
            .map_err(StoreError::CommitUncertain);
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    durable_directory(parent)?;
    let directory = File::open(if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    })?;
    match fs::create_dir(path) {
        Ok(()) => directory.sync_all().map_err(StoreError::CommitUncertain),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {
            directory.sync_all().map_err(StoreError::CommitUncertain)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn version_for(html: &[u8]) -> Version {
    Version {
        content_hash: hash(html),
        created_at: Utc::now(),
        size_bytes: html.len(),
    }
}

pub(crate) fn not_found(error: std::io::Error) -> StoreError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StoreError::NotFound
    } else {
        StoreError::Io(error)
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
    if blocking(state, move || Ok(keys.valid(&key))).await? {
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
