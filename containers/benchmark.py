import argparse
import concurrent.futures
import contextlib
import hashlib
import ipaddress
import json
import math
import os
import platform
import re
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

from performance import evaluate, number, oci_size, policy_hash, validate_policy


def command(*args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise RuntimeError("container command failed: " + result.stderr.strip())
    return result.stdout.strip()


def counters(cgroup):
    try:
        cpu = dict(line.split() for line in (cgroup / "cpu.stat").read_text().splitlines())
        usage = int(cpu["usage_usec"])
        memory = int((cgroup / "memory.current").read_text())
    except (OSError, ValueError, KeyError) as error:
        raise RuntimeError("cgroup v2 CPU/memory counters are unavailable") from error
    return number(usage, "usage_usec"), number(memory, "memory.current")


def memory_peak(cgroup):
    try:
        value = int((cgroup / "memory.peak").read_text())
    except (OSError, ValueError) as error:
        raise RuntimeError("cgroup v2 peak memory counter is unavailable") from error
    return number(value, "memory.peak")


def cpu_percent(start, end, seconds):
    if end < start or not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("invalid CPU measurement interval")
    return (end - start) / (seconds * 10000)


def percentile(values, fraction):
    if not values or not 0 < fraction <= 1:
        raise ValueError("invalid percentile sample")
    ordered = sorted(number(value, "latency") for value in values)
    return ordered[math.ceil(len(ordered) * fraction) - 1]


def summarize(timings, seconds, memory, cpu_usec):
    if not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("invalid workload interval")
    return {
        "completed": len(timings), "errors": 0,
        "p95_ms": percentile(timings, 0.95),
        "operations_per_second": len(timings) / seconds,
        "cpu_ms_per_op": number(cpu_usec, "cpu_usec") / 1000 / len(timings),
        "memory_bytes": number(memory, "memory_bytes"),
    }


def request(base, path, key=None, data=None, content_type=None):
    headers = {}
    if key:
        headers["Authorization"] = "Bearer " + key
    if content_type:
        headers["Content-Type"] = content_type
    req = urllib.request.Request(base + path, data=data, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        body = response.read(2097153)
        if len(body) > 2097152:
            raise RuntimeError("service response exceeds the measurement client bound")
        return response.status, response.headers, body


def rpc(base, key, name, arguments):
    data = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}}).encode()
    status, _, body = request(base, "/mcp", key, data, "application/json")
    if status != 200:
        raise RuntimeError("MCP workload returned an incorrect HTTP status")
    result = json.loads(body).get("result", {})
    if result.get("isError") is not False:
        raise RuntimeError("MCP workload returned a tool error")
    value = result["structuredContent"]
    if json.loads(result["content"][0]["text"]) != value:
        raise RuntimeError("MCP representations disagree")
    return value


def upload(base, key, content):
    data = b'--perf\r\nContent-Disposition: form-data; name="file"; filename="page.html"\r\nContent-Type: text/html\r\n\r\n' + content.encode() + b"\r\n--perf--\r\n"
    status, _, body = request(base, "/upload", key, data, "multipart/form-data; boundary=perf")
    if status != 200:
        raise RuntimeError("HTTP upload returned an incorrect status")
    value = json.loads(body)
    verify_hash(value, content)
    return value


def verify_hash(value, content):
    if value.get("content_hash") != hashlib.sha256(content.encode()).hexdigest():
        raise RuntimeError("write returned an incorrect content hash")


def page(size, serial):
    prefix = "<p>" + str(serial)
    return prefix + "x" * (size - len(prefix) - 4) + "</p>"


