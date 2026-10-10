mod database;
mod helpers;
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
use helpers::{
    MAX_HTML_BYTES, authorize, blocking, database_job, multipart_html, page_url, upload_response,
    valid_hash, valid_id, validate_html, version_response,
};
use keys::KeyStore;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use storage::{Storage, StoreError};
use url::Url;

#[derive(Parser)]
#[command(name = "slopbox", about = "Single-file HTML host")]
struct Cli {
    #[arg(long, env = "STORAGE_PATH", default_value = "/data")]
    storage_path: PathBuf,
    #[command(flatten)]
    database: database::DatabaseConfig,
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
    pub jobs: Arc<tokio::sync::Semaphore>,
}

#[derive(Debug)]
pub(crate) enum AppError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    NotFound,
    Internal(String),
    CommitUncertain(String),
}

impl AppError {
    fn public_message(&self) -> &str {
        match self {
            Self::BadRequest(message) => message,
            Self::Unauthorized => "invalid API key",
            Self::Forbidden => "untrusted origin",
            Self::NotFound => "not found",
            Self::Internal(message) => {
                tracing::error!(error = %message, "request failed");
                "internal error"
            }
            Self::CommitUncertain(message) => {
                tracing::error!(error = %message, "write durability uncertain");
                "write may have committed; retry the same content"
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::CommitUncertain(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        let message = self.public_message();
        let mut response = (status, Json(json!({ "error": message }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

impl From<StoreError> for AppError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::NotFound | StoreError::InvalidId => Self::NotFound,
            StoreError::CommitUncertain(error) => Self::CommitUncertain(error.to_string()),
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

fn main() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Keys { command } => {
            let keys = KeyStore::new(cli.database.open(&cli.storage_path).await?);
            match command {
                KeyCommand::Create => {
                    let (id, key) = keys.create().await?;
                    println!("{id} {key}");
                }
                KeyCommand::List => {
                    for id in keys.list().await? {
                        println!("{id}");
                    }
                }
                KeyCommand::Revoke { id } => keys.revoke(&id).await?,
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
                || !base_url.username().is_empty()
                || base_url.password().is_some()
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
            let database = cli.database.open(&cli.storage_path).await?;
            let state = AppState {
                storage: database.clone(),
                keys: Arc::new(KeyStore::new(database.clone())),
                base_url,
                jobs: Arc::new(tokio::sync::Semaphore::new(16)),
            };
            let jobs = state.jobs.clone();
            let shutdown = shutdown_signal()?;
            let app = router(state);
            let listener = tokio::net::TcpListener::bind(bind_addr)
                .await
                .context("bind failed")?;
            tracing::info!(address = %bind_addr, "listening");
            let (notify, started) = tokio::sync::oneshot::channel();
            let server = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown.await;
                    let _ = notify.send(());
                })
                .into_future();
            tokio::pin!(server);
            let completed = tokio::select! {
                result = &mut server => Some(result),
                _ = started => None,
            };
            tracing::info!("draining requests");
            tokio::time::timeout(Duration::from_secs(10), async {
                match completed {
                    Some(result) => result,
                    None => server.await,
                }
                .context("server failed")?;
                tracing::info!("draining admitted jobs");
                let _drained = jobs
                    .acquire_many_owned(16)
                    .await
                    .context("blocking queue closed")?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .context("shutdown drain timed out")??;
            database.close().await;
        }
    }
    Ok(())
}

fn shutdown_signal() -> Result<impl std::future::Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        Ok(async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::error!(%error, "shutdown signal failed");
            }
        })
    }
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

async fn auth_guard(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    authorize(&state, &headers).await?;
    Ok(next.run(request).await)
}

pub(crate) async fn upload(
    state: &AppState,
    id: Option<&str>,
    content: &str,
) -> Result<UploadResponse, AppError> {
    let owned = state.clone();
    let id = id.map(str::to_owned);
    let content = content.to_owned();
    let checked = content.clone();
    blocking(state, move || validate_html(&checked)).await?;
    database_job(state, async move {
        let saved = if let Some(id) = id {
            owned.storage.update(&id, content.as_bytes()).await?
        } else {
            owned.storage.create(content.as_bytes()).await?
        };
        Ok(upload_response(&owned, saved))
    })
    .await
}

pub(crate) async fn versions(state: &AppState, id: &str) -> Result<VersionListResponse, AppError> {
    let owned = state.clone();
    let id = id.to_owned();
    database_job(state, async move {
        let versions = owned.storage.versions(&id).await?;
        Ok(VersionListResponse {
            upload_id: id.clone(),
            versions: versions
                .into_iter()
                .map(|version| version_response(&owned, &id, version))
                .collect(),
        })
    })
    .await
}

pub(crate) async fn get_page_url(
    state: &AppState,
    id: &str,
    hash: Option<&str>,
) -> Result<String, AppError> {
    let owned = state.clone();
    let id = id.to_owned();
    let hash = hash.map(str::to_owned);
    database_job(state, async move {
        owned.storage.check_content(&id, hash.as_deref()).await?;
        Ok(page_url(&owned, &id, hash.as_deref()))
    })
    .await
}

async fn create_upload(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<UploadResponse>, AppError> {
    Ok(Json(
        upload(&state, None, &multipart_html(multipart).await?).await?,
    ))
}

async fn update_upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    multipart: Multipart,
) -> Result<Json<UploadResponse>, AppError> {
    Ok(Json(
        upload(&state, Some(&id), &multipart_html(multipart).await?).await?,
    ))
}

async fn list_versions_http(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<VersionListResponse>, AppError> {
    Ok(Json(versions(&state, &id).await?))
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
    let storage = state.storage.clone();
    let selected = hash.map(str::to_owned);
    let content = database_job(&state, async move {
        Ok(storage.get(&id, selected.as_deref()).await?)
    })
    .await?;
    let mut response = content.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "sandbox allow-scripts allow-forms allow-modals allow-popups allow-downloads",
        ),
    );
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
#[path = "../tests/support/helpers.rs"]
mod test_helpers;

#[cfg(test)]
#[path = "../tests/http.rs"]
mod http_tests;

#[cfg(test)]
#[path = "../tests/mcp.rs"]
mod mcp_tests;

#[cfg(test)]
#[path = "../tests/parity.rs"]
mod parity_tests;
