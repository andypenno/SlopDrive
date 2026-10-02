import hashlib
import json
import math
import re
import tarfile
from pathlib import Path


SCENARIOS = {
    "startup": {"ready_ms": "max"},
    "idle": {"memory_bytes": "max", "cpu_percent": "max"},
    "recovery": {"memory_growth_bytes": "max"},
    **{name: {"memory_bytes": "max", "cpu_ms_per_op": "max", "p95_ms": "max", "operations_per_second": "min"} for name in ("read", "write", "reject")},
}
REFERENCE_METRICS = {"cpu_ms_per_op", "operations_per_second"}


def number(value, name):
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise ValueError("invalid nonnegative measurement: " + name)
    return value


def policy_hash(policy):
    return hashlib.sha256(json.dumps(policy, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest()


def validate_policy(policy):
    if type(policy.get("schema")) is not int or policy["schema"] != 1 or type(policy.get("runs")) is not int or not 1 <= policy["runs"] <= 10:
        raise ValueError("invalid performance policy schema or run count")
    if set(policy.get("scenario_limits", {})) != set(SCENARIOS):
        raise ValueError("policy must cover every required scenario")
    for scenario, metrics in SCENARIOS.items():
        limits = policy["scenario_limits"][scenario]
        if set(limits) != set(metrics):
            raise ValueError("policy must cover every required metric: " + scenario)
        for metric, direction in metrics.items():
            if set(limits[metric]) != {direction}:
                raise ValueError("incorrect budget direction: " + metric)
            if number(limits[metric][direction], metric) <= 0:
                raise ValueError("budget must be positive: " + metric)
    if set(policy.get("image_limits", {})) != {"standard", "alpine"}:
        raise ValueError("both image variants need limits")
    for limits in policy["image_limits"].values():
        if set(limits) != {"compressed_bytes", "unpacked_bytes"}:
            raise ValueError("missing image-size limit")
        for metric, value in limits.items():
            if number(value, metric) <= 0:
                raise ValueError("image budget must be positive")
    resources, workload = policy["resources"], policy["workload"]
    if not 0 < number(resources["cpus"], "cpus") <= 8 or not 16777216 <= number(resources["memory_bytes"], "memory_bytes") <= 4294967296:
        raise ValueError("invalid resource envelope")
    for field, maximum in (("requests", 10000), ("concurrency", 32), ("idle_seconds", 60)):
        if type(workload[field]) is not int or not 1 <= workload[field] <= maximum:
            raise ValueError("invalid workload: " + field)
    if workload["requests"] < 100 or workload["concurrency"] > workload["requests"]:
        raise ValueError("insufficient workload samples")
    reference = policy.get("reference", {})
    if not 1 <= number(reference.get("maximum_ratio"), "maximum_ratio") <= 3:
        raise ValueError("invalid maximum regression ratio")
    if not 0 < number(reference.get("minimum_ratio"), "minimum_ratio") <= 1:
        raise ValueError("invalid minimum regression ratio")
    profiles = reference.get("profiles", {})
    if set(profiles) != {"local", "ci"}:
        raise ValueError("both runner profiles need approved references")
    for variants in profiles.values():
        if set(variants) != {"standard", "alpine"}:
            raise ValueError("both variants need approved references")
        for scenarios in variants.values():
            if set(scenarios) != {"read", "write", "reject"}:
                raise ValueError("missing reference scenario")
            for scenario, metrics in scenarios.items():
                if set(metrics) != REFERENCE_METRICS:
                    raise ValueError("missing reference metric")
                for metric, value in metrics.items():
                    if number(value, metric) <= 0:
                        raise ValueError("reference measurements must be positive")


def evaluate(policy, report):
    validate_policy(policy)
    if type(report.get("schema")) is not int or report["schema"] != 1 or report.get("complete") is not True or report.get("diagnostic") is True:
        raise ValueError("incomplete performance report")
    if report.get("policy_sha256") != policy_hash(policy):
        raise ValueError("report was measured against another policy")
    variant = report.get("variant")
    if variant not in policy["image_limits"]:
        raise ValueError("unknown image variant")
    profile = report.get("profile")
    if profile not in policy["reference"]["profiles"]:
        raise ValueError("unknown runner profile")
    image = report.get("image", {})
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", image.get("digest", "")):
        raise ValueError("missing immutable image identity")
    failures = []
    for metric, bound in policy["image_limits"][variant].items():
        value = number(image.get(metric), metric)
        if value > bound:
            failures.append(f"{variant}.{metric}: {value} > {bound}")
    samples = report.get("samples")
    if not isinstance(samples, list) or len(samples) != policy["runs"]:
        raise ValueError("incorrect number of complete runs")
    for index, sample in enumerate(samples):
        if not isinstance(sample, dict) or set(sample) != set(SCENARIOS):
            raise ValueError("missing or unknown scenario")
        for scenario, metrics in SCENARIOS.items():
            values = sample[scenario]
            if not isinstance(values, dict):
                raise ValueError("invalid scenario measurements")
            if scenario in ("read", "write", "reject"):
                if type(values.get("errors")) is not int or values["errors"] != 0:
                    raise ValueError("incorrect responses or request errors: " + scenario)
                if type(values.get("completed")) is not int or values["completed"] != policy["workload"]["requests"]:
                    raise ValueError("incomplete workload: " + scenario)
            for metric, direction in metrics.items():
                value = number(values.get(metric), scenario + "." + metric)
                bound = policy["scenario_limits"][scenario][metric][direction]
                if (direction == "max" and value > bound) or (direction == "min" and value < bound):
                    failures.append(f"run {index + 1} {scenario}.{metric}: {value} violates {direction} {bound}")
                if scenario in ("read", "write", "reject") and metric in REFERENCE_METRICS:
                    reference = policy["reference"]
                    baseline = reference["profiles"][profile][variant][scenario][metric]
                    ratio = reference["maximum_ratio"] if direction == "max" else reference["minimum_ratio"]
                    if (direction == "max" and value > baseline * ratio) or (direction == "min" and value < baseline * ratio):
                        failures.append(f"run {index + 1} {scenario}.{metric}: {value} violates fixed {profile}/{variant} reference {baseline} at ratio {ratio}")
    if failures:
        raise ValueError("\n".join(failures))


def oci_size(path):
    with tarfile.open(path, "r:*") as archive:
        def read_json(name):
            member = archive.getmember(name)
            if not member.isfile() or member.size > 1048576:
                raise ValueError("invalid OCI metadata member")
            return json.load(archive.extractfile(member))

        def blob(descriptor):
            digest = descriptor.get("digest", "")
            if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
                raise ValueError("invalid OCI digest")
            name = "blobs/sha256/" + digest[7:]
            member = archive.getmember(name)
            if not member.isfile() or type(descriptor.get("size")) is not int or member.size != descriptor["size"]:
                raise ValueError("OCI blob size differs from descriptor")
            hasher = hashlib.sha256()
            with archive.extractfile(member) as stream:
                while chunk := stream.read(1048576):
                    hasher.update(chunk)
            if hasher.hexdigest() != digest[7:]:
                raise ValueError("OCI blob digest differs from descriptor")
            return name

        candidates = []

        def visit(index, depth=0):
            if depth > 4:
                raise ValueError("OCI index nesting exceeds limit")
            for descriptor in index.get("manifests", []):
                platform = descriptor.get("platform")
                if platform and (platform.get("os"), platform.get("architecture")) != ("linux", "amd64"):
                    continue
                metadata = read_json(blob(descriptor))
                if "manifests" in metadata:
                    visit(metadata, depth + 1)
                elif "layers" in metadata and "config" in metadata:
                    config = read_json(blob(metadata["config"]))
                    if (config.get("os"), config.get("architecture")) == ("linux", "amd64"):
                        candidates.append((descriptor["digest"], metadata))

        visit(read_json("index.json"))
        if len(candidates) != 1:
            raise ValueError("OCI archive must identify one Linux amd64 runtime image")
        digest, manifest = candidates[0]
        layers = {}
        for layer in manifest["layers"]:
            if "image.layer" not in layer.get("mediaType", "") and "image.rootfs" not in layer.get("mediaType", ""):
                raise ValueError("unexpected runtime layer type")
            blob(layer)
            layers[layer["digest"]] = layer["size"]
        return {"digest": digest, "compressed_bytes": sum(layers.values()), "config_digest": manifest["config"]["digest"]}
