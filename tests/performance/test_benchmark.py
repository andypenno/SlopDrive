import importlib.util
import contextlib
import hashlib
import io
import json
import math
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location("benchmark", HERE / "benchmark.py")
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


def check_container_endpoint():
    metadata = [{"NetworkSettings": {"Networks": {"owned-network": {"IPAddress": "172.19.0.2"}}}}]
    with patch.object(benchmark, "command", return_value=json.dumps(metadata)) as command:
        assert benchmark.container_endpoint("docker", "owned-container", "owned-network") == "http://172.19.0.2:3000"
        assert command.call_args.args == ("docker", "inspect", "owned-container")
    for address in ("", "invalid", "127.0.0.1/path"):
        metadata[0]["NetworkSettings"]["Networks"]["owned-network"]["IPAddress"] = address
        with patch.object(benchmark, "command", return_value=json.dumps(metadata)):
            rejects(benchmark.container_endpoint, "docker", "owned-container", "owned-network")
    with patch.object(benchmark, "command", return_value="127.0.0.1:43210\n") as command:
        assert benchmark.container_endpoint("podman", "owned-container", "owned-network") == "http://127.0.0.1:43210"
        assert command.call_args.args == ("podman", "port", "owned-container", "3000/tcp")
    for port in ("0", "65536", "invalid"):
        with patch.object(benchmark, "command", return_value="127.0.0.1:" + port):
            rejects(benchmark.container_endpoint, "podman", "owned-container", "owned-network")


@contextlib.contextmanager
def sample_environment(network=None):
    pages = {}
    requests = []
    environments = []
    address = "172.19.0.2"

    def store(content, upload_id=None):
        upload_id = upload_id or "page" + str(len(pages))
        digest = hashlib.sha256(content.encode()).hexdigest()
        pages.setdefault(upload_id, {})[digest] = content
        return {"upload_id": upload_id, "content_hash": digest}

    def rpc(_, __, name, arguments):
        if name == "list_versions":
            return {"versions": [{"content_hash": digest} for digest in pages[arguments["upload_id"]]]}
        return store(arguments["content"], arguments.get("upload_id"))

    def request(_, path):
        requests.append(path)
        upload_id, _, digest = path[1:].partition("?q=")
        versions = pages[upload_id]
        content = versions[digest or next(reversed(versions))]
        return 200, {"Content-Security-Policy": "sandbox"}, content.encode()

    def command(engine, operation, *args):
        nonlocal address
        if operation == "run" and "--env-file" in args:
            path = Path(args[args.index("--env-file") + 1])
            assert path.stat().st_mode & 0o777 == 0o600
            environments.append((path, dict(line.split("=", 1) for line in path.read_text().splitlines())))
        if operation == "port":
            raise RuntimeError("no published port on internal Docker network")
        if operation == "restart":
            address = "172.19.0.3"
        if operation == "inspect":
            return json.dumps([{"State": {"Running": True}, "NetworkSettings": {"Networks": {network or args[0] + "-network": {"IPAddress": address}}}}])
        return "id key"

    replacements = {
        "command": command, "isolated_network": lambda *_: None,
        "cgroup_path": lambda *_: Path("unused"), "counters": lambda *_: (1, 1),
        "memory_samples": lambda *_: contextlib.nullcontext([1]), "cpu_percent": lambda *_: 0,
        "upload": lambda _, __, content: store(content), "rpc": rpc, "request": request,
    }
    with contextlib.ExitStack() as stack:
        for name, replacement in replacements.items():
            stack.enter_context(patch.object(benchmark, name, replacement))
        stack.enter_context(patch.object(benchmark.time, "sleep"))
        cleanup = stack.enter_context(patch.object(benchmark.subprocess, "run"))
        cleanup.return_value.returncode = 0
        ready = stack.enter_context(patch.object(benchmark, "ready"))
        workload = stack.enter_context(patch.object(benchmark, "workload", return_value={}))
        yield {"ready": ready, "cleanup": cleanup, "workload": workload, "requests": requests, "pages": pages, "environments": environments}


SAMPLE_POLICY = {"resources": {"cpus": 1, "memory_bytes": 256000000}, "workload": {"idle_seconds": 5}}


def check_restart_endpoint():
    with sample_environment() as environment:
        benchmark.run_sample("docker", "image", SAMPLE_POLICY)
        assert [call.args[0] for call in environment["ready"].call_args_list] == ["http://172.19.0.2:3000", "http://172.19.0.3:3000"]


def check_read_versions():
    with sample_environment() as environment:
        def workload(_, operation, __):
            if operation.__name__ == "read":
                for index in range(6):
                    operation(index)
            return {}

        environment["workload"].side_effect = workload
        benchmark.run_sample("docker", "image", SAMPLE_POLICY)
        versions = environment["pages"]["page1"]
        original = next(iter(versions))
        latest = next(reversed(versions))
        assert original != latest
        assert "/page1?q=" + original in environment["requests"]
        assert "/page1" in environment["requests"]
        assert "/page1?q=" + latest not in environment["requests"]


