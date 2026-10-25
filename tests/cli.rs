use std::process::{Command, Stdio};

#[test]
fn s3_configuration_errors_do_not_disclose_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .env("DATABASE_PROVIDER", "sqlite")
            .env("CONTENT_STORAGE", "s3")
            .env("S3_ENDPOINT", "https://user:private-password@example.com")
            .env("S3_BUCKET", "test-bucket")
            .env("S3_REGION", "garage")
            .env("S3_ACCESS_KEY_ID", "private-access")
            .env("S3_SECRET_ACCESS_KEY", "private-secret")
            .arg("--storage-path")
            .arg(dir.path())
            .args(["keys", "create"]),
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    for private in ["private-password", "private-access", "private-secret"] {
        assert!(!error.contains(private));
    }
}

#[path = "support/process.rs"]
mod process;

#[test]
fn database_provider_must_not_fall_back_to_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .env("DATABASE_PROVIDER", "unknown")
            .arg("--storage-path")
            .arg(dir.path())
            .args(["keys", "create"]),
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn key_cli_create_list_revoke_and_errors() {
    for provider in ["sqlite", "postgres"] {
        if provider == "postgres"
            && std::env::var("SLOPDRIVE_TEST_PROVIDER").as_deref() != Ok("postgres")
        {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_slopdrive"));
            command
                .arg("--storage-path")
                .arg(dir.path())
                .args(args)
                .env("DATABASE_PROVIDER", provider);
            if provider == "postgres" {
                command
                    .env(
                        "DATABASE_URL",
                        std::env::var("DATABASE_TEST_POSTGRES_URL").unwrap(),
                    )
                    .env(
                        "DATABASE_SCHEMA",
                        format!(
                            "cli_{}",
                            dir.path()
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .replace('.', "_")
                                .to_lowercase()
                        ),
                    )
                    .env("DATABASE_TLS_MODE", "disable");
            }
            process::output(&mut command)
        };

        let created = run(&["keys", "create"]);
        assert!(created.status.success());
        let output = String::from_utf8(created.stdout).unwrap();
        let (id, key) = output.trim().split_once(' ').unwrap();
        assert_eq!(id.len(), 16);
        assert!(key.starts_with(&format!("sd_{id}_")));
        assert_eq!(key.len(), 84);

        let listed = run(&["keys", "list"]);
        assert!(listed.status.success());
        assert_eq!(String::from_utf8(listed.stdout).unwrap().trim(), id);
        assert!(run(&["keys", "revoke", "bad/id"]).status.code().unwrap() != 0);
        assert!(
            run(&["keys", "revoke", &"0".repeat(16)])
                .status
                .code()
                .unwrap()
                != 0
        );
        assert!(run(&["keys", "revoke", id]).status.success());
        assert!(run(&["keys", "list"]).stdout.is_empty());
    }
}

#[test]
fn database_configuration_rejects_conflicts_without_disclosing_secrets() {
    let dir = tempfile::tempdir().unwrap();
    for settings in [
        vec![
            ("DATABASE_PROVIDER", "postgres"),
            ("DATABASE_URL", "postgres://user:private-password@host/db"),
            ("DATABASE_HOST", "other"),
        ],
        vec![
            ("DATABASE_PROVIDER", "sqlite"),
            ("DATABASE_PASSWORD", "private-password"),
        ],
        vec![
            ("DATABASE_PROVIDER", "postgres"),
            ("DATABASE_URL", "sqlite:/wrong"),
        ],
        vec![
            ("DATABASE_PROVIDER", "postgres"),
            (
                "DATABASE_URL",
                "postgres://user:private-password@host:invalid/db",
            ),
        ],
        vec![
            ("DATABASE_PROVIDER", "postgres"),
            ("DATABASE_URL", "postgres://host/db"),
            ("DATABASE_SCHEMA", "public;DROP TABLE api_keys"),
        ],
        vec![("DATABASE_MAX_CONNECTIONS", "0")],
    ] {
        let output = process::output(
            Command::new(env!("CARGO_BIN_EXE_slopdrive"))
                .arg("--storage-path")
                .arg(dir.path())
                .args(["keys", "list"])
                .envs(settings),
        );
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-password"));
    }
}

#[test]
fn serve_rejects_non_origin_base_urls() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    for url in [
        "ftp://example.com/",
        "https://example.com/path",
        "https://example.com/?q=1",
        "https://example.com/#fragment",
        "mailto:test@example.com",
        "https://user:password@example.com/",
        "https://user@example.com/",
    ] {
        let output = process::output(
            Command::new(env!("CARGO_BIN_EXE_slopdrive"))
                .arg("--storage-path")
                .arg(dir.path())
                .args(["serve", "--base-url", url])
                .args(["--bind-addr", &listener.local_addr().unwrap().to_string()]),
        );
        assert!(!output.status.success(), "{url}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("BASE_URL"),
            "{url}"
        );
    }
}

#[test]
fn serve_reports_bind_failure_and_honors_environment_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .env("STORAGE_PATH", dir.path())
            .env("BASE_URL", "http://localhost/")
            .env("BIND_ADDR", listener.local_addr().unwrap().to_string())
            .arg("serve"),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bind failed"));
    assert!(dir.path().join("slopdrive.sqlite").is_file());
}

