use super::*;
use clap::Parser;

#[tokio::test]
async fn sqlite_startup_waits_for_a_lock_during_wal_setup() {
    let dir = tempfile::tempdir().unwrap();
    let lock = crate::test_helpers::sqlite_startup_lock(dir.path()).await;
    let mut config = crate::Cli::try_parse_from(["slopdrive", "keys", "list"])
        .unwrap()
        .database;
    config.query_timeout_ms = 100;
    let mut opening = Box::pin(config.open(dir.path()));
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut opening)
            .await
            .is_err()
    );
    lock.release().await;
    let storage = opening.await.unwrap();
    let saved = storage
        .create(b"<p>initialized after contention</p>")
        .await
        .unwrap();
    assert_eq!(
        storage.get(&saved.upload_id, None).await.unwrap(),
        b"<p>initialized after contention</p>"
    );
    storage.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sqlite_database_permissions_protect_key_hashes() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = crate::test_helpers::TestApp::new().await;
    if fixture.config.provider == Provider::Sqlite {
        let file = fixture.dir.path().join("slopdrive.sqlite");
        assert_eq!(
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn postgres_options_use_verified_tls_even_when_the_url_disables_it() {
    let fixture = crate::test_helpers::TestApp::new().await;
    let mut config = fixture.config.clone();
    config.provider = Provider::Postgres;
    config.schema = None;
    config.tls_mode = None;
    config.url =
        Some("postgres://user:private-password@db.example:5433/pages?sslmode=disable".into());
    let (options, schema) = config.postgres_options().unwrap();
    assert!(matches!(options.get_ssl_mode(), PgSslMode::VerifyFull));
    assert_eq!(options.get_host(), "db.example");
    assert_eq!(options.get_port(), 5433);
    assert_eq!(options.get_database(), Some("pages"));
    assert_eq!(schema, "slopdrive");
    assert!(
        options
            .get_options()
            .unwrap()
            .contains("search_path=\"slopdrive\"")
    );
    config.tls_mode = Some("disable".into());
    assert!(matches!(
        config.postgres_options().unwrap().0.get_ssl_mode(),
        PgSslMode::Disable
    ));
    config.tls_ca_file = Some("ca.pem".into());
    assert!(matches!(
        config.postgres_options(),
        Err(StoreError::Configuration(_))
    ));
}

#[tokio::test]
async fn postgres_discrete_settings_and_schema_are_validated_without_connecting() {
    let fixture = crate::test_helpers::TestApp::new().await;
    let mut config = fixture.config.clone();
    config.provider = Provider::Postgres;
    config.url = None;
    config.host = Some("db.example".into());
    config.name = Some("pages".into());
    config.user = Some("publisher".into());
    config.password = Some("private-password".into());
    let (options, _) = config.postgres_options().unwrap();
    assert_eq!(options.get_username(), "publisher");
    assert_eq!(options.get_port(), 5432);
    for schema in [
        "",
        "pg_reserved",
        "public; DROP TABLE uploads",
        "1invalid",
        "Uppercase",
        &"a".repeat(64),
    ] {
        config.schema = Some(schema.into());
        assert!(matches!(
            config.postgres_options(),
            Err(StoreError::Configuration(_))
        ));
    }
}

#[tokio::test]
async fn sqlite_urls_cannot_select_volatile_storage_or_another_provider() {
    let dir = tempfile::tempdir().unwrap();
    for url in [
        format!(
            "sqlite://{}?mode=memory",
            dir.path().join("volatile").display()
        ),
        "sqlite::memory:".into(),
        "postgres://localhost/slopdrive".into(),
    ] {
        let mut config = crate::Cli::try_parse_from(["slopdrive", "keys", "list"])
            .unwrap()
            .database;
        config.url = Some(url.clone());
        assert!(
            matches!(
                config.open(dir.path()).await,
                Err(StoreError::Configuration(_))
            ),
            "accepted {url}"
        );
    }
}