def verify_image(metadata, inspected, expected_revision=None):
    image_id = inspected.get("Id", "")
    if not image_id.startswith("sha256:"):
        image_id = "sha256:" + image_id
    if image_id != metadata["config_digest"]:
        raise RuntimeError("loaded image config differs from the measured OCI runtime image")
    if expected_revision is not None:
        labels = inspected.get("Config", {}).get("Labels") or {}
        if not re.fullmatch(r"[0-9a-f]{40}", expected_revision) or labels.get("org.opencontainers.image.revision") != expected_revision:
            raise RuntimeError("loaded image was built for another source revision")


def ready(base, deadline):
    while time.monotonic() < deadline:
        try:
            status, _, content = request(base, "/")
            if status == 200 and b"SlopBox" in content:
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.02)
    raise RuntimeError("service never became ready")


@contextlib.contextmanager
def memory_samples(cgroup):
    values = []
    errors = []
    stop = threading.Event()

    def sample():
        while not stop.is_set():
            try:
                values.append(counters(cgroup)[1])
            except (ValueError, RuntimeError) as error:
                errors.append(error)
                return
            stop.wait(0.02)

    worker = threading.Thread(target=sample, daemon=True)
    worker.start()
    try:
        yield values
    finally:
        stop.set()
        worker.join(timeout=2)
    if errors or not values or worker.is_alive():
        raise RuntimeError("memory sampling failed")


def workload(cgroup, operation, policy):
    timings = []
    before = counters(cgroup)[0]
    start = time.monotonic()

    def timed(index):
        began = time.monotonic()
        operation(index)
        return (time.monotonic() - began) * 1000

    with memory_samples(cgroup) as samples:
        with concurrent.futures.ThreadPoolExecutor(max_workers=policy["workload"]["concurrency"]) as pool:
            timings.extend(pool.map(timed, range(policy["workload"]["requests"])))
        elapsed = time.monotonic() - start
        cpu = counters(cgroup)[0] - before
    result = summarize(timings, elapsed, max(max(samples), memory_peak(cgroup)), cpu)
    result["p50_ms"] = percentile(timings, 0.5)
    result["cpu_usage_usec"] = cpu
    result["duration_seconds"] = elapsed
    result["raw_latency_ms"] = timings
    result["raw_memory_bytes"] = samples
    return result


def cgroup_path(engine, name, resources):
    pid = int(command(engine, "inspect", "--format", "{{.State.Pid}}", name))
    unified = [line[3:] for line in Path(f"/proc/{pid}/cgroup").read_text().splitlines() if line.startswith("0::")]
    if len(unified) != 1:
        raise RuntimeError("performance checks require cgroup v2")
    root = Path("/sys/fs/cgroup").resolve()
    path = (root / unified[0].lstrip("/")).resolve()
    if not path.is_relative_to(root):
        raise RuntimeError("invalid container cgroup path")
    quota, period = (path / "cpu.max").read_text().split()
    if quota == "max" or abs(int(quota) / int(period) - resources["cpus"]) > 0.0001:
        raise RuntimeError("container engine did not apply the CPU quota")
    if (path / "memory.max").read_text().strip() != str(resources["memory_bytes"]):
        raise RuntimeError("container engine did not apply the memory limit")
    counters(path)
    return path


def isolated_network(engine, name):
    command(engine, "network", "create", "--internal", name)
    metadata = json.loads(command(engine, "network", "inspect", name))[0]
    if metadata.get("Internal", metadata.get("internal")) is not True:
        raise RuntimeError("container engine did not disable external networking")


def container_endpoint(engine, name, network):
    if engine == "docker":
        metadata = json.loads(command(engine, "inspect", name))[0]
        address = ipaddress.IPv4Address(metadata["NetworkSettings"]["Networks"][network]["IPAddress"])
        return f"http://{address}:3000"
    port = int(command(engine, "port", name, "3000/tcp").splitlines()[0].rsplit(":", 1)[1])
    if not 0 < port < 65536:
        raise ValueError("invalid published port")
    return f"http://127.0.0.1:{port}"


