# Database configuration

SQLite is the default. One database stores version history, latest selections and API key hashes. HTML bytes also live there unless [S3 content storage](content-storage.md) is selected. HTTP, MCP and CLI commands use the same backend and settings. Both providers implement the same contract. Pre-1.x databases from older schema histories are not imported; recreate development databases manually.

| Setting | Default | Meaning |
| --- | --- | --- |
| `DATABASE_PROVIDER` | `sqlite` | `sqlite` or `postgres` |
| `DATABASE_URL` | unset | SQLite file URL or PostgreSQL connection URL |
| `STORAGE_PATH` | `/data` | Directory for the default `slopdrive.sqlite` file |
| `DATABASE_HOST` | unset | PostgreSQL host when no URL is supplied |
| `DATABASE_PORT` | `5432` | PostgreSQL port |
| `DATABASE_NAME` | unset | PostgreSQL database name |
| `DATABASE_USER` | unset | PostgreSQL user |
| `DATABASE_PASSWORD` | unset | PostgreSQL password, environment only |
| `DATABASE_SCHEMA` | `slopdrive` | PostgreSQL schema, a lowercase SQL identifier, at most 63 bytes |
| `DATABASE_TLS_MODE` | `verify-full` | PostgreSQL TLS hostname and certificate verification, or explicit `disable` |
| `DATABASE_TLS_CA_FILE` | built-in CA roots | Optional PostgreSQL CA certificate file |
| `DATABASE_MAX_CONNECTIONS` | `4` | Pool size, from 1 through 32 |
| `DATABASE_CONNECT_TIMEOUT_MS` | `5000` | Connection, initialization and pool acquisition timeout, from 1 through 60000 ms |
| `DATABASE_QUERY_TIMEOUT_MS` | `5000` | PostgreSQL statement/lock timeout; SQLite writer-lock wait, from 1 through 60000 ms |

`DATABASE_URL` conflicts with host, port, name, user and password settings. PostgreSQL requires either a URL or host, database name, user and password. PostgreSQL-only settings are rejected with SQLite. Errors redact driver diagnostics and connection credentials. Keep connection URLs and passwords out of shell history and logs.

SQLite accepts `sqlite:///absolute/path/slopdrive.sqlite` or `sqlite:relative/path.sqlite`, without query parameters or fragments. In-memory databases are rejected. The default file is created with mode 0600 on Unix. Restrict directory access too. SQLite uses WAL, full synchronous commits and foreign keys; use a local filesystem, not a shared network mount. Multiple processes can share the database, including key management while the service runs.

PostgreSQL URLs must start with `postgres://` or `postgresql://`. `DATABASE_TLS_MODE` overrides URL TLS settings, so disabling TLS requires an explicit environment setting. Fixed table names live inside `DATABASE_SCHEMA`; table names are not configurable. The application creates its schema and applies embedded migrations at startup. Its role needs schema creation permission on first startup and ownership of its tables for later migrations. Use a dedicated database role and schema.

## Containers

[The default Compose file](../containers/compose.yaml) uses SQLite. [The PostgreSQL example](../containers/compose.postgres.yaml) runs a private PostgreSQL service without a published database port. Set `SLOPDRIVE_VERSION`, `BASE_URL` and `DATABASE_PASSWORD`, then use that file instead of the default one:

```sh
docker compose -f containers/compose.postgres.yaml up -d
docker compose -f containers/compose.postgres.yaml run --rm --no-deps slopdrive keys create
```

The PostgreSQL example disables database TLS on the local container network. For a remote database, configure verified TLS and mount a CA file if required. CLI commands must receive the same settings as the running service.

## Schema and backups

The shared [migration definitions](../src/database/migrations/v001.rs) describe one complete version-1 schema. SQLite and PostgreSQL render the same typed tables, constraints and relationships with their own SQL syntax. See [migration conventions](migrations.md) when changing the schema. Initialization holds a database lock and applies pending definitions with their ledger entries in one transaction. Concurrent startup is serialized. A changed checksum, missing ledger entry or schema newer than the binary is rejected.

This baseline replaces the earlier provider-specific histories. Stop SlopDrive and CLI writers, preserve any backups you need, then manually recreate the SQLite database or the dedicated PostgreSQL schema before starting this build. Recreating the database loses upload IDs, version history, API keys and S3 object references. Create new keys afterward. SlopDrive never resets a database or deletes S3 objects automatically. Existing objects remain private and unreferenced; they cannot reconstruct the deleted metadata.

For SQLite, stop writers and back up the entire data directory, including any WAL files. For online backups use SQLite's backup API or its `.backup` command; copying only the main file while writers run can lose committed data. For PostgreSQL use `pg_dump` and test restoring into a fresh database. Backups contain publisher key hashes and private metadata, so restrict access. Rollbacks require a schema-compatible binary or a tested backup restore.

History remains unlimited. Operators manage disk quotas, free space and backups for the database. Version lists grow with history; public reads select one version rather than loading every historical body. Never delete recorded versions automatically to make a write fit.
