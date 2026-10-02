import argparse
import hashlib
import json
import subprocess
import time
import urllib.error
import urllib.request
import uuid


def command(*args):
    return subprocess.check_output(args, text=True, timeout=30).strip()


def request(base, path, key=None, body=None, content_type=None, origin=None):
    headers = {}
    if key:
        headers["Authorization"] = "Bearer " + key
    if content_type:
        headers["Content-Type"] = content_type
    if origin:
        headers["Origin"] = origin
    req = urllib.request.Request(base + path, data=body, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=3)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.headers, response.read()


def ready(base):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            if request(base, "/")[0] == 200:
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.1)
    raise AssertionError("container never served the homepage")


def rpc(base, key, method, params):
    status, _, body = request(base, "/mcp", key, json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params
    }).encode(), "application/json")
    assert status == 200
    return json.loads(body)["result"]


def tool(base, key, name, arguments):
    result = rpc(base, key, "tools/call", {"name": name, "arguments": arguments})
    assert result["isError"] is False
    assert json.loads(result["content"][0]["text"]) == result["structuredContent"]
    return result["structuredContent"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", choices=["docker", "podman"], required=True)
    parser.add_argument("--image", default="slopbox:ci")
    parser.add_argument("--other-image")
    args = parser.parse_args()
    name = "slopbox-smoke-" + uuid.uuid4().hex
    volume = name + "-data"
    engine = args.engine
    command(engine, "volume", "create", volume)
    try:
        assert "slopbox" in command(engine, "run", "--rm", args.image, "--help")
        assert command(engine, "run", "--rm", "--entrypoint", "id", args.image, "-u") == "10001"
        key_id, key = command(engine, "run", "--rm", "-v", volume + ":/data", args.image, "keys", "create").split()
        command(engine, "run", "-d", "--name", name, "-p", "127.0.0.1::3000", "-v", volume + ":/data", "-e", "BASE_URL=https://pages.example/", args.image)
        port = command(engine, "port", name, "3000/tcp").splitlines()[0].rsplit(":", 1)[1]
        base = "http://127.0.0.1:" + port
        ready(base)
        assert request(base, "/upload", body=b"bad")[0] == 401
        assert request(base, "/mcp", key, b"{}", "application/json", "https://untrusted.example")[0] == 403
        initialized = rpc(base, key, "initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}})
        assert initialized["protocolVersion"] == "2025-03-26"
        assert len(rpc(base, key, "tools/list", {})["tools"]) == 4
        first = b"<p>persisted first</p>"
        multipart = b'--test\r\nContent-Disposition: form-data; name="file"; filename="p.html"\r\nContent-Type: text/html\r\n\r\n' + first + b"\r\n--test--\r\n"
        status, _, body = request(base, "/upload", key, multipart, "multipart/form-data; boundary=test")
        assert status == 200
        saved = json.loads(body)
        upload_id = saved["upload_id"]
        assert saved["content_hash"] == hashlib.sha256(first).hexdigest()
        assert saved["url"] == "https://pages.example/" + upload_id
        assert saved["versioned_url"] == saved["url"] + "?q=" + saved["content_hash"]
        second = "<p>persisted second</p>"
        updated = tool(base, key, "update_html", {"upload_id": upload_id, "content": second})
        assert updated["content_hash"] == hashlib.sha256(second.encode()).hexdigest()
        command(engine, "restart", "--time", "15", name)
        port = command(engine, "port", name, "3000/tcp").splitlines()[0].rsplit(":", 1)[1]
        base = "http://127.0.0.1:" + port
        ready(base)
        for path, expected in [("/" + upload_id, second.encode()), ("/" + upload_id + "?q=" + saved["content_hash"], first)]:
            status, headers, content = request(base, path)
            assert status == 200 and content == expected
            assert "sandbox" in headers["Content-Security-Policy"]
            assert "allow-same-origin" not in headers["Content-Security-Policy"]
        history = tool(base, key, "list_versions", {"upload_id": upload_id})
        assert len(history["versions"]) == 2
        assert [v["content_hash"] for v in history["versions"]] == [saved["content_hash"], updated["content_hash"]]
        assert tool(base, key, "get_html_url", {"upload_id": upload_id})["url"] == saved["url"]
        assert request(base, "/uploads")[0] == 404
        if args.other_image:
            for image in [args.other_image, args.image]:
                command(engine, "stop", "--time", "15", name)
                command(engine, "rm", name)
                command(engine, "run", "-d", "--name", name, "-p", "127.0.0.1::3000", "-v", volume + ":/data", "-e", "BASE_URL=https://pages.example/", image)
                port = command(engine, "port", name, "3000/tcp").splitlines()[0].rsplit(":", 1)[1]
                base = "http://127.0.0.1:" + port
                ready(base)
                assert request(base, "/" + upload_id)[2] == second.encode()
                assert request(base, "/" + upload_id + "?q=" + saved["content_hash"])[2] == first
                assert tool(base, key, "list_versions", {"upload_id": upload_id}) == history
                if image == args.other_image:
                    second = "<p>written by the other variant</p>"
                    tool(base, key, "update_html", {"upload_id": upload_id, "content": second})
                    history = tool(base, key, "list_versions", {"upload_id": upload_id})
                    assert len(history["versions"]) == 3
        command(engine, "run", "--rm", "-v", volume + ":/data", args.image, "keys", "revoke", key_id)
        assert request(base, "/mcp", key, b"{}", "application/json")[0] == 401
        print(engine + " service, MCP, authentication, and mounted persistence checks passed")
    except Exception:
        logs = subprocess.run([engine, "logs", name], capture_output=True, text=True, timeout=10)
        print(logs.stdout + logs.stderr)
        raise
    finally:
        subprocess.run([engine, "rm", "-f", name], capture_output=True, timeout=30)
        subprocess.run([engine, "volume", "rm", volume], capture_output=True, timeout=30)


if __name__ == "__main__":
    main()
