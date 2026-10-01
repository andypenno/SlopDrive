mod keys;
mod mcp;
mod storage;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use keys::KeyStore;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use storage::{FilesystemStorage, Saved, Storage, StoreError, Version, valid_hash, valid_id};
use url::Url;

const MAX_HTML_BYTES: usize = 1024 * 1024;

#[derive(Parser)]
#[command(name = "slopbox", about = "Single-file HTML host")]
struct Cli {
    #[arg(long, env = "STORAGE_PATH", default_value = "/data")]
    storage_path: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, env = "BASE_URL")]
        base_url: Url,
        #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:3000")]
        bind_addr: SocketAddr,
    },
    Keys {
        #[command(subcommand)]
        command: KeyCommand,
    },
}

#[derive(Subcommand)]
enum KeyCommand {
    Create,
    List,
    Revoke { id: String },
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub storage: Arc<dyn Storage>,
    pub keys: Arc<KeyStore>,
    pub base_url: Url,
}

#[derive(Debug)]
pub(crate) enum AppError {
    BadRequest(String),
    Unauthorized,
    NotFound,
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid API key".to_owned()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
            Self::Internal(message) => {
                tracing::error!(error = %message, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<StoreError> for AppError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::NotFound | StoreError::InvalidId => Self::NotFound,
            other => Self::Internal(other.to_string()),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    pub upload_id: String,
    pub content_hash: String,
    pub url: String,
    pub versioned_url: String,
}

#[derive(Serialize)]
pub(crate) struct VersionResponse {
    pub content_hash: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub size_bytes: usize,
    pub url: String,
    pub versioned_url: String,
}

#[derive(Serialize)]
pub(crate) struct VersionListResponse {
    pub upload_id: String,
    pub versions: Vec<VersionResponse>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Keys { command } => {
            let keys = KeyStore::new(cli.storage_path)?;
            match command {
                KeyCommand::Create => {
                    let (id, key) = keys.create()?;
                    println!("{id} {key}");
                }
                KeyCommand::List => {
                    for id in keys.list()? {
                        println!("{id}");
                    }
                }
                KeyCommand::Revoke { id } => keys.revoke(&id)?,
            }
        }
        Command::Serve {
            base_url,
            bind_addr,
        } => {
            if !matches!(base_url.scheme(), "http" | "https")
                || base_url.cannot_be_a_base()
                || base_url.query().is_some()
                || base_url.fragment().is_some()
                || base_url.path() != "/"
            {
                anyhow::bail!("BASE_URL must be an HTTP(S) origin ending in /");
            }
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let state = AppState {
                storage: Arc::new(FilesystemStorage::new(cli.storage_path.clone())?),
                keys: Arc::new(KeyStore::new(cli.storage_path)?),
                base_url,
            };
            let app = router(state);
            let listener = tokio::net::TcpListener::bind(bind_addr)
                .await
                .context("bind failed")?;
            tracing::info!(address = %bind_addr, "listening");
            axum::serve(listener, app).await.context("server failed")?;
        }
    }
    Ok(())
}

fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route(
            "/upload",
            post(create_upload).layer(DefaultBodyLimit::max(MAX_HTML_BYTES + 16 * 1024)),
        )
        .route(
            "/upload/{id}",
            post(update_upload).layer(DefaultBodyLimit::max(MAX_HTML_BYTES + 16 * 1024)),
        )
        .route("/upload/{id}/versions", get(list_versions_http))
        .route(
            "/mcp",
            post(mcp::handle).layer(DefaultBodyLimit::max(6 * MAX_HTML_BYTES + 16 * 1024)),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), auth_guard));
    Router::new()
        .route("/", get(root))
        .merge(protected)
        .route("/{id}", get(get_html))
        .with_state(state)
}

async fn root() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

pub(crate) fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    if state.keys.valid(key) {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

async fn auth_guard(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    Ok(next.run(request).await)
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
    let mut has_tag = false;
    for tag in dom.nodes().iter().filter_map(|node| node.as_tag()) {
        let name = tag.name().as_utf8_str();
        if name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
            has_tag = true;
        }
    }
    if !has_tag {
        return Err(AppError::BadRequest("HTML must contain an element".into()));
    }
    Ok(())
}

pub(crate) fn upload(
    state: &AppState,
    id: Option<&str>,
    content: &str,
) -> Result<UploadResponse, AppError> {
    validate_html(content)?;
    let saved = if let Some(id) = id {
        state.storage.update(id, content.as_bytes())?
    } else {
        state.storage.create(content.as_bytes())?
    };
    Ok(upload_response(state, saved))
}

fn upload_response(state: &AppState, saved: Saved) -> UploadResponse {
    let url = page_url(state, &saved.upload_id, None);
    let versioned_url = page_url(state, &saved.upload_id, Some(&saved.version.content_hash));
    UploadResponse {
        upload_id: saved.upload_id,
        content_hash: saved.version.content_hash,
        url,
        versioned_url,
    }
}

fn page_url(state: &AppState, id: &str, hash: Option<&str>) -> String {
    let mut url = state.base_url.join(id).expect("validated base URL and ID");
    if let Some(hash) = hash {
        url.query_pairs_mut().append_pair("q", hash);
    }
    url.to_string()
}

pub(crate) fn versions(state: &AppState, id: &str) -> Result<VersionListResponse, AppError> {
    let versions = state.storage.versions(id)?;
    Ok(VersionListResponse {
        upload_id: id.to_owned(),
        versions: versions
            .into_iter()
            .map(|version| version_response(state, id, version))
            .collect(),
    })
}

fn version_response(state: &AppState, id: &str, version: Version) -> VersionResponse {
    VersionResponse {
        url: page_url(state, id, None),
        versioned_url: page_url(state, id, Some(&version.content_hash)),
        content_hash: version.content_hash,
        created_at: version.created_at,
        size_bytes: version.size_bytes,
    }
}

pub(crate) fn get_page_url(
    state: &AppState,
    id: &str,
    hash: Option<&str>,
) -> Result<String, AppError> {
    state.storage.get(id, hash)?;
    Ok(page_url(state, id, hash))
}

async fn multipart_html(mut multipart: Multipart) -> Result<String, AppError> {
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

async fn create_upload(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<UploadResponse>, AppError> {
    Ok(Json(upload(
        &state,
        None,
        &multipart_html(multipart).await?,
    )?))
}

async fn update_upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    multipart: Multipart,
) -> Result<Json<UploadResponse>, AppError> {
    Ok(Json(upload(
        &state,
        Some(&id),
        &multipart_html(multipart).await?,
    )?))
}

async fn list_versions_http(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<VersionListResponse>, AppError> {
    Ok(Json(versions(&state, &id)?))
}

async fn get_html(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    if !valid_id(&id) {
        return Err(AppError::NotFound);
    }
    let hash = query.get("q").map(String::as_str);
    if hash.is_some_and(|hash| !valid_hash(hash)) {
        return Err(AppError::NotFound);
    }
    let content = state.storage.get(&id, hash)?;
    let mut response = content.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if hash.is_some() {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    Ok(response)
}

#[cfg(test)]
mod tests;
