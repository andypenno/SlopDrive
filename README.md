# SlopBox

SlopBox lets you publish self-contained HTML pages and share them with a link. The same link shows later updates. Anyone with the link can view a page; publishing requires an API key.

## Run in a container

Build the image with Docker or Podman:

```sh
docker build -f Containerfile -t slopbox .
# or
podman build -f Containerfile -t slopbox .
```

Create a volume for your data, then start the service. Set `BASE_URL` to the public HTTPS address served by your reverse proxy. Replace `docker` with `podman` for Podman:

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

Revoking a key takes effect without a restart. Back up the data volume to retain your pages and keys.

## Connect your agent

Add your instance to your agent's MCP configuration. Replace the example URL with your public address and `YOUR_API_KEY` with the key you created:

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

Install the `skills/slopbox` directory through your agent's skill loader. Keep the connection name `html_host` so the skill can use it. You can then ask your agent to publish an HTML page and share the returned link.
