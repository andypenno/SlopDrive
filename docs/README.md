# Contributor conventions

- [Testing](testing.md) covers verification and test design.
- [HTTP API](api.md) covers request handling and response policy.
- [MCP](mcp.md) covers protocol and tool contracts.
- [Migration conventions](migrations.md) covers shared schema definitions, provider rendering and ledger checksums.
- [Database configuration](database.md) covers providers, deployment, migrations and backups.
- [Deployment modes](deployment-modes.md) defines the supported database and content storage combinations, shared guarantees and S3 target contract.
- [HTML content storage](content-storage.md) covers S3 configuration, Garage, existing data and backup requirements.
- [Storage and keys](storage.md) covers persistence and authentication.
- [Release strategy](releases.md) covers branches, image variants, version promotion, and public release settings.
- [Performance budgets](performance.md) covers container measurements, local checks, calibrated limits, and owner control.

The root [AGENTS.md](../AGENTS.md) defines repository-wide rules. The root [README.md](../README.md) documents setup and client use.

## Repository layout

| Directory | Contents |
| --- | --- |
| `src/` | The Rust service, CLI and shared application code. Database migrations live in `src/database/migrations/`. |
| `tests/` | Rust test bodies, transport checks and executable verification tools. Shared Rust fixtures live in `tests/support/`. |
| `tests/performance/` | Benchmark runner, budget evaluator, policy and their self-checks. |
| `containers/` | Containerfiles, their build ignore files and Compose deployment definitions. |
| `release/` | Image publication and version promotion. These operations change registry state. |
| `static/` | Assets embedded in the service, including the homepage. |
| `skills/` | The SlopDrive client skill distributed to publishing agents. |
| `.github/` | CI workflows, dependency updates and ownership rules. |
| `docs/` | Contributor conventions, operator references and deployment contracts. |
| `test-output/` | Gitignored coverage, logs, measurements and review reports. |

Place files by responsibility. Check the existing owning directory before creating one, and group related test runners, helpers, policy and self-checks together. Keep `containers/` for build and deployment definitions; do not create catch-all `scripts/` or `utils/` directories. Update imports, CI commands, ownership rules and documentation when moving files.
