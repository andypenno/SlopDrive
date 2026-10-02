import json
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
assert len(sys.argv) > 1, "pass the Compose command, for example docker compose"
command = [*sys.argv[1:], "-f", str(ROOT / "containers/compose.yaml"), "config", "--format", "json"]
volume_names = set()
for tag in ["0.1.0", "0.1.0-alpine", "stable", "stable-alpine", "latest", "latest-alpine", "dev", "dev-alpine"]:
    settings = dict(os.environ, SLOPBOX_VERSION=tag, BASE_URL="https://pages.example.com/")
    result = subprocess.check_output(command, env=settings, text=True, timeout=30)
    config = json.loads(result)
    service = config["services"]["slopbox"]
    assert service["image"] == "ghcr.io/andypenno/slopbox:" + tag
    assert service["environment"]["BASE_URL"] == settings["BASE_URL"]
    assert service["ports"][0]["host_ip"] == "127.0.0.1"
    assert str(service["ports"][0]["published"]) == "3000"
    assert service["ports"][0]["target"] == 3000
    assert service["volumes"][0]["target"] == "/data"
    assert service["stop_grace_period"] == "15s"
    volume_names.add(config["volumes"]["slopbox-data"]["name"])
assert len(volume_names) == 1
for missing in ["SLOPBOX_VERSION", "BASE_URL"]:
    settings = dict(os.environ, SLOPBOX_VERSION="0.1.0", BASE_URL="https://pages.example.com/")
    settings.pop(missing)
    result = subprocess.run(command, env=settings, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and missing in result.stderr
print("Compose image selection, storage, port binding, and required settings checks passed")
