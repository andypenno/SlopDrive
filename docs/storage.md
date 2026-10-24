# Storage and key conventions

The shared [Storage interface](../src/storage.rs) coordinates every durable record. HTTP, MCP and key management use provider-neutral types; SQL and driver errors belong under `src/database/`. S3 content operations live in `src/content.rs`. [Database configuration](database.md) and [HTML content storage](content-storage.md) cover settings, schema changes and backups.

Create and update transactions publish version metadata and latest selection together. Inline bytes join that transaction; S3 bytes are stored before taking publication locks. Pre-commit failures preserve existing history and leave no partial new upload, but may leave private unreferenced objects. Never delete those objects as compensation. Database constraints enforce canonical IDs, content hashes, sizes, unique versions, latest membership and exclusive inline/object locations. SQLite serializes writers with `BEGIN IMMEDIATE`; PostgreSQL locks the upload row for updates. Independent connections must retain every distinct concurrent version.

Distinct content adds one version. Same-content retries and historical restores retain the original timestamp and size. A retry repairs corrupted bytes without duplicating history. Public reads validate the selected bytes against their stored hash and size. Version lists validate metadata and latest membership.

Print plaintext keys only from the creation command. Store their hashes and query the database on every authentication attempt so cross-process revocation takes effect immediately. Invalid records and database failures fail closed. Do not expose raw database errors or credentials in either transport.

A lost commit acknowledgement is uncertain. HTTP returns 503 and MCP tells the caller that the write may have committed. Retry the same content. Verify durability by reopening the database, not only by inspecting the original connection.

SIGTERM and SIGINT stop new connections and allow up to 10 seconds for active requests and admitted parsing/database jobs to drain. Cancelling a request does not cancel its admitted database transaction. Configure the container stop grace period to at least 15 seconds. A drain timeout exits with an error; database transactions prevent partial publication.
