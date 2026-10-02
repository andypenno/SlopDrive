import importlib.util
import json
import subprocess
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", ROOT / "containers/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def rejects(function, *args):
    try:
        function(*args)
    except (ValueError, RuntimeError):
        return
    raise AssertionError("invalid release operation accepted")


def check_versions():
    assert release.version("1.2.3") == (1, 2, 3)
    assert release.version("0.10.0") > release.version("0.9.9")
    for invalid in ["v1.2.3", "1.2", "01.2.3", "1.2.3-rc.1", "1.2.3\n", "-1.2.3", None]:
        rejects(release.version, invalid)
    sha = "a" * 40
    assert release.branch_channel("main") == "latest"
    assert release.branch_channel("dev") == "dev"
    rejects(release.branch_channel, "personal")
    assert release.commit_tag(sha, "standard") == "sha-" + sha
    assert release.commit_tag(sha, "alpine") == "sha-" + sha + "-alpine"
    rejects(release.commit_tag, "abc", "standard")
    rejects(release.commit_tag, sha, "unknown")


def check_release_boundary():
    with patch.object(release, "command") as command:
        assert release.validate_release("v1.2.3", "1.2.3") == "1.2.3"
        command.assert_called_once_with("git", "merge-base", "--is-ancestor", "HEAD", "origin/main")
    with patch.object(release, "command", side_effect=RuntimeError("not on main")):
        rejects(release.validate_release, "v1.2.3", "1.2.3")
    for tag, package in [("1.2.3", "1.2.3"), ("v1.2.3", "1.2.4"), ("v1.2.3-rc.1", "1.2.3")]:
        rejects(release.validate_release, tag, package)


def check_promotion():
    sha = "a" * 40
    digests = {"standard": "sha256:" + "1" * 64, "alpine": "sha256:" + "2" * 64}
    copies = []

    def inspect(tag):
        if tag.startswith("sha-"):
            variant = "alpine" if tag.endswith("-alpine") else "standard"
            return {"digest": digests[variant], "version": "1.2.3", "revision": sha}
        return None

    with patch.object(release, "inspect", side_effect=inspect), patch.object(release, "copy", side_effect=lambda *args: copies.append(args)):
        release.promote("1.2.3", sha)
    assert copies == [
        ("@" + digests["standard"], ":1.2.3"),
        ("@" + digests["alpine"], ":1.2.3-alpine"),
        ("@" + digests["standard"], ":stable"),
        ("@" + digests["alpine"], ":stable-alpine"),
    ]
    assert all("latest" not in destination for _, destination in copies)

    def older_release(tag):
        if tag in ["stable", "stable-alpine"]:
            return {"version": "1.10.0", "digest": "sha256:" + "3" * 64, "revision": sha}
        return inspect(tag)

    copies.clear()
    with patch.object(release, "inspect", side_effect=older_release), patch.object(release, "copy", side_effect=lambda *args: copies.append(args)):
        release.promote("1.2.3", sha)
    assert len(copies) == 2 and not any("stable" in destination for _, destination in copies)

    for bad_tag, bad_value in [
        ("sha-" + sha + "-alpine", None),
        ("1.2.3-alpine", {"digest": "sha256:" + "9" * 64}),
        ("sha-" + sha, {"digest": digests["standard"], "version": "1.2.3", "revision": "b" * 40}),
        ("sha-" + sha, {"digest": digests["standard"], "version": "1.2.4", "revision": sha}),
    ]:
        with patch.object(release, "inspect", side_effect=lambda tag: bad_value if tag == bad_tag else inspect(tag)), patch.object(release, "copy") as copy:
            rejects(release.promote, "1.2.3", sha)
            copy.assert_not_called()


def check_branch_races():
    sha = "a" * 40
    images = {variant: {"digest": "sha256:" + digit * 64, "revision": sha} for variant, digit in [("standard", "1"), ("alpine", "2")]}
    for branch, channel in [("main", "latest"), ("dev", "dev")]:
        with patch.object(release, "command", return_value=sha), patch.object(release, "copy") as copy:
            release.update_branch(branch, sha, images)
            assert [call.args[1] for call in copy.call_args_list] == [":" + channel, ":" + channel + "-alpine"]
    with patch.object(release, "command", return_value="b" * 40), patch.object(release, "copy") as copy:
        release.update_branch("main", sha, images)
        copy.assert_not_called()
    with patch.object(release, "command", return_value=""), patch.object(release, "copy") as copy:
        rejects(release.update_branch, "main", sha, images)
        copy.assert_not_called()


