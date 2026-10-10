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
command = [*sys.argv[1:], "-f", str(ROOT / "containers/compose.postgres.yaml"), "config", "--format", "json"]
settings = dict(os.environ, SLOPBOX_VERSION="0.1.0-alpine", BASE_URL="https://pages.example.com/", DATABASE_PASSWORD="compose-test-only")
config = json.loads(subprocess.check_output(command, env=settings, text=True, timeout=30))
service = config["services"]["slopbox"]
database = config["services"]["postgres"]
assert service["image"] == "ghcr.io/andypenno/slopbox:0.1.0-alpine"
assert service["environment"]["DATABASE_PROVIDER"] == "postgres"
assert service["environment"]["DATABASE_HOST"] == "postgres"
assert service["environment"]["DATABASE_PASSWORD"] == database["environment"]["POSTGRES_PASSWORD"] == settings["DATABASE_PASSWORD"]
assert service["environment"]["DATABASE_TLS_MODE"] == "disable"
assert service["depends_on"]["postgres"]["condition"] == "service_healthy"
assert service["ports"][0]["host_ip"] == "127.0.0.1"
assert service["stop_grace_period"] == "15s"
assert not database.get("ports")
assert database["volumes"][0]["target"] == "/var/lib/postgresql/data"
for missing in ["SLOPBOX_VERSION", "BASE_URL", "DATABASE_PASSWORD"]:
    incomplete = settings.copy()
    incomplete.pop(missing)
    result = subprocess.run(command, env=incomplete, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and missing in result.stderr
print("SQLite and PostgreSQL Compose configuration checks passed")
