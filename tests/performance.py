import copy
import hashlib
import importlib.util
import io
import json
import tarfile
from pathlib import Path
from tempfile import TemporaryDirectory


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("performance", ROOT / "containers/performance.py")
performance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(performance)


def rejects(function, *args):
    try:
        function(*args)
    except (ValueError, RuntimeError):
        return
    raise AssertionError("invalid measurement accepted")


def policy_and_report():
    limits = {
        "startup": {"ready_ms": {"max": 1000}},
        "idle": {"memory_bytes": {"max": 1000000}, "cpu_percent": {"max": 1}},
        "recovery": {"memory_growth_bytes": {"max": 100000}},
    }
    for scenario in ("read", "write", "reject"):
        limits[scenario] = {
            "memory_bytes": {"max": 1000000},
            "cpu_ms_per_op": {"max": 10},
            "p95_ms": {"max": 100},
            "operations_per_second": {"min": 10},
        }
    policy = {
        "schema": 1,
        "runs": 2,
        "resources": {"cpus": 1, "memory_bytes": 268435456},
        "workload": {"requests": 100, "concurrency": 4, "idle_seconds": 5},
        "image_limits": {v: {"compressed_bytes": 1000, "unpacked_bytes": 2000} for v in ("standard", "alpine")},
        "scenario_limits": limits,
    }
    baseline = {scenario: {metric: bound.get("max", bound.get("min")) for metric, bound in limits[scenario].items() if metric in ("cpu_ms_per_op", "operations_per_second")} for scenario in ("read", "write", "reject")}
    policy["reference"] = {
        "maximum_ratio": 2,
        "profiles": {profile: {variant: copy.deepcopy(baseline) for variant in ("standard", "alpine")} for profile in ("local", "ci")},
    }
    sample = {name: {metric: 0 if "max" in bound else bound["min"] for metric, bound in metrics.items()} for name, metrics in limits.items()}
    for scenario in ("read", "write", "reject"):
        sample[scenario].update(completed=100, errors=0)
    report = {
        "schema": 1,
        "variant": "standard",
        "profile": "local",
        "policy_sha256": performance.policy_hash(policy),
        "complete": True,
        "image": {"digest": "sha256:" + "a" * 64, "compressed_bytes": 1000, "unpacked_bytes": 2000},
        "samples": [copy.deepcopy(sample) for _ in range(2)],
    }
    return policy, report


def check_policy_boundaries():
    policy = json.loads((ROOT / "containers/performance-policy.json").read_text())
    _, report = policy_and_report()
    report["policy_sha256"] = performance.policy_hash(policy)
    report["samples"] = [copy.deepcopy(report["samples"][0]) for _ in range(policy["runs"])]
    report["samples"][0]["idle"]["memory_bytes"] = 50000000
    for profile in ("local", "ci"):
        report["profile"] = profile
        for variant in ("standard", "alpine"):
            report["variant"] = variant
            for sample in report["samples"]:
                for scenario in ("read", "write", "reject"):
                    sample[scenario].update(
                        completed=policy["workload"]["requests"], p95_ms=500,
                        operations_per_second=policy["reference"]["profiles"][profile][variant][scenario]["operations_per_second"],
                    )
            performance.evaluate(policy, report)
            for scenario in ("read", "write", "reject"):
                changed = copy.deepcopy(report)
                changed["samples"][0][scenario]["p95_ms"] = 500.01
                rejects(performance.evaluate, policy, changed)
            changed = copy.deepcopy(report)
            changed["samples"][0]["idle"]["memory_bytes"] = 50000001
            rejects(performance.evaluate, policy, changed)


def check_budgets():
    policy, report = policy_and_report()
    performance.evaluate(policy, report)
    for variant in ("standard", "alpine"):
        report["variant"] = variant
        performance.evaluate(policy, report)
    for field in ("compressed_bytes", "unpacked_bytes"):
        changed = copy.deepcopy(report)
        changed["image"][field] += 1
        rejects(performance.evaluate, policy, changed)
    for scenario, metrics in policy["scenario_limits"].items():
        for metric, bound in metrics.items():
            changed = copy.deepcopy(report)
            direction, value = next(iter(bound.items()))
            changed["samples"][0][scenario][metric] = value
            performance.evaluate(policy, changed)
            changed["samples"][0][scenario][metric] = value + (1 if direction == "max" else -1)
            rejects(performance.evaluate, policy, changed)
            for invalid in (None, True, -1, float("nan"), float("inf"), "0"):
                changed["samples"][0][scenario][metric] = invalid
                rejects(performance.evaluate, policy, changed)
            del changed["samples"][0][scenario][metric]
            rejects(performance.evaluate, policy, changed)


def check_incomplete_and_policy_tampering():
    policy, report = policy_and_report()
    for field, invalid in (("complete", False), ("diagnostic", True), ("schema", True), ("schema", 2), ("variant", "unknown"), ("policy_sha256", "0" * 64)):
        changed = copy.deepcopy(report)
        changed[field] = invalid
        rejects(performance.evaluate, policy, changed)
    changed = copy.deepcopy(report)
    changed["samples"].pop()
    rejects(performance.evaluate, policy, changed)
    for scenario in ("read", "write", "reject"):
        for field, value in (("errors", 1), ("completed", 99), ("completed", True)):
            changed = copy.deepcopy(report)
            changed["samples"][0][scenario][field] = value
            rejects(performance.evaluate, policy, changed)
    changed = copy.deepcopy(policy)
    changed["scenario_limits"]["read"]["p95_ms"]["max"] *= 100
    rejects(performance.evaluate, changed, report)
    for scenario in policy["scenario_limits"]:
        changed = copy.deepcopy(policy)
        del changed["scenario_limits"][scenario]
        rejects(performance.evaluate, changed, report)


