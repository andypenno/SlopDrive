use std::process::Command;

#[path = "support/process.rs"]
mod process;

#[test]
fn key_cli_create_list_revoke_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_slopbox"))
                .arg("--storage-path")
                .arg(dir.path())
                .args(args),
        )
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
            Command::new(env!("CARGO_BIN_EXE_slopbox"))
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
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .env("STORAGE_PATH", dir.path())
            .env("BASE_URL", "http://localhost/")
            .env("BIND_ADDR", listener.local_addr().unwrap().to_string())
            .arg("serve"),
    );
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
        let output = process::output(
            Command::new(env!("CARGO_BIN_EXE_slopbox"))
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
fn serve_reports_key_store_initialization_failure_after_storage_setup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keys"), b"keep").unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(["serve", "--base-url", "http://localhost/"]),
    );
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert!(dir.path().join("uploads").is_dir());
    assert_eq!(std::fs::read(dir.path().join("keys")).unwrap(), b"keep");
}

#[test]
fn startup_rejection_tests_cannot_wait_forever_for_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let started = std::time::Instant::now();
    let child = process::Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args([
                "serve",
                "--base-url",
                "http://localhost/",
                "--bind-addr",
                &address.to_string(),
            ]),
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
            Command::new(env!("CARGO_BIN_EXE_slopbox"))
                .arg("--storage-path")
                .arg(dir.path())
                .args(["keys", "create"]),
        );
        let key_output = String::from_utf8(created.stdout).unwrap();
        let key = key_output.split_whitespace().nth(1).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut child = process::Process::spawn(
            Command::new(env!("CARGO_BIN_EXE_slopbox"))
                .arg("--storage-path")
                .arg(dir.path())
                .args([
                    "serve",
                    "--base-url",
                    "http://localhost/",
                    "--bind-addr",
                    &address.to_string(),
                ]),
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
        write!(stream,"POST /upload HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {key}\r\nContent-Type: multipart/form-data; boundary=boundary\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prefix}",prefix.len()+suffix.len()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            Command::new("kill")
                .args(["-s", signal, &child.0.as_mut().unwrap().id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        std::thread::sleep(Duration::from_millis(50));
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
        let uploads: Vec<_> = std::fs::read_dir(dir.path().join("uploads"))
            .unwrap()
            .collect();
        assert_eq!(uploads.len(), 1);
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(uploads[0].as_ref().unwrap().path().join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["versions"].as_array().unwrap().len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn shutdown_waits_for_a_disconnected_requests_blocking_job() {
    use std::io::{BufRead, Write};
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(["keys", "create"]),
    );
    let key_output = String::from_utf8(output.stdout).unwrap();
    let (id, key) = key_output.trim().split_once(' ').unwrap();
    let key_path = dir.path().join("keys").join(format!("{id}.json"));
    let record = std::fs::read(&key_path).unwrap();
    std::fs::rename(&key_path, key_path.with_extension("original")).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&key_path)
            .status()
            .unwrap()
            .success()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut child = process::Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args([
                "serve",
                "--base-url",
                "http://localhost/",
                "--bind-addr",
                &address.to_string(),
            ]),
    );
    let (phase, reached) = std::sync::mpsc::channel();
    let stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
    let logs = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            if line.unwrap().contains("draining blocking jobs") {
                let _ = phase.send(());
            }
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match std::net::TcpStream::connect(address) {
            Ok(stream) => break stream,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("server did not start: {error}"),
        }
    };
    write!(stream,"GET /upload/{}/versions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {key}\r\n\r\n","0".repeat(32)).unwrap();
    let (opened, reading) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(key_path)
            .unwrap();
        opened.send(()).unwrap();
        let _ = wait.recv_timeout(Duration::from_secs(10));
        file.write_all(&record).unwrap();
    });
    reading.recv_timeout(Duration::from_secs(3)).unwrap();
    stream.shutdown(std::net::Shutdown::Both).unwrap();
    drop(stream);
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.0.as_mut().unwrap().id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    reached
        .recv_timeout(Duration::from_secs(2))
        .expect("HTTP connections did not drain");
    std::thread::sleep(Duration::from_secs(2));
    assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
    release.send(()).unwrap();
    writer.join().unwrap();
    assert!(
        child
            .output(Duration::from_secs(3))
            .unwrap()
            .status
            .success()
    );
    logs.join().unwrap();
}

#[cfg(unix)]
#[test]
fn shutdown_exits_with_an_error_when_a_request_does_not_drain() {
    use std::io::Write;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args(["keys", "create"]),
    );
    let key_output = String::from_utf8(output.stdout).unwrap();
    let key = key_output.split_whitespace().nth(1).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut child = process::Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .arg("--storage-path")
            .arg(dir.path())
            .args([
                "serve",
                "--base-url",
                "http://localhost/",
                "--bind-addr",
                &address.to_string(),
            ]),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match std::net::TcpStream::connect(address) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("server did not start: {error}"),
        }
    };
    write!(stream,"POST /upload HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {key}\r\nContent-Type: multipart/form-data; boundary=b\r\nContent-Length: 100000\r\n\r\n--b\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.0.as_mut().unwrap().id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let output = child
        .output(Duration::from_secs(13))
        .expect("shutdown exceeded its deadline");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("shutdown drain timed out"));
    assert_eq!(
        std::fs::read_dir(dir.path().join("uploads"))
            .unwrap()
            .count(),
        0
    );
}
