# SlopBox

[![Main CI](https://github.com/andypenno/SlopBox/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/andypenno/SlopBox/actions/workflows/ci.yml?query=branch%3Amain)
[![Dev CI](https://github.com/andypenno/SlopBox/actions/workflows/ci.yml/badge.svg?branch=dev&event=push)](https://github.com/andypenno/SlopBox/actions/workflows/ci.yml?query=branch%3Adev)
[![Release promotion](https://github.com/andypenno/SlopBox/actions/workflows/release.yml/badge.svg)](https://github.com/andypenno/SlopBox/actions/workflows/release.yml)
[![Latest version](https://img.shields.io/github/v/tag/andypenno/SlopBox?filter=v*&sort=semver&label=version)](https://github.com/andypenno/SlopBox/tags)
[![Standard container](https://img.shields.io/badge/GHCR-stable-blue)](https://github.com/andypenno/SlopBox/pkgs/container/slopbox)
[![Alpine container](https://img.shields.io/badge/GHCR-stable--alpine-blue)](https://github.com/andypenno/SlopBox/pkgs/container/slopbox)

SlopBox lets you publish self-contained HTML pages and share them with a link. The same link shows later updates. Anyone with the link can view a page; publishing requires an API key.

Published HTML is untrusted. Pages run in a browser sandbox that permits scripts but gives each document an opaque origin. localStorage and IndexedDB are unavailable, and requests that depend on same-origin access need explicit CORS support. Keep interactive state in memory or offer file import and export.

## Run in a container

Use Docker Compose with [containers/compose.yaml](containers/compose.yaml). Set `SLOPBOX_VERSION` to a published version and `BASE_URL` to the public HTTPS address served by your reverse proxy. The version below is an example; it must exist in [GHCR](https://github.com/andypenno/SlopBox/pkgs/container/slopbox) before you can pull it. Private packages require `docker login ghcr.io` with a token that has `read:packages`; public packages need no login.

```sh
export SLOPBOX_VERSION=0.1.0
export BASE_URL=https://pages.example.com/
docker compose -f containers/compose.yaml pull
docker compose -f containers/compose.yaml up -d
```

Use `0.1.0-alpine` for the Alpine variant. Both variants target Linux amd64 and use the same data format. `stable` tracks the highest versioned release, `latest` tracks successful `main` builds, and `dev` tracks successful `dev` builds. Each channel also has an `-alpine` tag. Pin an exact version for controlled updates; see the [release strategy](docs/releases.md).

The image runs as UID/GID 10001 and writes to `/data`. If you use a bind mount, make that directory writable by the container's user. The reverse proxy handles TLS and forwards the public origin to port 3000. Restrict network access to your trusted network or VPN.

Every API key can update any known page and list its history. Keys are instance-wide publisher credentials. Give them only to publishers who share one trust domain; use separate instances for independent users.

Create a key locally using the same volume. The command prints its ID and plaintext key once:

```sh
docker compose -f containers/compose.yaml run --rm --no-deps slopbox keys create
docker compose -f containers/compose.yaml run --rm --no-deps slopbox keys list
docker compose -f containers/compose.yaml run --rm --no-deps slopbox keys revoke KEY_ID
```

Revoking a key takes effect without a restart. Back up the data volume to retain your pages and keys.

To update, change `SLOPBOX_VERSION`, then repeat `pull` and `up -d`. Keep the data volume; `down --volumes` deletes it. Check release notes for storage compatibility before rolling back.

### Build locally

If no versioned image is available yet, build from the repository root. Both image definitions use that directory as their build context:

```sh
docker build -f containers/Containerfile -t slopbox .
docker build -f containers/Containerfile.alpine -t slopbox:alpine .
docker volume create slopbox-data
docker run -d --name slopbox --stop-timeout 15 -p 127.0.0.1:3000:3000 \
  -v slopbox-data:/data -e BASE_URL=https://pages.example.com/ slopbox
docker run --rm -v slopbox-data:/data slopbox keys create
```

For Podman builds, pass the matching ignore file explicitly:

```sh
podman build --ignorefile containers/Containerfile.dockerignore -f containers/Containerfile -t slopbox .
podman build --ignorefile containers/Containerfile.alpine.dockerignore -f containers/Containerfile.alpine -t slopbox:alpine .
```

Replace `docker` with `podman` in the run commands. The Compose example requires a Compose provider installed for `podman compose`.

History is unlimited. Set filesystem quotas for the mounted data, monitor free space, and plan backups as histories grow. Keep existing versions when a quota is reached; writes can fail until the operator provides more space. Reads and version lists parse the full history, so large histories also increase memory and request work. Configure proxy rate, connection, and request-timeout limits, including public reads. Allow up to 1 MiB + 16 KiB for multipart uploads and 6 MiB + 16 KiB for MCP JSON envelopes. SlopBox bounds each decoded page to 1 MiB and admits at most 16 blocking operations at once.

## Connect your agent

Add your instance to your agent's MCP configuration. Replace the example URL with your public address and `YOUR_API_KEY` with the key you created:

```json
{
  "mcpServers": {
    "slopbox": {
      "url": "https://pages.example.com/mcp",
      "headers": { "Authorization": "Bearer YOUR_API_KEY" }
    }
  }
}
```

Install the `skills/slopbox` directory through your agent's skill loader. Keep the connection name `slopbox` so the skill can use it. You can then ask your agent to publish an HTML page and share the returned link.
