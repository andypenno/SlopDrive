# SlopDrive

[![Main CI](https://img.shields.io/github/actions/workflow/status/andypenno/SlopDrive/ci.yml?branch=main&event=push&label=Main%20CI)](https://github.com/andypenno/SlopDrive/actions/workflows/ci.yml?query=branch%3Amain+event%3Apush)
[![Dev CI](https://img.shields.io/github/actions/workflow/status/andypenno/SlopDrive/ci.yml?branch=dev&event=push&label=Dev%20CI)](https://github.com/andypenno/SlopDrive/actions/workflows/ci.yml?query=branch%3Adev+event%3Apush)
[![Latest version](https://img.shields.io/github/v/tag/andypenno/SlopDrive?filter=v*&sort=semver&label=Latest%20version)](https://github.com/andypenno/SlopDrive/tags)
[![Standard image tag](https://img.shields.io/badge/Standard%20image-stable-blue)](https://github.com/andypenno/SlopDrive/pkgs/container/slopdrive)
[![Alpine image tag](https://img.shields.io/badge/Alpine%20image-stable--alpine-blue)](https://github.com/andypenno/SlopDrive/pkgs/container/slopdrive)

SlopDrive lets you publish self-contained HTML pages and share them with a link. The same link shows later updates. Anyone with the link can view a page; publishing requires an API key.

Published HTML is untrusted. Pages run in a browser sandbox that permits scripts but gives each document an opaque origin. localStorage and IndexedDB are unavailable, and requests that depend on same-origin access need explicit CORS support. Keep interactive state in memory or offer file import and export.

## Run in a container

Use Docker Compose with [containers/compose.yaml](containers/compose.yaml). Set `SLOPDRIVE_VERSION` to an available image tag and `BASE_URL` to the public HTTPS address served by your reverse proxy. The tag below is an example; it must exist in [GHCR](https://github.com/andypenno/SlopDrive/pkgs/container/slopdrive) before you can pull it. Private packages require `docker login ghcr.io` with a token that has `read:packages`; public packages need no login.

```sh
export SLOPDRIVE_VERSION=1.0.0
export BASE_URL=https://pages.example.com/
docker compose -f containers/compose.yaml pull
docker compose -f containers/compose.yaml up -d
```

Use `1.0.0-alpine` for the Alpine variant. Both variants target Linux amd64 and use the same data format. `stable` tracks the highest versioned release, `latest` tracks successful `main` builds, and `dev` tracks successful `dev` builds. Each channel also has an `-alpine` tag. Pin an exact version for controlled updates; see the [release strategy](docs/releases.md).

The image runs as UID/GID 10001 and writes to `/data`. If you use a bind mount, make that directory writable by the container's user. The reverse proxy handles TLS and forwards the public origin to port 3000. Restrict network access to your trusted network or VPN.

Every API key can update any known page and list its history. Keys are instance-wide publisher credentials. Give them only to publishers who share one trust domain; use separate instances for independent users.

Create a key locally using the same volume. The command prints its ID and plaintext key once:

```sh
docker compose -f containers/compose.yaml run --rm --no-deps slopdrive keys create
docker compose -f containers/compose.yaml run --rm --no-deps slopdrive keys list
docker compose -f containers/compose.yaml run --rm --no-deps slopdrive keys revoke KEY_ID
```

Revoking a key takes effect without a restart. SQLite stores pages, history and key hashes in `/data/slopdrive.sqlite` by default. PostgreSQL is also supported through environment settings and an [example Compose file](containers/compose.postgres.yaml). See [database configuration and backups](docs/database.md).

SlopDrive can store HTML through a configurable S3-compatible endpoint while SQLite or PostgreSQL retains metadata and keys. See [HTML content storage](docs/content-storage.md) for Garage configuration, the PostgreSQL/S3 Compose example and upgrade requirements.

Version 1.0.0 establishes the supported migration baseline. Older development databases require [manual recreation](docs/database.md#schema-and-backups), which loses metadata, keys and S3 object references. Upgrades within 1.x preserve existing data through the shared migrations; see the [compatibility policy](docs/releases.md#compatibility).

To update a compatible installation, change `SLOPDRIVE_VERSION`, then repeat `pull` and `up -d`. Keep the data volume; `down --volumes` deletes it. Check release notes for storage compatibility before rolling back.

### Build locally

If no versioned image is available yet, build from the repository root. Both image definitions use that directory as their build context:

```sh
docker build -f containers/Containerfile -t slopdrive .
docker build -f containers/Containerfile.alpine -t slopdrive:alpine .
docker volume create slopdrive-data
docker run -d --name slopdrive --stop-timeout 15 -p 127.0.0.1:3000:3000 \
  -v slopdrive-data:/data -e BASE_URL=https://pages.example.com/ slopdrive
docker run --rm -v slopdrive-data:/data slopdrive keys create
```

For Podman builds, pass the matching ignore file explicitly:

```sh
podman build --ignorefile containers/Containerfile.dockerignore -f containers/Containerfile -t slopdrive .
podman build --ignorefile containers/Containerfile.alpine.dockerignore -f containers/Containerfile.alpine -t slopdrive:alpine .
```

Replace `docker` with `podman` in the run commands. The Compose example requires a Compose provider installed for `podman compose`.

History is unlimited. Set filesystem quotas for the mounted data, monitor free space, and plan backups as histories grow. Keep existing versions when a quota is reached; writes can fail until the operator provides more space. Version lists load the full history metadata, so large histories increase memory and request work. Public reads load only the selected page. Configure proxy rate, connection, and request-timeout limits, including public reads. Allow up to 1 MiB + 16 KiB for multipart uploads and 6 MiB + 16 KiB for MCP JSON envelopes. SlopDrive bounds each decoded page to 1 MiB and admits at most 16 parsing or database operations at once.

## Connect your agent

Add your instance to your agent's MCP configuration. Replace the example URL with your public address and `YOUR_API_KEY` with the key you created:

```json
{
  "mcpServers": {
    "slopdrive": {
      "url": "https://pages.example.com/mcp",
      "headers": { "Authorization": "Bearer YOUR_API_KEY" }
    }
  }
}
```

Install the `skills/slopdrive` directory through your agent's skill loader. Keep the connection name `slopdrive` so the skill can use it. You can then ask your agent to publish an HTML page and share the returned link.

## Licence

SlopDrive is licensed under [AGPL-3.0-only](LICENSE). If you modify SlopDrive and let users interact with it over a network, offer those users the corresponding source as required by the licence. Dependency licences remain applicable. See the [1.0.0 release notes](CHANGELOG.md).
