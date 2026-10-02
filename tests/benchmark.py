import importlib.util
import io
import json
import math
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "containers"))
spec = importlib.util.spec_from_file_location("benchmark", ROOT / "containers/benchmark.py")
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


def rejects(function, *args):
    try:
        function(*args)
    except (ValueError, RuntimeError):
        return
    raise AssertionError("invalid resource measurement accepted")


def check_cgroup_measurements():
    with TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "cpu.stat").write_text("usage_usec 250000\nuser_usec 100000\n")
        (root / "memory.current").write_text("123456\n")
        (root / "memory.peak").write_text("345678\n")
        assert benchmark.counters(root) == (250000, 123456)
        assert benchmark.memory_peak(root) == 345678
        (root / "memory.peak").write_text("-1\n")
        rejects(benchmark.memory_peak, root)
        for invalid in ("usage_usec -1\n", "user_usec 123\n", "usage_usec nan\n"):
            (root / "cpu.stat").write_text(invalid)
            rejects(benchmark.counters, root)
        (root / "cpu.stat").write_text("usage_usec 250000\n")
        (root / "memory.current").write_text("-1\n")
        rejects(benchmark.counters, root)
    assert benchmark.cpu_percent(100000, 300000, 2) == 10
    rejects(benchmark.cpu_percent, 300000, 100000, 2)
    rejects(benchmark.cpu_percent, 0, 1, 0)


def check_statistics():
    timings = [float(n) for n in range(1, 101)]
    assert benchmark.percentile(timings, 0.95) == 95
    rejects(benchmark.percentile, [], 0.95)
    rejects(benchmark.percentile, [math.nan], 0.95)
    results = benchmark.summarize([1.0] * 100, 2.0, 1000000, 100000)
    assert results == {
        "completed": 100, "errors": 0, "p95_ms": 1.0,
        "operations_per_second": 50.0, "cpu_ms_per_op": 1.0,
        "memory_bytes": 1000000,
    }


def check_image_identity():
    metadata = {"config_digest": "sha256:" + "a" * 64}
    benchmark.verify_image(metadata, {"Id": "sha256:" + "a" * 64})
    benchmark.verify_image(metadata, {"Id": "a" * 64})
    rejects(benchmark.verify_image, metadata, {"Id": "sha256:" + "b" * 64})
    rejects(benchmark.verify_image, metadata, {"RepoDigests": ["old:image"]})
    inspected = {"Id": "a" * 64, "Config": {"Labels": {"org.opencontainers.image.revision": "c" * 40}}}
    benchmark.verify_image(metadata, inspected, "c" * 40)
    rejects(benchmark.verify_image, metadata, inspected, "d" * 40)
    rejects(benchmark.verify_image, metadata, {"Id": "a" * 64}, "c" * 40)


def check_response_correctness():
    with patch.object(benchmark, "request", return_value=(200, {}, b'{"content_hash":"incorrect"}')):
        rejects(benchmark.upload, "http://local", "test-key", "<p>content</p>")
    with patch.object(benchmark, "request", return_value=(503, {}, b"unavailable")):
        rejects(benchmark.upload, "http://local", "test-key", "<p>content</p>")
    failed = json.dumps({"result": {"isError": True}}).encode()
    with patch.object(benchmark, "request", return_value=(200, {}, failed)):
        rejects(benchmark.rpc, "http://local", "test-key", "upload_html", {})
    inconsistent = json.dumps({"result": {"isError": False, "structuredContent": {}, "content": [{"text": '{"different":true}'}]}}).encode()
    with patch.object(benchmark, "request", return_value=(200, {}, inconsistent)):
        rejects(benchmark.rpc, "http://local", "test-key", "upload_html", {})


def check_response_bound():
    response = io.BytesIO(b"x" * (2097152 + 1))
    response.status = 200
    response.headers = {}
    with patch.object(benchmark.urllib.request, "urlopen", return_value=response):
        rejects(benchmark.request, "http://local", "/")


def check_network_constraints():
    for engine, field in (("docker", "Internal"), ("podman", "internal")):
        with patch.object(benchmark, "command", side_effect=["owned-network", json.dumps([{field: True}])]) as command:
            benchmark.isolated_network(engine, "owned-network")
            assert command.call_args_list[0].args == (engine, "network", "create", "--internal", "owned-network")
        for value in (False, None, "true"):
            with patch.object(benchmark, "command", side_effect=["owned-network", json.dumps([{field: value}])]):
                rejects(benchmark.isolated_network, engine, "owned-network")


if __name__ == "__main__":
    check_cgroup_measurements()
    check_statistics()
    check_image_identity()
    check_response_correctness()
    check_response_bound()
    check_network_constraints()
    print("Cgroup counters, CPU intervals, and latency distributions checked")
