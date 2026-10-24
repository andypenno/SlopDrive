# HTML content storage

`CONTENT_STORAGE=database` keeps HTML in the selected SQLite or PostgreSQL database. This remains the default. `CONTENT_STORAGE=s3` stores new HTML versions through an S3-compatible endpoint while the database retains keys, version metadata and latest selections. See [deployment modes](deployment-modes.md) for the four combinations.

S3 support requires a build containing this change; the published 0.2.0 release does not include it. [The S3 Compose example](../containers/compose.s3.yaml) connects PostgreSQL and SlopBox to an externally provisioned endpoint. It works with Docker Compose or a Podman Compose provider. Set `SLOPBOX_VERSION` to an image built with S3 support, not 0.2.0.

The standard and Alpine images both include every backend. Select the database with `DATABASE_PROVIDER` and HTML storage with `CONTENT_STORAGE`; no backend-specific image is required. The Compose examples configure these same two image variants.

## Configuration

| Setting | Default | Meaning |
| --- | --- | --- |
| `CONTENT_STORAGE` | `database` | `database` or `s3`, independent of `DATABASE_PROVIDER` |
| `S3_ENDPOINT` | required for S3 | HTTP(S) origin without a path, credentials, query or fragment |
| `S3_BUCKET` | required for S3 | Existing private bucket, 3 through 63 lowercase letters, digits, dots or hyphens; begin and end with a letter or digit and omit consecutive dots |
| `S3_REGION` | required for S3 | Signing region configured by the endpoint, such as `garage` |
| `S3_ACCESS_KEY_ID` | required for S3 | Access key, environment only |
| `S3_SECRET_ACCESS_KEY` | required for S3 | Secret key, environment only |
| `S3_SESSION_TOKEN` | unset | Optional temporary-credential token, environment only |
| `S3_PREFIX` | `slopbox` | Object-key prefix; empty is permitted, leading/trailing slashes and traversal components are rejected |
| `S3_PATH_STYLE` | `true` | Path-style addressing; `false` prepends the bucket to the endpoint hostname |
| `S3_ALLOW_HTTP` | `false` | Explicit permission for plaintext HTTP on a private network or in tests |
| `S3_TLS_CA_FILE` | system CA roots | Optional PEM CA bundle; certificate verification remains enabled |
| `S3_TIMEOUT_MS` | `3000` | Whole PUT or GET deadline, including body reads and retries, from 1 through 5000 ms |

S3 settings are rejected in database content mode. Invalid settings fail startup without falling back to another store. Endpoints are explicit, including AWS endpoints. SlopBox uses Signature Version 4 with the configured credentials; it does not discover an AWS account or contact an instance metadata service.

`STORAGE_PATH` still identifies the directory for the default SQLite database. It does not select a loose-file HTML backend. CLI key commands receive the same database and S3 configuration as `serve`, but perform no object requests. Secrets do not have command-line flags and backend errors do not expose endpoint responses.

## Garage and other endpoints

Garage is the primary self-hosted integration target. Follow Garage's [quick start](https://garagehq.deuxfleurs.fr/documentation/quick-start/) to provision the service, assign storage and create a private bucket. Create an access key with read and write access to that bucket. Set `S3_REGION` to the Garage API's configured region and use its S3 API endpoint, not its admin or website endpoint. Use Garage's `consistent` mode for read-after-write consistency.

Garage 2.4.1 is the tested integration reference. The shared contracts and independent-process checks also exercised PostgreSQL 17.11. Other server versions require their own validation.

SlopBox requires whole-object PUT and GET only. It does not create buckets, change policies or ACLs, list objects, use bucket versioning or perform multipart uploads. AWS S3 and MinIO use the same client; protocol support alone is not evidence that a particular server version has passed integration tests. Keep the bucket private. Public page traffic goes through SlopBox to retain content validation, security headers and URL behavior.

The endpoint must acknowledge durable writes and provide read-after-write consistency across the endpoints used by replicas. TLS terminates at the object endpoint or its trusted reverse proxy. For a private HTTP endpoint, enable `S3_ALLOW_HTTP` explicitly; do not disable certificate validation to work around TLS errors.

For multiple SlopBox replicas, use PostgreSQL with identical schema, content settings and public base URL. Each replica needs its own listening address behind the reverse proxy. The example's fixed localhost port is for one replica; adapt that mapping or let the proxy discover individual replicas. S3 does not make a local SQLite database distributable.

## Existing data and upgrades

This pre-1.x build uses a fresh version-1 schema and rejects the earlier migration histories. Follow the [manual database recreation requirements](database.md#schema-and-backups) before starting it against an older development installation. Recreation loses metadata and S3 object references. It does not delete objects or import existing HTML. Stop SlopBox and CLI writers before recreation or a content-mode change.

Enabling S3 explicitly changes where new distinct versions are written. Existing inline versions stay inline, and retries or historical restores repair their recorded location. No automatic export occurs. One upload can therefore have both inline and external versions, which SlopBox resolves from their metadata.

The first S3 initialization binds the database to its normalized endpoint, bucket and prefix, and creates a random namespace within that prefix. Other replicas reuse it. Credentials can rotate without changing the binding. A different endpoint, bucket or prefix, or removing S3 configuration after binding, fails startup. Do not edit the binding manually. Moving a bucket, exporting inline history, switching databases and returning to inline-only operation require a separate migration procedure; none is provided here.

## Failure handling and backups

S3 writes finish before the SQL transaction publishes a new version. A failed PUT leaves metadata unchanged. A successful PUT followed by failed metadata publication can leave an unreferenced private object. SlopBox never deletes that object as compensation because another concurrent request may have published it. There is no automatic object collection or expiration.

Missing recorded objects, invalid size or hash, and endpoint failures produce generic storage errors. Reads never fall back to another location or an older version. Updating a known upload with the same HTML repairs missing or corrupt bytes without duplicating version metadata. A lost SQL commit acknowledgement retains the existing uncertain-write response; retry the same content against the known upload ID.

Back up both the database and the objects it references. Retain objects across the database snapshot and backup interval, and test restoring both into an isolated deployment. Disable expiration rules that could remove recorded history. SlopBox's application hash identifies a version; S3 ETags and provider-side bucket versions are not its version identifiers.
