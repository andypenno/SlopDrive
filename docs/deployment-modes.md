# Deployment and storage contract

This is the contract for supported deployment modes. SQLite and PostgreSQL support inline and S3 HTML storage. [Database configuration](database.md) and [HTML content storage](content-storage.md) describe the settings and upgrade boundaries. Bulk migration between stores is not provided.

## Responsibilities

The database owns API key hashes, upload IDs, version hashes, timestamps, sizes, version ordering and the latest-version pointer. It coordinates concurrent publication and records where each version's HTML is stored.

Content storage owns the HTML bytes. It is either the same database or an S3-compatible endpoint. HTTP, MCP and CLI operations use the same database and storage configuration within one Rust binary.

## Supported combinations

| Database | HTML storage | Deployment model | Availability |
| --- | --- | --- | --- |
| SQLite | Same SQLite database | Default. One host with persistent local storage. | Implemented |
| PostgreSQL | Same PostgreSQL database | Multiple SlopDrive replicas without a shared filesystem. | Implemented |
| SQLite | S3-compatible endpoint | One SlopDrive host with external HTML storage. SQLite remains local. | Implemented |
| PostgreSQL | S3-compatible endpoint | Multiple SlopDrive replicas with external metadata and HTML storage. | Implemented |

SQLite must use a local filesystem, not a shared network mount. Multiple processes on that host can access the database, including CLI key management. External HTML storage does not make SQLite a distributed database.

PostgreSQL modes permit replicas on different hosts. Every replica must use the same database and schema, public base URL and content storage configuration. No sticky sessions or local HTML copies are required. Connection limits must account for the combined pools of every replica.

PostgreSQL with inline HTML remains supported. S3 separates content traffic and retention from the database; it is not required for replication of the application.

## Filesystem HTML storage

A separate backend of loose HTML files is outside this contract. SQLite already provides local persistence, versioning and atomic publication in the default mode. A filesystem backend would add file and database coordination without an identified requirement for direct file access.

Node-local HTML files are unsuitable for replicas on different hosts. A shared filesystem backend would need its own concurrency and durability contract before becoming supported.

## Shared behavior

| Concern | Required behavior |
| --- | --- |
| Access | Writes and version lists require authentication. Reads by known upload ID are public. There is no global upload listing. |
| Version identity | A content hash identifies a version within an upload. Distinct content adds a version. Repeated updates with identical content and historical restores retain the original timestamp and size. |
| Publication | A successful write means the selected bytes and metadata are durable. A failure before metadata publication preserves the previous latest selection and history. |
| Concurrency | Independent writers retain every distinct committed version. The database serializes publication for an upload and selects latest in that order. Request arrival order does not determine latest. |
| Integrity | Public reads validate bytes against the recorded hash and size. Retrying an update with the same content repairs missing or corrupted bytes without duplicating version metadata. |
| Failure policy | Missing upload or version metadata returns not found. Missing or corrupt bytes for recorded content are an internal storage failure. Backend failures never silently select another backend or older version. |
| URLs | Uploads and version lists return SlopDrive base and versioned URLs. Base URLs follow latest. Versioned URLs retain their content. HTML delivery preserves SlopDrive security headers and cache policy. |
| Keys | Store only key hashes and query the database for authentication. Revocation takes effect across replicas. Fail closed on invalid records or database errors. |

Version lists validate metadata and latest membership without fetching every historical HTML body. URL construction does not establish that content storage is healthy. Public reads perform byte validation.

An uncertain database commit remains an explicit uncertain result. HTTP returns 503 and MCP reports that the write may have committed. Retrying identical content against a known upload ID must not duplicate its version. Retrying a create after losing its response is not an exactly-once operation; the caller may not know the allocated ID.

## S3 protocol contract

Use a configurable endpoint, bucket, signing region and credentials, with configurable path-style or virtual-hosted addressing. Support Signature Version 4 without requiring an AWS account or AWS endpoint discovery. Secrets must not appear in transport errors or logs. Verify TLS by default; plaintext HTTP requires explicit configuration for local or private deployments.

Garage is the primary self-hosted integration target. AWS S3 and MinIO are compatibility targets through the same protocol implementation, without provider-specific application branches. Compatibility claims require tests against the named implementation and version. The required runtime subset is whole-object PUT and GET; do not require bucket versioning, bucket creation, ACL management, AWS policy APIs, object listing or multipart uploads. The existing 1 MiB page limit remains.

The endpoint must provide durable acknowledged writes and read-after-write consistency across the endpoints used by replicas. Garage deployments must use its consistent mode. Provision the bucket and permissions outside SlopDrive. Keep objects private and deliver HTML through SlopDrive so public URLs retain the service's browser isolation policy.

S3 and SQL do not share an atomic transaction. Store bytes before publishing their metadata. A failed or uncertain operation may leave an unreferenced private object, but must not publish metadata before its bytes are stored. Concurrent retries must not overwrite a version with different bytes. Automatic object deletion and orphan collection are outside the initial contract.

## Configuration changes, migration and backups

Changing configuration must not silently move, discard or reinterpret existing content. Record enough location information to resolve stored versions and reject incompatible configuration. Migration between databases or content stores requires an explicit procedure that preserves upload IDs, hashes, timestamps, ordering, latest selections and key hashes. Define and test that procedure before advertising a backend switch as supported.

This pre-1.x baseline replaces earlier schema histories without an upgrade path. Older development databases must be [recreated manually](database.md#schema-and-backups). SlopDrive never resets databases, exports HTML or deletes S3 objects automatically. A rollback requires a binary compatible with the schema and recorded content locations, or a tested backup restore.

Inline modes need a database backup. S3 modes need the database and every referenced object. A database backup alone cannot restore externally stored HTML. Coordinate backups to retain the objects referenced by the database snapshot, and test restoration before relying on them. Never apply expiration rules that can remove recorded versions.

## Verification contract

Run shared storage, authentication and paired HTTP/MCP contracts for each supported combination. S3 coverage must include missing and corrupt objects, retry repair, failed PUTs, failed metadata publication, uncertain acknowledgements and concurrent identical updates.

Prove distributed behavior with independent SlopDrive processes using PostgreSQL and the same content store. Publish through one replica and read through another, retain concurrent distinct versions, revoke keys across replicas and verify persistence after restarting replicas without local data. Exercise both PostgreSQL content modes. External integration runs follow the approval and fixture rules in [testing conventions](testing.md).

Garage's [S3 compatibility reference](https://github.com/deuxfleurs-org/garage/blob/main-v2/doc/book/reference-manual/s3-compatibility.md) and [consistency configuration](https://github.com/deuxfleurs-org/garage/blob/main-v2/doc/book/reference-manual/configuration.md) describe the protocol subset and deployment requirements. Pin a tested Garage version in integration fixtures rather than assuming every S3-compatible server has identical behavior.
