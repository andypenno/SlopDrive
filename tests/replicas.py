import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import uuid

from container_smoke import ready, request, tool


def run_mode(binary, database, content, output_directory):
    with tempfile.TemporaryDirectory(prefix="slopbox-replicas-") as directory:
        root = Path(directory)
        settings = {key: value for key, value in os.environ.items()
                    if not key.startswith(("DATABASE_", "S3_", "CONTENT_STORAGE"))}
        settings.update(BASE_URL="https://pages.example/", CONTENT_STORAGE=content)
        if database == "postgres":
            settings.update(DATABASE_PROVIDER="postgres",
                            DATABASE_URL=os.environ["DATABASE_TEST_POSTGRES_URL"],
                            DATABASE_SCHEMA="replicas_" + uuid.uuid4().hex,
                            DATABASE_TLS_MODE="disable")
        else:
            settings.update(DATABASE_PROVIDER="sqlite",
                            DATABASE_URL="sqlite:" + str(root / "shared.sqlite"))
        if content == "s3":
            for field in ["ENDPOINT", "BUCKET", "ACCESS_KEY_ID", "SECRET_ACCESS_KEY", "REGION"]:
                settings["S3_" + field] = os.environ["S3_TEST_" + field]
            settings.update(S3_ALLOW_HTTP=os.environ.get("S3_TEST_ALLOW_HTTP", "false"),
                            S3_PREFIX="replicas/" + uuid.uuid4().hex)
            if "S3_TEST_TLS_CA_FILE" in os.environ:
                settings["S3_TLS_CA_FILE"] = os.environ["S3_TEST_TLS_CA_FILE"]

        def cli(*arguments):
            result = subprocess.run([binary, *arguments], env=settings, capture_output=True,
                                    text=True, timeout=15)
            assert result.returncode == 0, "key CLI failed; inspect the isolated service logs"
            return result.stdout.strip()

        key_id, key = cli("keys", "create").split()
        processes = []
        logs = []

        def start(name):
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            log = (output_directory / f"replica-{database}-{content}-{name}.log").open("wb")
            logs.append(log)
            environment = dict(settings, STORAGE_PATH=str(root / name),
                               BIND_ADDR=f"127.0.0.1:{port}")
            process = subprocess.Popen([binary, "serve"], env=environment,
                                       stdout=log, stderr=log)
            processes.append(process)
            base = f"http://127.0.0.1:{port}"
            ready(base)
            return process, base

        def stop(process):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
                    raise AssertionError("replica did not drain") from None
                assert process.returncode == 0, "replica exited with an error"

        try:
            first_process, first = start("first")
            second_process, second = start("second")
            original = b"<p>original</p>"
            saved = tool(first, key, "upload_html", {"content": original.decode()})
            upload_id = saved["upload_id"]
            assert request(second, "/" + upload_id)[2] == original

            def http_update():
                page = b"<p>HTTP version</p>"
                body = (b'--replica\r\nContent-Disposition: form-data; name="file"; '
                        b'filename="page.html"\r\nContent-Type: text/html\r\n\r\n'
                        + page + b"\r\n--replica--\r\n")
                status, _, result = request(first, "/upload/" + upload_id, key, body,
                                            "multipart/form-data; boundary=replica")
                assert status == 200
                return page, json.loads(result)

            def mcp_update():
                page = b"<p>MCP version</p>"
                return page, tool(second, key, "update_html",
                                  {"upload_id": upload_id, "content": page.decode()})

            with ThreadPoolExecutor(max_workers=2) as executor:
                futures = [executor.submit(http_update), executor.submit(mcp_update)]
                changed = [future.result(timeout=15) for future in futures]
            status, _, body = request(second, f"/upload/{upload_id}/versions", key)
            assert status == 200
            history = json.loads(body)["versions"]
            expected = {hashlib.sha256(page).hexdigest() for page in
                        [original, *[page for page, _ in changed]]}
            assert {version["content_hash"] for version in history} == expected
            original_metadata = next(v for v in history if v["content_hash"] == saved["content_hash"])
            for page, version in [(original, saved), *changed]:
                assert request(second, f"/{upload_id}?q={version['content_hash']}")[2] == page
            tool(first, key, "update_html", {"upload_id": upload_id, "content": original.decode()})
            restored = tool(second, key, "list_versions", {"upload_id": upload_id})["versions"]
            assert len(restored) == 3
            assert next(v for v in restored if v["content_hash"] == saved["content_hash"]) == original_metadata
            cli("keys", "revoke", key_id)
            for base in [first, second]:
                assert request(base, f"/upload/{upload_id}/versions", key)[0] == 401
            stop(first_process)
            stop(second_process)
            _, restarted = start("fresh-local-directory")
            assert request(restarted, "/" + upload_id)[2] == original
            for page, version in changed:
                assert request(restarted, f"/{upload_id}?q={version['content_hash']}")[2] == page
            assert request(restarted, f"/upload/{upload_id}/versions", key)[0] == 401
        finally:
            try:
                for process in processes:
                    stop(process)
            finally:
                for log in logs:
                    log.close()
    return {"database": database, "content": content, "passed": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=lambda value: str(Path(value).resolve()), required=True)
    parser.add_argument("--postgres", action="store_true", help="explicit opt-in to the disposable PostgreSQL fixture")
    parser.add_argument("--s3", action="store_true", help="explicit opt-in to the external S3 fixture")
    parser.add_argument("--output", type=Path, default=Path("test-output/replicas.json"))
    args = parser.parse_args()
    results = []
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for database in ["sqlite", *(["postgres"] if args.postgres else [])]:
        for content in ["database", *(["s3"] if args.s3 else [])]:
            results.append(run_mode(args.binary, database, content, args.output.parent))
            args.output.write_text(json.dumps(results, indent=2) + "\n")
    print(f"Passed {len(results)} independent-process storage configurations")