def check_existing_commit_images():
    sha = "a" * 40
    original = {"digest": "sha256:" + "1" * 64, "version": "1.2.3", "revision": sha}
    archive = '{"Digest":"sha256:' + "2" * 64 + '","Labels":{"org.opencontainers.image.revision":"' + sha + '","org.opencontainers.image.version":"1.2.3"}}'
    with patch.object(release, "command", return_value=archive) as command, patch.object(release, "inspect", return_value=original), patch.object(release, "update_branch") as update:
        release.publish(Path("/archives"), "main", sha)
        assert all(call.args[1] == "inspect" for call in command.call_args_list)
        images = update.call_args.args[2]
        assert all(image["digest"] == original["digest"] for image in images.values())
    for existing in [dict(original, revision="b" * 40), dict(original, version="1.2.4")]:
        with patch.object(release, "command", return_value=archive) as command, patch.object(release, "inspect", return_value=existing), patch.object(release, "update_branch") as update:
            rejects(release.publish, Path("/archives"), "main", sha)
            update.assert_not_called()
            assert all(call.args[1] == "inspect" for call in command.call_args_list)


def check_registry_failures():
    with patch.object(release, "command", side_effect=RuntimeError("manifest unknown")):
        assert release.inspect("missing") is None
    for failure in ["unauthorized", "connection refused", "rate limit exceeded"]:
        with patch.object(release, "command", side_effect=RuntimeError(failure)):
            rejects(release.inspect, "stable")


def check_archive_publication():
    sha = "a" * 40
    images = {variant: {"digest": "sha256:" + digit * 64, "version": "1.2.3", "revision": sha} for variant, digit in [("standard", "1"), ("alpine", "2")]}
    registry = {}

    def command(*args):
        if args[:2] == ("skopeo", "inspect"):
            variant = Path(args[-1]).stem
            image = images[variant]
            return json.dumps({"Digest": image["digest"], "Labels": {"org.opencontainers.image.revision": sha, "org.opencontainers.image.version": "1.2.3"}})
        if args[0] == "gh":
            return sha
        assert args[:4] == ("skopeo", "copy", "--all", "--preserve-digests")
        source, destination = args[-2:]
        if source.startswith("oci-archive:"):
            image = images[Path(source).stem]
        else:
            image = next(image for image in images.values() if source.endswith("@" + image["digest"]))
        registry[destination.rsplit(":", 1)[1]] = image
        return ""

    with patch.object(release, "command", side_effect=command), patch.object(release, "inspect", side_effect=registry.get):
        release.publish(Path("/archives"), "main", sha)
    assert registry == {
        "sha-" + sha: images["standard"], "sha-" + sha + "-alpine": images["alpine"],
        "latest": images["standard"], "latest-alpine": images["alpine"],
    }


def check_git_ancestry():
    with TemporaryDirectory() as directory:
        root = Path(directory)

        def git(*args):
            return subprocess.check_output(["git", "-c", "user.name=Release test", "-c", "user.email=release-test@example.invalid", *args], cwd=root, text=True).strip()

        git("init", "--quiet", "--initial-branch=main")
        git("commit", "--quiet", "--allow-empty", "-m", "baseline")
        git("update-ref", "refs/remotes/origin/main", "HEAD")
        with patch.object(release, "ROOT", root):
            assert release.validate_release("v1.2.3", "1.2.3") == "1.2.3"
            git("switch", "--quiet", "-c", "personal")
            git("commit", "--quiet", "--allow-empty", "-m", "outside main")
            rejects(release.validate_release, "v1.2.3", "1.2.3")


check_versions()
check_release_boundary()
check_promotion()
check_branch_races()
check_existing_commit_images()
check_registry_failures()
check_archive_publication()
check_git_ancestry()
print("Release versions, promotion, immutability, and branch race checks passed")
