mod sql;

use self::sql::{PgBackend, SqliteBackend};
use crate::storage::{Storage, StoreError};
use clap::{Args, ValueEnum};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Provider {
    Sqlite,
    Postgres,
}

#[derive(Args, Clone)]
pub struct DatabaseConfig {
    #[arg(long, env = "DATABASE_PROVIDER", value_enum, default_value = "sqlite")]
    pub provider: Provider,
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub url: Option<String>,
    #[arg(long, env = "DATABASE_HOST")]
    pub host: Option<String>,
    #[arg(long, env = "DATABASE_PORT")]
    pub port: Option<u16>,
    #[arg(long, env = "DATABASE_NAME")]
    pub name: Option<String>,
    #[arg(long, env = "DATABASE_USER", hide_env_values = true)]
    pub user: Option<String>,
    #[arg(skip = std::env::var("DATABASE_PASSWORD").ok())]
    pub password: Option<String>,
    #[arg(long, env = "DATABASE_SCHEMA")]
    pub schema: Option<String>,
    #[arg(long, env = "DATABASE_TLS_MODE")]
    pub tls_mode: Option<String>,
    #[arg(long, env = "DATABASE_TLS_CA_FILE")]
    pub tls_ca_file: Option<PathBuf>,
    #[arg(long, env = "DATABASE_MAX_CONNECTIONS", default_value = "4")]
    pub max_connections: u32,
    #[arg(long, env = "DATABASE_CONNECT_TIMEOUT_MS", default_value = "5000")]
    pub connect_timeout_ms: u64,
    #[arg(long, env = "DATABASE_QUERY_TIMEOUT_MS", default_value = "5000")]
    pub query_timeout_ms: u64,
}

#[cfg(test)]
#[path = "../tests/database.rs"]
mod tests;

