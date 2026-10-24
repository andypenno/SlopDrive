use crate::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use serde_json::Value;
use sqlx::{PgPool, SqlitePool};
use tempfile::TempDir;
use tower::ServiceExt;

pub(crate) async fn sqlite_startup_lock(root: &std::path::Path) -> TestWriteLock {
    let pool = SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(root.join("slopbox.sqlite"))
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await
    .unwrap();
    TestWriteLock::Sqlite(pool.begin_with("BEGIN EXCLUSIVE").await.unwrap())
}

pub(crate) enum TestDatabase {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

pub(crate) enum TestWriteLock {
    Sqlite(sqlx::Transaction<'static, sqlx::Sqlite>),
    Postgres(sqlx::Transaction<'static, sqlx::Postgres>),
}
impl TestWriteLock {
    pub(crate) async fn release(self) {
        match self {
            Self::Sqlite(tx) => tx.rollback().await.unwrap(),
            Self::Postgres(tx) => tx.rollback().await.unwrap(),
        }
    }
}
impl TestDatabase {
    pub(crate) async fn lock_writes(&self) -> TestWriteLock {
        match self {
            Self::Sqlite(pool) => {
                TestWriteLock::Sqlite(pool.begin_with("BEGIN IMMEDIATE").await.unwrap())
            }
            Self::Postgres(pool) => {
                let mut tx = pool.begin().await.unwrap();
                sqlx::query("LOCK TABLE uploads IN EXCLUSIVE MODE")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                TestWriteLock::Postgres(tx)
            }
        }
    }
    pub(crate) async fn rejects_null_key_id(&self) -> bool {
        self.execute(&format!(
            "INSERT INTO api_keys (id, secret_hash) VALUES (NULL, '{}')",
            "a".repeat(64)
        ))
        .await
        .is_err()
    }
    pub(crate) async fn rejects_invalid_timestamp(&self, id: &str) -> bool {
        self.execute(&format!(
            "UPDATE versions SET created_at = 'invalid' WHERE upload_id = '{id}'"
        ))
        .await
        .is_err()
    }
    pub(crate) async fn rejects_duplicate_version(&self, id: &str) -> bool {
        self.execute(&format!("BEGIN; INSERT INTO versions (upload_id, content_hash, created_at, size_bytes, version_number, content, object_key) SELECT upload_id, content_hash, created_at, size_bytes, version_number + 1, content, object_key FROM versions WHERE upload_id = '{id}'; COMMIT;")).await.is_err()
    }
    pub(crate) async fn rejects_missing_latest(&self, id: &str) -> bool {
        self.execute(&format!(
            "UPDATE uploads SET latest_hash = '{}' WHERE id = '{id}'",
            "0".repeat(64)
        ))
        .await
        .is_err()
    }
    pub(crate) async fn set_schema_version(&self, version: i64) {
        self.execute(&format!("UPDATE schema_migrations SET version = {version} WHERE version = (SELECT MAX(version) FROM schema_migrations)"))
            .await
            .unwrap();
    }
    pub(crate) async fn corrupt_migration_checksum(&self) {
        self.execute("UPDATE schema_migrations SET checksum = 'modified'")
            .await
            .unwrap();
    }
    pub(crate) async fn corrupt_timestamp(&self, id: &str) {
        self.execute(&format!(
            "UPDATE versions SET created_at = {} WHERE upload_id = '{id}'",
            i64::MAX
        ))
        .await
        .unwrap();
    }
    pub(crate) async fn reject_versions(&self, existing: &str) {
        let sql = match self {
            Self::Sqlite(_) => "CREATE TRIGGER reject_version BEFORE INSERT ON versions BEGIN SELECT RAISE(ABORT, 'private database detail'); END;".to_owned(),
            Self::Postgres(_) => format!("ALTER TABLE versions ADD CONSTRAINT reject_version CHECK (content_hash = '{existing}')"),
        };
        self.execute(&sql).await.unwrap();
    }
    pub(crate) async fn corrupt_content(&self, id: &str, content: &[u8]) {
        match self {
            Self::Sqlite(pool) => {
                sqlx::query("UPDATE versions SET content = $1 WHERE upload_id = $2")
                    .bind(content)
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap();
            }
            Self::Postgres(pool) => {
                sqlx::query("UPDATE versions SET content = $1 WHERE upload_id = $2")
                    .bind(content)
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap();
            }
        }
    }
    async fn execute(&self, sql: &str) -> Result<(), sqlx::Error> {
        macro_rules! run {
            ($pool:expr) => {{
                let mut connection = $pool.acquire().await?;
                let result = sqlx::raw_sql(sql).execute(&mut *connection).await;
                if result.is_err() {
                    let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                }
                result.map(|_| ())
            }};
        }
        match self {
            Self::Sqlite(pool) => run!(pool),
            Self::Postgres(pool) => run!(pool),
        }
    }
    pub(crate) async fn upload_count(&self) -> i64 {
        match self {
            Self::Sqlite(pool) => sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
                .fetch_one(pool)
                .await
                .unwrap(),
            Self::Postgres(pool) => sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
                .fetch_one(pool)
                .await
                .unwrap(),
        }
    }
}

pub(crate) struct TestApp {
    pub(crate) app: Router,
    pub(crate) key: String,
    pub(crate) state: AppState,
    pub(crate) dir: TempDir,
    pub(crate) config: database::DatabaseConfig,
    pub(crate) db: TestDatabase,
    s3_fixture: Option<crate::test_s3::TestS3>,
}

impl TestApp {
    pub(crate) async fn new() -> Self {
        let s3 = if std::env::var("SLOPBOX_TEST_CONTENT").as_deref() == Ok("s3") {
            Some(crate::test_s3::TestS3::new().await)
        } else {
            None
        };
        let content = match &s3 {
            Some(server) => {
                let mut config = server.config.clone();
                if let Ok(endpoint) = std::env::var("S3_TEST_ENDPOINT") {
                    config.s3_endpoint = Some(endpoint);
                    config.s3_bucket = Some(std::env::var("S3_TEST_BUCKET").unwrap());
                    config.s3_access_key_id = Some(std::env::var("S3_TEST_ACCESS_KEY_ID").unwrap());
                    config.s3_secret_access_key =
                        Some(std::env::var("S3_TEST_SECRET_ACCESS_KEY").unwrap());
                    config.s3_region =
                        Some(std::env::var("S3_TEST_REGION").unwrap_or_else(|_| "garage".into()));
                }
                config
            }
            None => Default::default(),
        };
        let mut fixture = Self::with_content(content).await;
        fixture.s3_fixture = s3;
        fixture
    }

    pub(crate) async fn with_content(content: crate::content::ContentConfig) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = database::DatabaseConfig {
            content,
            provider: database::Provider::Sqlite,
            url: None,
            host: None,
            port: None,
            name: None,
            user: None,
            password: None,
            schema: None,
            tls_mode: None,
            tls_ca_file: None,
            max_connections: 4,
            connect_timeout_ms: 5000,
            query_timeout_ms: 5000,
        };
        config.provider = match std::env::var("SLOPBOX_TEST_PROVIDER").as_deref() {
            Ok("postgres") => database::Provider::Postgres,
            Ok("sqlite") | Err(_) => database::Provider::Sqlite,
            _ => panic!("invalid SLOPBOX_TEST_PROVIDER"),
        };
        if config.provider == database::Provider::Postgres {
            config.url = Some(
                std::env::var("DATABASE_TEST_POSTGRES_URL")
                    .expect("PostgreSQL tests require DATABASE_TEST_POSTGRES_URL"),
            );
            config.schema = Some(format!("test_{}", helpers::random_hex(8).unwrap()));
            config.tls_mode = Some("disable".into());
        }
        let storage = config.open(dir.path()).await.unwrap();
        let db = match config.provider {
            database::Provider::Sqlite => TestDatabase::Sqlite(
                SqlitePool::connect(&format!(
                    "sqlite:{}",
                    dir.path().join("slopbox.sqlite").display()
                ))
                .await
                .unwrap(),
            ),
            database::Provider::Postgres => {
                use std::str::FromStr;
                let options =
                    sqlx::postgres::PgConnectOptions::from_str(config.url.as_ref().unwrap())
                        .unwrap()
                        .ssl_mode(sqlx::postgres::PgSslMode::Disable)
                        .options([(
                            "search_path",
                            format!("\"{}\"", config.schema.as_ref().unwrap()),
                        )]);
                TestDatabase::Postgres(
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_with(options)
                        .await
                        .unwrap(),
                )
            }
        };
        let keys = Arc::new(KeyStore::new(storage.clone()));
        let (_, key) = keys.create().await.unwrap();
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
            config,
            db,
            s3_fixture: None,
        }
    }

    pub(crate) async fn corrupt_content(&self, id: &str, bytes: &[u8]) {
        let key: Option<String> = match &self.db {
            TestDatabase::Sqlite(pool) => {
                sqlx::query_scalar("SELECT object_key FROM versions WHERE upload_id = $1 LIMIT 1")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap()
            }
            TestDatabase::Postgres(pool) => {
                sqlx::query_scalar("SELECT object_key FROM versions WHERE upload_id = $1 LIMIT 1")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap()
            }
        };
        if let Some(key) = key {
            self.config
                .content
                .open()
                .unwrap()
                .unwrap()
                .put(&key, bytes)
                .await
                .unwrap();
        } else {
            self.db.corrupt_content(id, bytes).await;
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