def check_backend_configuration():
    settings = {"DATABASE_TEST_POSTGRES_URL": "postgres://fixture", "S3_TEST_ENDPOINT": "http://garage:3900", "S3_TEST_BUCKET": "fixture", "S3_TEST_REGION": "garage", "S3_TEST_ACCESS_KEY_ID": "fixture-access", "S3_TEST_SECRET_ACCESS_KEY": "fixture-secret", "S3_TEST_ALLOW_HTTP": "true"}
    with patch.dict(benchmark.os.environ, settings, clear=True):
        for mode in benchmark.MODES:
            configured = benchmark.backend_environment(mode, "fixture")
            assert configured["DATABASE_PROVIDER"] + "-" + configured["CONTENT_STORAGE"] == mode
            if mode.startswith("postgres-"):
                assert configured["DATABASE_SCHEMA"] == "perf_fixture"
                assert configured["DATABASE_URL"] == settings["DATABASE_TEST_POSTGRES_URL"]
            if mode.endswith("-s3"):
                assert configured["S3_PREFIX"] == "performance/fixture"
                assert configured["S3_SECRET_ACCESS_KEY"] == settings["S3_TEST_SECRET_ACCESS_KEY"]
                assert configured["S3_ALLOW_HTTP"] == "true"
            if mode != "sqlite-database":
                with sample_environment() as environment:
                    rejects(benchmark.run_sample, "docker", "image", SAMPLE_POLICY, mode)
                    environment["cleanup"].assert_not_called()
        for field in settings:
            if field == "S3_TEST_ALLOW_HTTP":
                continue
            with patch.dict(benchmark.os.environ, {k: v for k, v in settings.items() if k != field}, clear=True):
                rejects(benchmark.backend_environment, "postgres-s3", "fixture")
        with patch.dict(benchmark.os.environ, {"S3_TEST_SECRET_ACCESS_KEY": "private\nINJECTED=true"}):
            try:
                benchmark.backend_environment("postgres-s3", "fixture")
            except ValueError as error:
                assert "private" not in str(error) and "INJECTED" not in str(error)
            else:
                raise AssertionError("multiline setting accepted")
        with sample_environment("owned-backends") as environment:
            benchmark.run_sample("docker", "image", SAMPLE_POLICY, "postgres-s3", "owned-backends")
            assert len(environment["environments"]) == 2
            assert environment["environments"][0][1] == environment["environments"][1][1]
            for path, configured in environment["environments"]:
                assert not path.exists()
                assert configured["DATABASE_URL"] == settings["DATABASE_TEST_POSTGRES_URL"]
                assert configured["S3_SECRET_ACCESS_KEY"] == settings["S3_TEST_SECRET_ACCESS_KEY"]
            assert all(call.args[0][1:3] != ["network", "rm"] for call in environment["cleanup"].call_args_list)
        with sample_environment("owned-backends") as environment:
            environment["workload"].side_effect = RuntimeError("failed workload")
            rejects(benchmark.run_sample, "docker", "image", SAMPLE_POLICY, "postgres-s3", "owned-backends")
            assert len(environment["cleanup"].call_args_list) == 2
            assert all(not path.exists() for path, _ in environment["environments"])


def check_network_constraints():
    for engine, field in (("docker", "Internal"), ("podman", "internal")):
        with patch.object(benchmark, "command", return_value=json.dumps([{field: True}])) as command:
            benchmark.isolated_network(engine, "owned-network", False)
            command.assert_called_once_with(engine, "network", "inspect", "owned-network")
        with patch.object(benchmark, "command", side_effect=["owned-network", json.dumps([{field: True}])]) as command:
            benchmark.isolated_network(engine, "owned-network")
            assert command.call_args_list[0].args == (engine, "network", "create", "--internal", "owned-network")
        for value in (False, None, "true"):
            with patch.object(benchmark, "command", side_effect=["owned-network", json.dumps([{field: value}])]):
                rejects(benchmark.isolated_network, engine, "owned-network")


def check_cleanup():
    name = "slopdrive-perf-cleanup-check"
    expected = [
        ["docker", "rm", "-f", name],
        ["docker", "volume", "rm", name + "-data"],
        ["docker", "network", "rm", name + "-network"],
    ]
    for phase, failure in ((None, None), ("ready", RuntimeError("setup failed")), ("workload", RuntimeError("workload failed")), ("workload", KeyboardInterrupt())):
        with sample_environment() as environment, patch.object(benchmark.uuid, "uuid4") as identifier:
            identifier.return_value.hex = "cleanup-check"
            if phase:
                environment[phase].side_effect = failure
            try:
                benchmark.run_sample("docker", "image", SAMPLE_POLICY)
                assert failure is None
            except (RuntimeError, KeyboardInterrupt) as error:
                assert error is failure
            assert [call.args[0] for call in environment["cleanup"].call_args_list] == expected
    for failure in (benchmark.subprocess.CompletedProcess([], 1, stderr=b"removal denied"), benchmark.subprocess.TimeoutExpired("docker rm", 30), PermissionError("engine unavailable")):
        with sample_environment() as environment, patch.object(benchmark.uuid, "uuid4") as identifier:
            identifier.return_value.hex = "cleanup-check"
            success = benchmark.subprocess.CompletedProcess([], 0)
            environment["cleanup"].side_effect = [failure, success, success]
            try:
                benchmark.run_sample("docker", "image", SAMPLE_POLICY)
            except RuntimeError as error:
                assert "cleanup failed" in str(error)
            else:
                raise AssertionError("cleanup failure was ignored")
            assert [call.args[0] for call in environment["cleanup"].call_args_list] == expected


if __name__ == "__main__":
    check_cgroup_measurements()
    check_statistics()
    check_image_identity()
    check_response_correctness()
    check_response_bound()
    check_container_endpoint()
    check_restart_endpoint()
    check_read_versions()
    check_backend_configuration()
    check_network_constraints()
    check_cleanup()
    print("Cgroup counters, CPU intervals, and latency distributions checked")
