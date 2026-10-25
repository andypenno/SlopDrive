import argparse
import json
import re
import subprocess
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
IMAGE = "ghcr.io/andypenno/slopdrive"
VARIANTS = ("standard", "alpine")


def command(*args):
    result = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=600)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "command failed: " + args[0])
    return result.stdout.strip()


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError("release version must be X.Y.Z without leading zeros")
    return tuple(map(int, value.split(".")))


def suffix(variant):
    if variant not in VARIANTS:
        raise ValueError("unknown image variant")
    return "-alpine" if variant == "alpine" else ""


def commit_tag(sha, variant):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("a full commit SHA is required")
    return "sha-" + sha + suffix(variant)


def branch_channel(branch):
    if branch not in ("main", "dev"):
        raise ValueError("only main and dev publish branch channels")
    return "latest" if branch == "main" else "dev"


def validate_release(tag, package_version):
    if not tag.startswith("v"):
        raise ValueError("release tags must start with v")
    value = tag[1:]
    version(value)
    if value != package_version:
        raise ValueError("release tag differs from Cargo.toml version")
    command("git", "merge-base", "--is-ancestor", "HEAD", "origin/main")
    return value


def inspect(tag):
    reference = "docker://" + IMAGE + ":" + tag
    try:
        metadata = json.loads(command("skopeo", "inspect", "--no-tags", "--override-os", "linux", "--override-arch", "amd64", reference))
    except RuntimeError as error:
        if re.search(r"manifest unknown|name unknown|MANIFEST_UNKNOWN|NAME_UNKNOWN", str(error)):
            return None
        raise
    labels = metadata.get("Labels") or {}
    return {"digest": metadata["Digest"], "version": labels.get("org.opencontainers.image.version"), "revision": labels.get("org.opencontainers.image.revision")}


def copy(source, destination):
    command("skopeo", "copy", "--all", "--preserve-digests", "docker://" + IMAGE + source, "docker://" + IMAGE + destination)


def release_image(value, sha, variant):
    image = inspect(commit_tag(sha, variant))
    if not image or image["revision"] != sha or image["version"] != value:
        raise ValueError("missing verified image or source/version mismatch: " + variant)
    tag = value + suffix(variant)
    existing = inspect(tag)
    if existing and existing["digest"] != image["digest"]:
        raise ValueError("version tag already points to another image: " + tag)
    return image, existing


def promote(value, sha):
    version(value)
    images = {variant: release_image(value, sha, variant) for variant in VARIANTS}
    stable = []
    for variant in VARIANTS:
        current = inspect("stable" + suffix(variant))
        if current:
            stable.append(version(current["version"]))
    for variant, (image, existing) in images.items():
        if not existing:
            copy("@" + image["digest"], ":" + value + suffix(variant))
    if not stable or version(value) >= max(stable):
        for variant, (image, _) in images.items():
            copy("@" + image["digest"], ":stable" + suffix(variant))


def update_branch(branch, sha, images):
    channel = branch_channel(branch)
    remote = command("gh", "api", "repos/andypenno/SlopDrive/git/ref/heads/" + branch, "--jq", ".object.sha")
    if not re.fullmatch(r"[0-9a-f]{40}", remote):
        raise ValueError("branch does not exist on origin")
    if remote != sha:
        print("Branch advanced; immutable images retained without moving " + channel)
        return
    for variant in VARIANTS:
        copy("@" + images[variant]["digest"], ":" + channel + suffix(variant))


def publish(directory, branch, sha):
    branch_channel(branch)
    sources = {}
    images = {}
    for variant in VARIANTS:
        tag = commit_tag(sha, variant)
        archive = "oci-archive:" + str(directory / (variant + ".tar"))
        metadata = json.loads(command("skopeo", "inspect", "--override-os", "linux", "--override-arch", "amd64", archive))
        labels = metadata.get("Labels") or {}
        if labels.get("org.opencontainers.image.revision") != sha:
            raise ValueError("archive revision does not match commit")
        existing = inspect(tag)
        if existing and (existing["revision"] != sha or existing["version"] != labels.get("org.opencontainers.image.version")):
            raise ValueError("commit tag has a source/version mismatch: " + tag)
        sources[variant] = (archive, existing)
        images[variant] = existing or {"digest": metadata["Digest"], "revision": sha}
    for variant, (archive, existing) in sources.items():
        tag = commit_tag(sha, variant)
        if not existing:
            command("skopeo", "copy", "--all", "--preserve-digests", archive, "docker://" + IMAGE + ":" + tag)
        if inspect(tag)["digest"] != images[variant]["digest"]:
            raise ValueError("registry digest differs from tested archive")
    update_branch(branch, sha, images)


def main():
    parser = argparse.ArgumentParser()
    subcommands = parser.add_subparsers(dest="operation", required=True)
    subcommands.add_parser("version")
    publish_parser = subcommands.add_parser("publish")
    publish_parser.add_argument("--archives", type=Path, required=True)
    publish_parser.add_argument("--branch", required=True)
    publish_parser.add_argument("--sha", required=True)
    promote_parser = subcommands.add_parser("promote")
    promote_parser.add_argument("--tag", required=True)
    args = parser.parse_args()
    package_version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if args.operation == "version":
        print(package_version)
    elif args.operation == "publish":
        publish(args.archives, args.branch, args.sha)
    else:
        value = validate_release(args.tag, package_version)
        sha = command("git", "rev-parse", "HEAD^{commit}")
        promote(value, sha)


if __name__ == "__main__":
    main()
