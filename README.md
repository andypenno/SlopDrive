# SlopBox

SlopBox hosts single-file HTML pages at random, non-sequential URLs. A page's base URL always serves its latest content. A URL with `?q=<sha256-hash>` serves one immutable version. Viewers need the URL; uploads and version lists need an API key. There is no directory of pages.

## Run in a container

Build the OCI image with either engine:

```sh
docker build -f Containerfile -t slopbox .
# or
podman build -f Containerfile -t slopbox .
```

Create a persistent named volume, then start the service. Set `BASE_URL` to the public HTTPS origin served by your reverse proxy. Replace `docker` with `podman` for Podman:

```sh
docker volume create slopbox-data
docker run -d --name slopbox -p 127.0.0.1:3000:3000 \
  -v slopbox-data:/data \
  -e BASE_URL=https://pages.example.com/ \
  slopbox
```

The image runs as UID/GID 10001 and writes to `/data`. If you use a bind mount, make that directory writable by the container's user. The reverse proxy handles TLS and forwards the public origin to port 3000. Restrict network access to your trusted network or VPN.

Create a key locally using the same volume. The command prints its ID and plaintext key once:

```sh
docker run --rm -v slopbox-data:/data slopbox keys create
docker run --rm -v slopbox-data:/data slopbox keys list
docker run --rm -v slopbox-data:/data slopbox keys revoke KEY_ID
```

The server reads key records on each authenticated request, so revocation takes effect without a restart. Only SHA-256 hashes of keys are stored. Back up the entire `/data` volume to retain pages, versions, and keys.

The image's default command is `serve`. `STORAGE_PATH` defaults to `/data`, `BIND_ADDR` to `0.0.0.0:3000`, and `BASE_URL` is required. Logs are JSON on stdout. The same commands work outside a container with `cargo run -- ...`.

## HTTP API

Send one UTF-8 HTML file of at most 1 MiB in a multipart `file` field with content type `text/html`. Uploads and version lists require `Authorization: Bearer <API_KEY>`.

| Method | Path | Result |
| --- | --- | --- |
| `POST` | `/upload` | Create a page with a random ID |
| `POST` | `/upload/{upload_id}` | Publish a version or return the existing latest version for an identical retry |
| `GET` | `/upload/{upload_id}/versions` | List versions of a known page |
| `GET` | `/{upload_id}` | Read the latest HTML without authentication |
| `GET` | `/{upload_id}?q={content_hash}` | Read one historical version without authentication |

Both upload responses contain `upload_id`, `content_hash`, `url`, and `versioned_url`. Each version-list entry contains `content_hash`, `created_at`, `size_bytes`, and both URLs. The base URL is the link to share when the viewer should see updates.

## MCP and agent skill

The authenticated streamable HTTP MCP endpoint is `/mcp`. Configure an MCP client with that URL and a bearer header:

```json
{
  "mcpServers": {
    "html_host": {
      "url": "https://pages.example.com/mcp",
      "headers": { "Authorization": "Bearer YOUR_API_KEY" }
    }
  }
}
```

Its tools are `upload_html`, `update_html`, `list_versions`, and `get_html_url`. There is no tool to enumerate pages. The Hermes skill is at `skills/slopbox/SKILL.md`; install that directory through your agent's skill loader and configure its MCP server name as `html_host` for the tool calls in the skill.