def run_sample(engine, image, policy):
    name = "slopbox-perf-" + uuid.uuid4().hex
    volume = name + "-data"
    network = name + "-network"
    resources = policy["resources"]
    envelope = ["--cpus", str(resources["cpus"]), "--memory", str(resources["memory_bytes"]), "--memory-swap", str(resources["memory_bytes"])]
    command(engine, "volume", "create", volume)
    try:
        isolated_network(engine, network)
        _, key = command(engine, "run", "--rm", "--network", "none", *envelope, "-v", volume + ":/data", image, "keys", "create").split()
        start = time.monotonic()
        command(engine, "run", "-d", "--name", name, *envelope, "--network", network, "--pids-limit", "128", "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "-p", "127.0.0.1::3000", "-v", volume + ":/data", "-e", "BASE_URL=http://127.0.0.1/", image)
        base = container_endpoint(engine, name, network)
        ready(base, start + 15)
        result = {"startup": {"ready_ms": (time.monotonic() - start) * 1000}}
        cgroup = cgroup_path(engine, name, resources)
        pages = []
        for size in (1024, 65536, 1048576):
            content = page(size, 0)
            pages.append((upload(base, key, content), content))
        pinned_pages = pages.copy()
        history_id = pages[1][0]["upload_id"]
        versions = {pages[1][0]["content_hash"]}
        for index in range(1, 100):
            content = page(65536, index)
            value = rpc(base, key, "update_html", {"upload_id": history_id, "content": content})
            verify_hash(value, content)
            versions.add(value["content_hash"])
        pages[1] = (value, content)
        history = rpc(base, key, "list_versions", {"upload_id": history_id})
        if {v["content_hash"] for v in history["versions"]} != versions:
            raise RuntimeError("history seeding lost versions")
        before, _ = counters(cgroup)
        start = time.monotonic()
        with memory_samples(cgroup) as samples:
            time.sleep(policy["workload"]["idle_seconds"])
        after, idle_memory = counters(cgroup)
        result["idle"] = {"memory_bytes": max(samples), "cpu_percent": cpu_percent(before, after, time.monotonic() - start)}

        def read(index):
            selected = pinned_pages if index % 2 else pages
            saved, expected = selected[index % len(selected)]
            path = "/" + saved["upload_id"]
            if index % 2:
                path += "?q=" + saved["content_hash"]
            status, headers, body = request(base, path)
            if status != 200 or body != expected.encode() or "sandbox" not in headers.get("Content-Security-Policy", ""):
                raise RuntimeError("public read returned incorrect content or policy")

        writes = []
        writes_lock = threading.Lock()

        def write(index):
            content = page(65536, 1000 + index)
            if index % 3 == 0:
                saved = upload(base, key, content)
            elif index % 3 == 1:
                saved = rpc(base, key, "upload_html", {"content": content})
            else:
                saved = rpc(base, key, "update_html", {"upload_id": history_id, "content": content})
            verify_hash(saved, content)
            status, _, body = request(base, "/" + saved["upload_id"] + "?q=" + saved["content_hash"])
            if status != 200 or body != content.encode():
                raise RuntimeError("write was acknowledged without readable content")
            history = rpc(base, key, "list_versions", {"upload_id": saved["upload_id"]})
            if saved["content_hash"] not in {v["content_hash"] for v in history["versions"]}:
                raise RuntimeError("write was omitted from history")
            with writes_lock:
                writes.append((saved, content))

        def reject(index):
            if index % 3 == 0:
                status, _, _ = request(base, "/upload", "invalid", b"bad", "multipart/form-data")
                expected = 401
            elif index % 3 == 1:
                status, _, _ = request(base, "/" + "0" * 32)
                expected = 404
            else:
                data = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "upload_html", "arguments": {"content": "x" * 1048577}}}).encode()
                status, _, body = request(base, "/mcp", key, data, "application/json")
                if status == 200 and json.loads(body).get("result", {}).get("isError") is True:
                    return
                raise RuntimeError("oversized MCP upload was not rejected")
            if status != expected:
                raise RuntimeError("rejected request returned incorrect status")

        for scenario, operation in (("read", read), ("write", write), ("reject", reject)):
            result[scenario] = workload(cgroup, operation, policy)
        time.sleep(policy["workload"]["idle_seconds"])
        result["recovery"] = {"memory_growth_bytes": max(0, counters(cgroup)[1] - idle_memory)}
        state = json.loads(command(engine, "inspect", name))[0]["State"]
        if not state.get("Running") or state.get("OOMKilled"):
            raise RuntimeError("container stopped or was OOM-killed")
        command(engine, "restart", "--time", "5", name)
        base = container_endpoint(engine, name, network)
        ready(base, time.monotonic() + 15)
        for saved, content in writes:
            status, _, body = request(base, "/" + saved["upload_id"] + "?q=" + saved["content_hash"])
            if status != 200 or body != content.encode():
                raise RuntimeError("acknowledged write did not survive restart")
        return result
    finally:
        cleanup_errors = []
        for args in ([engine, "rm", "-f", name], [engine, "volume", "rm", volume], [engine, "network", "rm", network]):
            try:
                result = subprocess.run(args, capture_output=True, timeout=30)
                if result.returncode:
                    cleanup_errors.append(" ".join(args) + ": " + result.stderr.decode(errors="replace").strip())
            except (OSError, subprocess.TimeoutExpired) as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            raise RuntimeError("container cleanup failed: " + "; ".join(cleanup_errors))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", choices=["docker", "podman"], required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--variant", choices=["standard", "alpine"], required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--collect-only", action="store_true")
    parser.add_argument("--profile", choices=["local", "ci"], default="local")
    parser.add_argument("--revision")
    args = parser.parse_args()
    if args.profile == "ci" and args.revision is None:
        parser.error("the CI profile requires --revision")
    policy = json.loads(args.policy.read_text())
    validate_policy(policy)
    cpu_info = Path("/proc/cpuinfo").read_text().splitlines()
    model = next((line.split(":", 1)[1].strip() for line in cpu_info if line.startswith("model name")), "unknown")
    report = {
        "schema": 1, "variant": args.variant, "profile": args.profile, "policy_sha256": policy_hash(policy), "runner_pid": os.getpid(),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes() + Path(__file__).with_name("performance.py").read_bytes()).hexdigest(),
        "complete": False, "diagnostic": args.collect_only, "samples": [],
        "host": {"kernel": platform.release(), "machine": platform.machine(), "engine": command(args.engine, "--version"), "cpu": model, "logical_cpus": os.cpu_count()},
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)

    def save():
        temporary = args.output.with_suffix(".tmp")
        temporary.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
        temporary.replace(args.output)

    def cancelled(*_):
        raise RuntimeError("measurement cancelled")

    signal.signal(signal.SIGTERM, cancelled)
    signal.alarm(1200)
    signal.signal(signal.SIGALRM, cancelled)
    try:
        metadata = oci_size(args.archive)
        inspected = json.loads(command(args.engine, "image", "inspect", args.image))[0]
        verify_image(metadata, inspected, args.revision)
        labels = inspected.get("Config", {}).get("Labels") or {}
        report["image"] = {**metadata, "unpacked_bytes": inspected["Size"], "source_revision": labels.get("org.opencontainers.image.revision")}
        save()
        for _ in range(policy["runs"]):
            report["samples"].append(run_sample(args.engine, args.image, policy))
            save()
        report["complete"] = True
        save()
        if not args.collect_only:
            evaluate(policy, report)
        print("Collected" if args.collect_only else "Passed", args.variant, "performance checks; report:", args.output)
    except (Exception, KeyboardInterrupt) as error:
        report["error"] = str(error)
        save()
        raise
    finally:
        signal.alarm(0)


if __name__ == "__main__":
    main()
