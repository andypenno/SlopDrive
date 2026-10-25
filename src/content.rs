use crate::helpers::{MAX_HTML_BYTES, object_key};
use crate::storage::StoreError;
use clap::{Args, ValueEnum};
use futures_util::TryStreamExt;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::{Certificate, ClientOptions, ObjectStoreExt, RetryConfig, path::Path};
use std::path::PathBuf;
use std::time::Duration;
use url::Url;

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ContentStorage {
    #[default]
    Database,
    S3,
}

#[derive(Args, Clone, Default)]
pub struct ContentConfig {
    #[arg(long, env = "CONTENT_STORAGE", value_enum, default_value = "database")]
    pub content_storage: ContentStorage,
    #[arg(long, env = "S3_ENDPOINT", hide_env_values = true)]
    pub s3_endpoint: Option<String>,
    #[arg(long, env = "S3_BUCKET")]
    pub s3_bucket: Option<String>,
    #[arg(long, env = "S3_REGION")]
    pub s3_region: Option<String>,
    #[arg(skip = std::env::var("S3_ACCESS_KEY_ID").ok())]
    pub s3_access_key_id: Option<String>,
    #[arg(skip = std::env::var("S3_SECRET_ACCESS_KEY").ok())]
    pub s3_secret_access_key: Option<String>,
    #[arg(skip = std::env::var("S3_SESSION_TOKEN").ok())]
    pub s3_session_token: Option<String>,
    #[arg(long, env = "S3_PREFIX")]
    pub s3_prefix: Option<String>,
    #[arg(long, env = "S3_PATH_STYLE", action = clap::ArgAction::Set)]
    pub s3_path_style: Option<bool>,
    #[arg(long, env = "S3_ALLOW_HTTP", action = clap::ArgAction::Set)]
    pub s3_allow_http: Option<bool>,
    #[arg(long, env = "S3_TLS_CA_FILE")]
    pub s3_tls_ca_file: Option<PathBuf>,
    #[arg(long, env = "S3_TIMEOUT_MS")]
    pub s3_timeout_ms: Option<u64>,
}

pub struct S3Content {
    client: AmazonS3,
    pub identity: String,
    pub namespace: String,
    prefix: String,
    timeout: Duration,
}

impl ContentConfig {
    fn endpoint(&self) -> Result<Url, StoreError> {
        let invalid = || StoreError::Configuration("invalid S3 endpoint configuration");
        let endpoint =
            Url::parse(self.s3_endpoint.as_deref().ok_or_else(invalid)?).map_err(|_| invalid())?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || endpoint.path() != "/"
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || (endpoint.scheme() == "http" && !self.s3_allow_http.unwrap_or(false))
            || (endpoint.scheme() == "http" && self.s3_tls_ca_file.is_some())
        {
            return Err(invalid());
        }
        Ok(endpoint)
    }

