import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import threading

from container_smoke import ready, request, rpc
from replicas import run_mode


def check(binary, output):
    objects = {}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *arguments):
            pass

        def do_PUT(self):
            assert self.headers["Authorization"].startswith("AWS4-HMAC-SHA256 ")
            objects[self.path] = self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(200)
            self.send_header("ETag", '"test"')
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_GET(self):
            value = objects[self.path]
            self.send_response(200)
            self.send_header("ETag", '"test"')
            self.send_header("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT")
            self.send_header("Content-Length", str(len(value)))
            self.end_headers()
            self.wfile.write(value)

    with tempfile.TemporaryDirectory(prefix="slopbox-tls-") as directory:
        root = Path(directory)

        def openssl(*arguments):
            result = subprocess.run(["openssl", *arguments], cwd=root, capture_output=True, timeout=15)
            assert result.returncode == 0, "test certificate generation failed"

        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key",
                "-out", "ca.pem", "-days", "1", "-subj", "/CN=SlopBox test CA",
                "-addext", "basicConstraints=critical,CA:TRUE")
        openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", "server.key",
                "-out", "server.csr", "-subj", "/CN=127.0.0.1")
        (root / "extensions").write_text("subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n")
        openssl("x509", "-req", "-in", "server.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
                "-CAcreateserial", "-out", "server.pem", "-days", "1", "-extfile", "extensions")
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(root / "server.pem", root / "server.key")
        server.socket = context.wrap_socket(server.socket, server_side=True)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        settings = {key: value for key, value in os.environ.items()
                    if not key.startswith(("DATABASE_", "S3_", "CONTENT_STORAGE"))}
        settings.update(STORAGE_PATH=str(root / "data"), BASE_URL="https://pages.example/",
                        CONTENT_STORAGE="s3", S3_ENDPOINT=f"https://127.0.0.1:{server.server_port}",
                        S3_BUCKET="test-bucket", S3_REGION="garage", S3_ACCESS_KEY_ID="test-access",
                        S3_SECRET_ACCESS_KEY="test-secret", S3_TIMEOUT_MS="1000")
        try:
            result = subprocess.run([binary, "keys", "create"], env=settings,
                                    capture_output=True, text=True, timeout=5)
            assert result.returncode == 0, "key CLI failed without contacting S3"
            key = result.stdout.split()[1]
            for trusted in [False, True]:
                if trusted:
                    settings["S3_TLS_CA_FILE"] = str(root / "ca.pem")
                with socket.socket() as listener:
                    listener.bind(("127.0.0.1", 0))
                    port = listener.getsockname()[1]
                settings["BIND_ADDR"] = f"127.0.0.1:{port}"
                with (output / f"tls-service-{trusted}.log").open("wb") as log:
                    process = subprocess.Popen([binary, "serve"], env=settings, stdout=log, stderr=log)
                    try:
                        base = f"http://127.0.0.1:{port}"
                        ready(base)
                        result = rpc(base, key, "tools/call", {
                            "name": "upload_html", "arguments": {"content": "<p>TLS content</p>"}})
                        if trusted:
                            assert result["isError"] is False, "trusted CA round trip failed"
                            upload_id = result["structuredContent"]["upload_id"]
                            assert request(base, "/" + upload_id)[2] == b"<p>TLS content</p>"
                        else:
                            assert result["isError"] is True
                            assert result["content"][0]["text"] == "internal error"
                            assert not objects, "untrusted server received content"
                    finally:
                        process.terminate()
                        try:
                            process.wait(timeout=12)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=3)
                            raise
                        assert process.returncode == 0
            previous = dict(os.environ)
            try:
                for field in ["ENDPOINT", "BUCKET", "ACCESS_KEY_ID", "SECRET_ACCESS_KEY", "REGION", "TLS_CA_FILE"]:
                    os.environ["S3_TEST_" + field] = settings["S3_" + field]
                os.environ["S3_TEST_ALLOW_HTTP"] = "false"
                run_mode(binary, "sqlite", "s3", output)
            finally:
                os.environ.clear()
                os.environ.update(previous)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)
    print("TLS checks passed: unknown CA rejected; configured CA permitted signed PUT and validated GET")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=lambda value: str(Path(value).resolve()), required=True)
    parser.add_argument("--output", type=Path, default=Path("test-output"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    check(args.binary, args.output)
