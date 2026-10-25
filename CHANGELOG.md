# Release notes

## 1.0.0

SlopDrive's first stable release supports SQLite or PostgreSQL for metadata and keys, with HTML stored in that database or through an S3-compatible endpoint. PostgreSQL deployments can run multiple replicas without a shared filesystem. Garage is the tested self-hosted S3 reference.

- Publish and update pages through HTTP or MCP. Base URLs follow the latest version; hash-selected URLs retain their content.
- Keep private publisher keys hashed, require authentication for writes and history, and serve known page URLs publicly with browser isolation.
- Use the same configurable backends in the standard and Alpine images, on Docker or Podman.
- Apply one shared migration history through provider-specific SQL rendering. Version 1.0.0 establishes the supported baseline for future upgrades.
- Update security dependencies and license SlopDrive under AGPL-3.0-only.

Earlier development databases are not upgraded to this baseline. [Recreation](docs/database.md#schema-and-backups) loses their metadata, keys and S3 references. This exception applies to development databases, not future upgrades of released 1.x data. Back up before changing a deployment.

Moving between database providers or content stores requires a separate migration procedure and is not provided in this release. See the [compatibility policy](docs/releases.md#compatibility).
