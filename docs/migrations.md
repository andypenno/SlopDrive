# Database migrations

`src/database/migrations/v001.rs` defines the complete baseline for uploads, versions, key hashes and S3 bindings. `mod.rs` defines the migration ledger and ordered registry. Add schema changes once in the shared definitions. Do not keep SQLite and PostgreSQL SQL histories.

The model supports table creation, typed columns, nullability, primary and unique keys, foreign keys, numeric bounds, canonical hex text and exclusive inline/object content locations. SQLite renders STRICT tables and inline foreign keys. PostgreSQL renders BIGINT/BYTEA and adds foreign keys after creating tables to support the uploads/versions cycle. Both providers defer latest-version membership until commit.

## Checksums and application

Each ledger checksum hashes UTF-8 JSON for the ordered tuple `("slopbox-schema-v1", LEDGER, migration)` using SHA-256. Serialization uses declaration-order struct fields, enum names and ordered slices. There are no maps or provider SQL strings in this encoding. Field and variant names, ordering, ledger definitions and encoding version are persisted format. Changing them changes checksums. Freeze applied definitions; append new versions in contiguous order starting at 1.

Initialization validates every recorded version and checksum before applying pending migrations. SQLite holds `BEGIN IMMEDIATE`; PostgreSQL holds a schema-scoped transaction advisory lock. Schema operations, ledger inserts and S3 binding share the startup transaction. Failure rolls back pending DDL and ledger entries. No migration runner resets a database or removes objects.

Renderer changes do not change logical checksums. Verify their effect on both providers before shipping them. Do not use renderer changes to alter an already-applied schema silently.

## Future changes

Add only operations required by an actual migration. There is no general schema-diff engine, down-migration system or SQLite table-rebuild planner. If a future change needs ALTER or a rebuild, add its shared operation and provider implementations together with rollback and data-preservation tests. Shared runner tests use a test-only second table-creation migration.

The initial baseline intentionally rejects older pre-1.x histories. Follow [database recreation requirements](database.md#schema-and-backups) rather than editing checksums or fabricating ledger entries.