def check_fixed_reference():
    policy, report = policy_and_report()
    policy["reference"]["profiles"]["local"]["standard"]["read"]["cpu_ms_per_op"] = 1
    report["policy_sha256"] = performance.policy_hash(policy)
    report["samples"][0]["read"]["cpu_ms_per_op"] = 2
    performance.evaluate(policy, report)
    report["samples"][0]["read"]["cpu_ms_per_op"] = 2.01
    rejects(performance.evaluate, policy, report)
    report["samples"][0]["read"]["cpu_ms_per_op"] = 1
    policy["reference"]["profiles"]["local"]["standard"]["read"]["operations_per_second"] = 40
    report["policy_sha256"] = performance.policy_hash(policy)
    for sample in report["samples"]:
        sample["read"]["operations_per_second"] = 20
    performance.evaluate(policy, report)
    report["samples"][0]["read"]["operations_per_second"] = 19.9
    performance.evaluate(policy, report)
    report["samples"][0]["read"]["operations_per_second"] = 9.9
    rejects(performance.evaluate, policy, report)
    report["profile"] = "unknown"
    rejects(performance.evaluate, policy, report)
    del policy["reference"]
    rejects(performance.evaluate, policy, report)


def check_oci_size():
    with TemporaryDirectory() as directory:
        archive_path = Path(directory) / "image.tar"
        layer = b"runtime layer"
        blobs = {}

        def blob(value, media):
            data = value if isinstance(value, bytes) else json.dumps(value).encode()
            digest = "sha256:" + hashlib.sha256(data).hexdigest()
            blobs["blobs/sha256/" + digest[7:]] = data
            return {"digest": digest, "size": len(data), "mediaType": media}

        config = blob({"architecture": "amd64", "os": "linux"}, "application/vnd.oci.image.config.v1+json")
        layer_descriptor = blob(layer, "application/vnd.oci.image.layer.v1.tar+gzip")
        manifest = blob({"schemaVersion": 2, "config": config, "layers": [layer_descriptor]}, "application/vnd.oci.image.manifest.v1+json")
        attestation = blob({"schemaVersion": 2, "config": config, "layers": [blob(b"large SBOM" * 100, "application/vnd.in-toto+json")]}, "application/vnd.oci.image.manifest.v1+json")
        manifest["platform"] = {"architecture": "amd64", "os": "linux"}
        attestation["platform"] = {"architecture": "unknown", "os": "unknown"}
        blobs["index.json"] = json.dumps({"schemaVersion": 2, "manifests": [manifest, attestation]}).encode()

        def write_archive():
            with tarfile.open(archive_path, "w") as archive:
                for name, data in blobs.items():
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    archive.addfile(member, io.BytesIO(data))

        write_archive()
        metadata = performance.oci_size(archive_path)
        assert metadata == {"digest": manifest["digest"], "compressed_bytes": len(layer), "config_digest": config["digest"]}
        blobs["blobs/sha256/" + layer_descriptor["digest"][7:]] = b"forged layer!"
        write_archive()
        rejects(performance.oci_size, archive_path)


def check_runner_cpu_allowances():
    policy = json.loads((ROOT / "containers/performance-policy.json").read_text())
    _, report = policy_and_report()
    report["policy_sha256"] = performance.policy_hash(policy)
    report["image"].update(compressed_bytes=1, unpacked_bytes=1)
    for profile in ("local", "ci"):
        report["profile"] = profile
        for variant, costs in (("standard", (1.5, 6, 3)), ("alpine", (2.8, 10, 3.5))):
            report["variant"] = variant
            sample = {
                scenario: {metric: 0 if "max" in bound else bound["min"]
                           for metric, bound in metrics.items()}
                for scenario, metrics in policy["scenario_limits"].items()
            }
            for scenario, cost in zip(("read", "write", "reject"), costs):
                reference = policy["reference"]["profiles"][profile][variant][scenario]
                sample[scenario].update(completed=policy["workload"]["requests"], errors=0,
                                        cpu_ms_per_op=cost,
                                        operations_per_second=reference["operations_per_second"])
            report["samples"] = [copy.deepcopy(sample) for _ in range(policy["runs"])]
            performance.evaluate(policy, report)
            for scenario in ("read", "write", "reject"):
                changed = copy.deepcopy(report)
                changed["samples"][0][scenario]["cpu_ms_per_op"] = (
                    policy["scenario_limits"][scenario]["cpu_ms_per_op"]["max"] + 0.01
                )
                rejects(performance.evaluate, policy, changed)


if __name__ == "__main__":
    check_policy_boundaries()
    check_budgets()
    check_incomplete_and_policy_tampering()
    check_fixed_reference()
    check_runner_cpu_allowances()
    check_oci_size()
    print("Performance budgets, incomplete results, policy identity, and OCI sizes checked")