#[test]
fn cli_reports_storage_initialization_failures() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, b"existing").unwrap();
    for args in [
        vec!["keys", "list"],
        vec!["serve", "--base-url", "http://localhost/"],
    ] {
        let output = process::output(
            Command::new(env!("CARGO_BIN_EXE_slopdrive"))
                .arg("--storage-path")
                .arg(&blocker)
                .args(args),
        );
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        assert_eq!(std::fs::read(&blocker).unwrap(), b"existing");
    }
}

#[test]
fn serve_reports_database_initialization_failure() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("slopdrive.sqlite")).unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(["serve", "--base-url", "http://localhost/"]),
    );
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert!(dir.path().join("slopdrive.sqlite").is_dir());
}

#[test]
fn startup_rejection_tests_cannot_wait_forever_for_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let started = std::time::Instant::now();
    let child = process::Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .arg("--storage-path")
            .arg(dir.path())
            .args([
                "serve",
                "--base-url",
                "http://localhost/",
                "--bind-addr",
                &address.to_string(),
            ]),
        Stdio::piped(),
    );
    let timeout = std::time::Duration::from_millis(100);
    let outcome = child
        .output(timeout)
        .expect_err("server unexpectedly exited");
    assert!(!outcome.status.success());
    assert!(started.elapsed() < timeout + std::time::Duration::from_secs(2));
    assert!(std::net::TcpListener::bind(address).is_ok());
}

#[cfg(unix)]
#[test]
fn termination_signals_drain_an_active_upload_before_exit() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    for signal in ["TERM", "INT"] {
        let dir = tempfile::tempdir().unwrap();
        let created = process::output(
            Command::new(env!("CARGO_BIN_EXE_slopdrive"))
                .arg("--storage-path")
                .arg(dir.path())
                .args(["keys", "create"]),
        );
        let key_output = String::from_utf8(created.stdout).unwrap();
        let key = key_output.split_whitespace().nth(1).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let log = dir.path().join("server.log");
        let mut child = process::Process::spawn(
            Command::new(env!("CARGO_BIN_EXE_slopdrive"))
                .arg("--storage-path")
                .arg(dir.path())
                .args([
                    "serve",
                    "--base-url",
                    "http://localhost/",
                    "--bind-addr",
                    &address.to_string(),
                ])
                .env("RUST_LOG", "info"),
            std::fs::File::create(&log).unwrap().into(),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match std::net::TcpStream::connect(address) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("server did not start: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let prefix = "--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"p.html\"\r\nContent-Type: text/html\r\n\r\n";
        let suffix = "<p>drained</p>\r\n--boundary--\r\n";
        write!(stream,"POST /upload HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {key}\r\nContent-Type: multipart/form-data; boundary=boundary\r\nContent-Length: {}\r\nConnection: close\r\nExpect: 100-continue\r\n\r\n{prefix}",prefix.len()+suffix.len()).unwrap();
        let mut interim = [0; 25];
        stream.read_exact(&mut interim).unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        assert!(
            Command::new("kill")
                .args(["-s", signal, &child.0.as_mut().unwrap().id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        process::wait_for_log(&log, "draining requests");
        stream.write_all(suffix.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "request was interrupted by {signal}"
        );
        assert!(
            child
                .output(Duration::from_secs(3))
                .unwrap()
                .status
                .success()
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let pool = sqlx::SqlitePool::connect(&format!(
                "sqlite:{}",
                dir.path().join("slopdrive.sqlite").display()
            ))
            .await
            .unwrap();
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM versions")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(count, 1);
            let content: Vec<u8> = sqlx::query_scalar("SELECT content FROM versions")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(content, b"<p>drained</p>");
        });
    }
}

#[cfg(unix)]
#[test]
fn shutdown_exits_with_an_error_when_a_request_does_not_drain() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(["keys", "create"]),
    );
    let key_output = String::from_utf8(output.stdout).unwrap();
    let key = key_output.split_whitespace().nth(1).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let log = dir.path().join("server.log");
    let mut child = process::Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_slopdrive"))
            .arg("--storage-path")
            .arg(dir.path())
            .args([
                "serve",
                "--base-url",
                "http://localhost/",
                "--bind-addr",
                &address.to_string(),
            ])
            .env("RUST_LOG", "info"),
        std::fs::File::create(&log).unwrap().into(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match std::net::TcpStream::connect(address) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("server did not start: {error}"),
        }
    };
    write!(stream,"POST /upload HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {key}\r\nContent-Type: multipart/form-data; boundary=b\r\nContent-Length: 100000\r\nExpect: 100-continue\r\n\r\n--b\r\n").unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut interim = [0; 25];
    stream.read_exact(&mut interim).unwrap();
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.0.as_mut().unwrap().id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    process::wait_for_log(&log, "draining requests");
    let output = child
        .output(Duration::from_secs(13))
        .expect("shutdown exceeded its deadline");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("shutdown drain timed out"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let pool = sqlx::SqlitePool::connect(&format!(
            "sqlite:{}",
            dir.path().join("slopdrive.sqlite").display()
        ))
        .await
        .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    });
}