impl DatabaseConfig {
    pub async fn open(&self, root: &Path) -> Result<Arc<dyn Storage>, StoreError> {
        if !(1..=32).contains(&self.max_connections)
            || !(1..=60000).contains(&self.connect_timeout_ms)
            || !(1..=60000).contains(&self.query_timeout_ms)
        {
            return Err(StoreError::Configuration(
                "invalid database pool or timeout limit",
            ));
        }
        let separate = self.host.is_some()
            || self.port.is_some()
            || self.name.is_some()
            || self.user.is_some()
            || self.password.is_some();
        if self.url.is_some() && separate {
            return Err(StoreError::Configuration(
                "DATABASE_URL conflicts with individual connection settings",
            ));
        }
        let timeout = Duration::from_millis(self.connect_timeout_ms);
        let open = async {
            match self.provider {
                Provider::Sqlite => {
                    if separate
                        || self.schema.is_some()
                        || self.tls_mode.is_some()
                        || self.tls_ca_file.is_some()
                    {
                        return Err(StoreError::Configuration(
                            "PostgreSQL settings cannot be used with SQLite",
                        ));
                    }
                    let options = match &self.url {
                        Some(url) => {
                            if !url.starts_with("sqlite:") || url.contains(['?', '#']) {
                                return Err(StoreError::Configuration(
                                    "SQLite DATABASE_URL must name a file without URL options",
                                ));
                            }
                            SqliteConnectOptions::from_str(url).map_err(|_| {
                                StoreError::Configuration("invalid SQLite DATABASE_URL")
                            })?
                        }
                        None => SqliteConnectOptions::new().filename(root.join("slopbox.sqlite")),
                    };
                    let path = options.get_filename();
                    if path == Path::new(":memory:")
                        || path.as_os_str().is_empty()
                        || path.to_string_lossy().starts_with("file:")
                    {
                        return Err(StoreError::Configuration(
                            "SQLite requires a persistent database file",
                        ));
                    }
                    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut file = std::fs::OpenOptions::new();
                    file.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        file.mode(0o600);
                    }
                    match file.open(path) {
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error.into()),
                    }
                    let options = options
                        .foreign_keys(true)
                        .journal_mode(SqliteJournalMode::Wal)
                        .synchronous(SqliteSynchronous::Full)
                        .busy_timeout(Duration::from_millis(self.query_timeout_ms));
                    let pool = loop {
                        let connected = SqlitePoolOptions::new()
                            .max_connections(self.max_connections)
                            .acquire_timeout(timeout)
                            .connect_with(options.clone())
                            .await;
                        match connected {
                            Ok(pool) => break pool,
                            // Concurrent WAL setup can return SQLITE_BUSY without waiting.
                            Err(sqlx::Error::Database(error))
                                if error.code().as_deref() == Some("5") =>
                            {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            Err(error) => return Err(error.into()),
                        }
                    };
                    let backend = SqliteBackend(pool);
                    backend.initialize().await?;
                    Ok(Arc::new(backend) as Arc<dyn Storage>)
                }
                Provider::Postgres => {
                    let (options, schema) = self.postgres_options()?;
                    let pool = PgPoolOptions::new()
                        .max_connections(self.max_connections)
                        .acquire_timeout(timeout)
                        .connect_with(options)
                        .await?;
                    let backend = PgBackend(pool);
                    backend.initialize(schema).await?;
                    Ok(Arc::new(backend) as Arc<dyn Storage>)
                }
            }
        };
        tokio::time::timeout(timeout, open)
            .await
            .map_err(|_| StoreError::Database("connection timeout".into()))?
    }

    fn postgres_options(&self) -> Result<(PgConnectOptions, &str), StoreError> {
        let schema = self.schema.as_deref().unwrap_or("slopbox");
        if schema.is_empty()
            || schema.len() > 63
            || schema.starts_with("pg_")
            || !schema
                .bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_lowercase() || b == b'_' || (i > 0 && b.is_ascii_digit()))
        {
            return Err(StoreError::Configuration(
                "DATABASE_SCHEMA must be a lowercase SQL identifier",
            ));
        }
        let options = match &self.url {
            Some(url) => {
                if !url.starts_with("postgres://") && !url.starts_with("postgresql://") {
                    return Err(StoreError::Configuration("invalid PostgreSQL DATABASE_URL"));
                }
                PgConnectOptions::from_str(url)
                    .map_err(|_| StoreError::Configuration("invalid PostgreSQL DATABASE_URL"))?
            }
            None => PgConnectOptions::new()
                .host(self.host.as_deref().ok_or(StoreError::Configuration(
                    "DATABASE_HOST is required without DATABASE_URL",
                ))?)
                .port(self.port.unwrap_or(5432))
                .database(self.name.as_deref().ok_or(StoreError::Configuration(
                    "DATABASE_NAME is required without DATABASE_URL",
                ))?)
                .username(self.user.as_deref().ok_or(StoreError::Configuration(
                    "DATABASE_USER is required without DATABASE_URL",
                ))?)
                .password(self.password.as_deref().ok_or(StoreError::Configuration(
                    "DATABASE_PASSWORD is required without DATABASE_URL",
                ))?),
        };
        let ssl = match self.tls_mode.as_deref().unwrap_or("verify-full") {
            "verify-full" => PgSslMode::VerifyFull,
            "disable" => PgSslMode::Disable,
            _ => {
                return Err(StoreError::Configuration(
                    "DATABASE_TLS_MODE must be verify-full or disable",
                ));
            }
        };
        if matches!(ssl, PgSslMode::Disable) && self.tls_ca_file.is_some() {
            return Err(StoreError::Configuration(
                "TLS CA file conflicts with disabled TLS",
            ));
        }
        let mut options = options.ssl_mode(ssl).options([
            ("search_path", format!("\"{schema}\"")),
            ("statement_timeout", self.query_timeout_ms.to_string()),
            ("lock_timeout", self.query_timeout_ms.to_string()),
        ]);
        if let Some(path) = &self.tls_ca_file {
            options = options.ssl_root_cert(path);
        }
        Ok((options, schema))
    }
}
