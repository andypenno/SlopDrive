# HTTP API conventions

Keep transport handlers thin. Parse requests, call the shared operations, and translate results into HTTP responses. Apply the shared-surface rule in [AGENTS.md](../AGENTS.md) when changing behavior.

Authenticate before parsing protected request bodies. Apply the access and enumeration policy in [AGENTS.md](../AGENTS.md). Clients must not choose IDs for new pages.

Accept one UTF-8 multipart `file` field with content type `text/html`. Validate decoded content with the shared HTML validator. Bound the request body as well as the decoded content; multipart overhead must not bypass limits.

Use 400 for invalid uploads, 401 for missing or invalid keys, and 404 for missing pages or versions. Identifier rejection must not reveal filesystem paths. Log internal failures privately and return a generic 500 response.

Apply the URL policy in [AGENTS.md](../AGENTS.md). The base URL follows the latest content; a hash-selected URL preserves a version. Construct URLs from the configured origin, never the request's Host header. Keep immutable caching separate from latest-page caching, and preserve HTML MIME and security headers.
