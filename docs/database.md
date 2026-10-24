# Database configuration

SQLite is the default. One database stores version history, latest selections and API key hashes. HTML bytes also live there unless [S3 content storage](content-storage.md) is selected. HTTP, MCP and CLI commands use the same backend and settings. Both providers implement the same contract. Existing filesystem installations are not imported; rebuild alpha installations from scratch.

| Setting | Default | Meaning |
| --- | --- | --- |
| `DATABASE_PROVIDER` | `sqlite` | `sqlite` or `postgres` |
| `DATABASE_URL` | unset | SQLite file URL or PostgreSQL connection URL |
| `STORAGE_PATH` | `/data` | Directory for the default `slopbox.sqlite` file |
| `DATABASE_HOST` | unset | PostgreSQL host when no URL is supplied |
| `DATABASE_PORT` | `5432` | PostgreSQL port |
| `DATABASE_NAME` | unset | PostgreSQL database name |
| `DATABASE_USER` | unset | PostgreSQL user |
| `DATABASE_PASSWORD` | unset | PostgreSQL password, environment only |
| `DATABASE_SCHEMA` | `slopbox` | PostgreSQL schema, a lowercase SQL identifier, at most 63 bytes |
| `DATABASE_TLS_MODE` | `verify-full` | PostgreSQL TLS hostname and certificate verification, or explicit `disable` |
| `DATABASE_TLS_CA_FILE` | built-in CA roots | Optional PostgreSQL CA certificate file |
| `DATABASE_MAX_CONNECTIONS` | `4` | Pool size, from 1 through 32 |
| `DATABASE_CONNECT_TIMEOUT_MS` | `5000` | Connection, initialization and pool acquisition timeout, from 1 through 60000 ms |
| `DATABASE_QUERY_TIMEOUT_MS` | `5000` | PostgreSQL statement/lock timeout; SQLite writer-lock wait, from 1 through 60000 ms |

`DATABASE_URL` conflicts with host, port, name, user and password settings. PostgreSQL requires either a URL or host, database name, user and password. PostgreSQL-only settings are rejected with SQLite. Errors redact driver diagnostics and connection credentials. Keep connection URLs and passwords out of shell history and logs.

SQLite accepts `sqlite:///absolute/path/slopbox.sqlite` or `sqlite:relative/path.sqlite`, without query parameters or fragments. In-memory databases are rejected. The default file is created with mode 0600 on Unix. Restrict directory access too. SQLite uses WAL, full synchronous commits and foreign keys; use a local filesystem, not a shared network mount. Multiple processes can share the database, including key management while the service runs.

PostgreSQL URLs must start with `postgres://` or `postgresql://`. `DATABASE_TLS_MODE` overrides URL TLS settings, so disabling TLS requires an explicit environment setting. Fixed table names live inside `DATABASE_SCHEMA`; table names are not configurable. The application creates its schema and applies embedded migrations at startup. Its role needs schema creation permission on first startup and ownership of its tables for later migrations. Use a dedicated database role and schema.

## Containers

[The default Compose file](../containers/compose.yaml) uses SQLite. [The PostgreSQL example](../containers/compose.postgres.yaml) runs a private PostgreSQL service without a published database port. Set `SLOPBOX_VERSION`, `BASE_URL` and `DATABASE_PASSWORD`, then use that file instead of the default one:

```sh
docker compose -f containers/compose.postgres.yaml up -d
docker compose -f containers/compose.postgres.yaml run --rm --no-deps slopbox keys create
```

The PostgreSQL example disables database TLS on the local container network. For a remote database, configure verified TLS and mount a CA file if required. CLI commands must receive the same settings as the running service.

## Schema and backups

Migrations are embedded from `migrations/sqlite/` and `migrations/postgres/`. Initialization holds a database lock and records migration checksums in `schema_migrations`. Concurrent startup is serialized. A changed checksum or a schema newer than the binary is rejected. Add paired migrations for schema changes and test both providers. Never edit an applied migration or manually change the ledger to bypass compatibility checks.

For SQLite, stop writers and back up the entire data directory, including any WAL files. For online backups use SQLite's backup API or its `.backup` command; copying only the main file while writers run can lose committed data. For PostgreSQL use `pg_dump` and test restoring into a fresh database. Backups contain publisher key hashes and private metadata, so restrict access. Rollbacks require a schema-compatible binary or a tested backup restore.

History remains unlimited. Operators manage disk quotas, free space and backups for the database. Version lists grow with history; public reads select one version rather than loading every historical body. Never delete recorded versions automatically to make a write fit.