    fn prefix(&self) -> Result<&str, StoreError> {
        let prefix = self.s3_prefix.as_deref().unwrap_or("slopdrive");
        if prefix.len() > 512
            || (!prefix.is_empty()
                && prefix.split('/').any(|part| {
                    part.is_empty()
                        || part == "."
                        || part == ".."
                        || !part.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'
                        })
                }))
        {
            return Err(StoreError::Configuration("invalid S3 prefix"));
        }
        Ok(prefix)
    }

    pub fn open(&self) -> Result<Option<S3Content>, StoreError> {
        let invalid = || StoreError::Configuration("invalid S3 configuration");
        if self.content_storage == ContentStorage::Database {
            if self.s3_endpoint.is_some()
                || self.s3_bucket.is_some()
                || self.s3_region.is_some()
                || self.s3_access_key_id.is_some()
                || self.s3_secret_access_key.is_some()
                || self.s3_session_token.is_some()
                || self.s3_prefix.is_some()
                || self.s3_path_style.is_some()
                || self.s3_allow_http.is_some()
                || self.s3_tls_ca_file.is_some()
                || self.s3_timeout_ms.is_some()
            {
                return Err(StoreError::Configuration(
                    "S3 settings require CONTENT_STORAGE=s3",
                ));
            }
            return Ok(None);
        }
        let required = |value: &Option<String>| {
            value
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .ok_or_else(invalid)
        };
        let mut endpoint = self.endpoint()?;
        let bucket = required(&self.s3_bucket)?;
        if !(3..=63).contains(&bucket.len())
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || !bucket.as_bytes()[0].is_ascii_alphanumeric()
            || !bucket.as_bytes()[bucket.len() - 1].is_ascii_alphanumeric()
            || bucket.contains("..")
        {
            return Err(invalid());
        }
        let region = required(&self.s3_region)?;
        if !region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(invalid());
        }
        let prefix = self.prefix()?;
        let timeout_ms = self.s3_timeout_ms.unwrap_or(3000);
        if !(1..=5000).contains(&timeout_ms) {
            return Err(invalid());
        }
        let timeout = Duration::from_millis(timeout_ms);
        // Reuse the database TLS provider rather than linking a second crypto implementation.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut options = ClientOptions::new()
            .with_allow_http(self.s3_allow_http.unwrap_or(false))
            .with_timeout(timeout)
            .with_connect_timeout(timeout)
            .with_default_content_type("text/html; charset=utf-8");
        if let Some(path) = &self.s3_tls_ca_file {
            let pem = std::fs::read(path).map_err(|_| invalid())?;
            let certificates = Certificate::from_pem_bundle(&pem).map_err(|_| invalid())?;
            if certificates.is_empty() {
                return Err(invalid());
            }
            for certificate in certificates {
                options = options.with_root_certificate(certificate);
            }
        }
        let identity =
            serde_json::to_string(&(endpoint.as_str(), &bucket, prefix)).map_err(|_| invalid())?;
        if !self.s3_path_style.unwrap_or(true) {
            let host = format!("{bucket}.{}", endpoint.host_str().ok_or_else(invalid)?);
            endpoint.set_host(Some(&host)).map_err(|_| invalid())?;
        }
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(endpoint.as_str())
            .with_bucket_name(bucket)
            .with_region(region)
            .with_access_key_id(required(&self.s3_access_key_id)?)
            .with_secret_access_key(required(&self.s3_secret_access_key)?)
            .with_virtual_hosted_style_request(!self.s3_path_style.unwrap_or(true))
            .with_client_options(options)
            .with_retry(RetryConfig {
                max_retries: 1,
                retry_timeout: timeout,
                ..Default::default()
            });
        if let Some(token) = &self.s3_session_token {
            if token.is_empty() {
                return Err(invalid());
            }
            builder = builder.with_token(token);
        }
        Ok(Some(S3Content {
            client: builder.build().map_err(|_| invalid())?,
            identity,
            namespace: String::new(),
            prefix: prefix.to_owned(),
            timeout,
        }))
    }
}

impl S3Content {
    pub fn key(&self, id: &str, hash: &str) -> String {
        object_key(&self.prefix, &self.namespace, id, hash)
    }

    pub async fn put(&self, key: &str, html: &[u8]) -> Result<(), StoreError> {
        tokio::time::timeout(
            self.timeout,
            self.client.put(&Path::from(key), html.to_vec().into()),
        )
        .await
        .map_err(|_| StoreError::ObjectStorage)?
        .map_err(|_| StoreError::ObjectStorage)?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        tokio::time::timeout(self.timeout, async {
            let result = self
                .client
                .get(&Path::from(key))
                .await
                .map_err(|_| StoreError::ObjectStorage)?;
            if result.meta.size > MAX_HTML_BYTES as u64 {
                return Err(StoreError::InvalidMetadata);
            }
            let mut stream = result.into_stream();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream
                .try_next()
                .await
                .map_err(|_| StoreError::ObjectStorage)?
            {
                if bytes.len() + chunk.len() > MAX_HTML_BYTES {
                    return Err(StoreError::InvalidMetadata);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| StoreError::ObjectStorage)?
    }
}

#[cfg(test)]
#[path = "../tests/content.rs"]
mod tests;
