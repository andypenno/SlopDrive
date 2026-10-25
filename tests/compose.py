import json
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
environment = os.environ.copy()
environment.pop("COMPOSE_PROJECT_NAME", None)
assert len(sys.argv) > 1, "pass the Compose command, for example docker compose"
command = [*sys.argv[1:], "-f", str(ROOT / "containers/compose.yaml"), "config", "--format", "json"]
volume_names = set()
for tag in ["1.0.0", "1.0.0-alpine", "stable", "stable-alpine", "latest", "latest-alpine", "dev", "dev-alpine"]:
    settings = dict(environment, SLOPDRIVE_VERSION=tag, BASE_URL="https://pages.example.com/")
    result = subprocess.check_output(command, env=settings, text=True, timeout=30)
    config = json.loads(result)
    assert config["name"] == "slopdrive"
    service = config["services"]["slopdrive"]
    assert service["image"] == "ghcr.io/andypenno/slopdrive:" + tag
    assert service["environment"]["BASE_URL"] == settings["BASE_URL"]
    assert service["ports"][0]["host_ip"] == "127.0.0.1"
    assert str(service["ports"][0]["published"]) == "3000"
    assert service["ports"][0]["target"] == 3000
    assert service["volumes"][0]["target"] == "/data"
    assert service["stop_grace_period"] == "15s"
    volume_names.add(config["volumes"]["slopdrive-data"]["name"])
assert len(volume_names) == 1
for missing in ["SLOPDRIVE_VERSION", "BASE_URL"]:
    settings = dict(environment, SLOPDRIVE_VERSION="1.0.0", BASE_URL="https://pages.example.com/")
    settings.pop(missing)
    result = subprocess.run(command, env=settings, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and missing in result.stderr
command = [*sys.argv[1:], "-f", str(ROOT / "containers/compose.postgres.yaml"), "config", "--format", "json"]
settings = dict(environment, SLOPDRIVE_VERSION="1.0.0-alpine", BASE_URL="https://pages.example.com/", DATABASE_PASSWORD="compose-test-only")
config = json.loads(subprocess.check_output(command, env=settings, text=True, timeout=30))
assert config["name"] == "slopdrive"
service = config["services"]["slopdrive"]
database = config["services"]["postgres"]
assert service["image"] == "ghcr.io/andypenno/slopdrive:1.0.0-alpine"
assert service["environment"]["DATABASE_PROVIDER"] == "postgres"
assert service["environment"]["DATABASE_HOST"] == "postgres"
assert service["environment"]["DATABASE_PASSWORD"] == database["environment"]["POSTGRES_PASSWORD"] == settings["DATABASE_PASSWORD"]
assert service["environment"]["DATABASE_TLS_MODE"] == "disable"
assert service["depends_on"]["postgres"]["condition"] == "service_healthy"
assert service["ports"][0]["host_ip"] == "127.0.0.1"
assert service["stop_grace_period"] == "15s"
assert not database.get("ports")
assert database["volumes"][0]["target"] == "/var/lib/postgresql/data"
for missing in ["SLOPDRIVE_VERSION", "BASE_URL", "DATABASE_PASSWORD"]:
    incomplete = settings.copy()
    incomplete.pop(missing)
    result = subprocess.run(command, env=incomplete, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and missing in result.stderr
command = [*sys.argv[1:], "-f", str(ROOT / "containers/compose.s3.yaml"), "config", "--format", "json"]
settings.update(S3_ENDPOINT="https://objects.example.com/", S3_BUCKET="pages",
                S3_REGION="garage", S3_ACCESS_KEY_ID="compose-test-access",
                S3_SECRET_ACCESS_KEY="compose-test-secret")
config = json.loads(subprocess.check_output(command, env=settings, text=True, timeout=30))
assert config["name"] == "slopdrive"
service = config["services"]["slopdrive"]
assert service["environment"]["DATABASE_PROVIDER"] == "postgres"
assert service["environment"]["CONTENT_STORAGE"] == "s3"
assert service["environment"]["S3_ENDPOINT"] == settings["S3_ENDPOINT"]
assert service["environment"]["S3_PATH_STYLE"] == "true"
assert service["environment"]["S3_ALLOW_HTTP"] == "false"
assert not service.get("volumes")
assert not config["services"]["postgres"].get("ports")
for missing in ["S3_ENDPOINT", "S3_BUCKET", "S3_REGION", "S3_ACCESS_KEY_ID", "S3_SECRET_ACCESS_KEY"]:
    incomplete = settings.copy()
    incomplete.pop(missing)
    result = subprocess.run(command, env=incomplete, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and missing in result.stderr
print("SQLite, PostgreSQL and S3 Compose configuration checks passed")
