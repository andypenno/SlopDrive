use std::process::Command;

#[test]
fn key_cli_create_list_revoke_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(args)
            .output()
            .unwrap()
    };

    let created = run(&["keys", "create"]);
    assert!(created.status.success());
    let output = String::from_utf8(created.stdout).unwrap();
    let (id, key) = output.trim().split_once(' ').unwrap();
    assert_eq!(id.len(), 16);
    assert!(key.starts_with(&format!("sb_{id}_")));
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

#[test]
fn serve_rejects_non_origin_base_urls() {
    let dir = tempfile::tempdir().unwrap();
    for url in [
        "ftp://example.com/",
        "https://example.com/path",
        "https://example.com/?q=1",
        "https://example.com/#fragment",
        "mailto:test@example.com",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(["serve", "--base-url", url])
            .output()
            .unwrap();
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
    let output = Command::new(env!("CARGO_BIN_EXE_slopbox"))
        .env("STORAGE_PATH", dir.path())
        .env("BASE_URL", "http://localhost/")
        .env("BIND_ADDR", listener.local_addr().unwrap().to_string())
        .arg("serve")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bind failed"));
    assert!(dir.path().join("uploads").is_dir());
    assert!(dir.path().join("keys").is_dir());
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
        let output = Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(&blocker)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        assert_eq!(std::fs::read(&blocker).unwrap(), b"existing");
    }
}

#[test]
fn serve_reports_key_store_initialization_failure_after_storage_setup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keys"), b"keep").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_slopbox"))
        .arg("--storage-path")
        .arg(dir.path())
        .args(["serve", "--base-url", "http://localhost/"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert!(dir.path().join("uploads").is_dir());
    assert_eq!(std::fs::read(dir.path().join("keys")).unwrap(), b"keep");
}
