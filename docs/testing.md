# Testing conventions

Run the checks required by [AGENTS.md](../AGENTS.md) before committing code. A bug fix needs a check that fails on the original behavior. Exercise observable results and preservation of data, not only status codes or counts.

Run `node tests/homepage.cjs` for homepage clipboard behavior. This dependency-free check executes the page's script with successful, denied, and unavailable clipboard access. Also check the page in a browser for layout, keyboard access, and actual clipboard permissions.

Run `python3 tests/releases.py` for version validation, immutable image promotion, and channel ordering. Run `python3 tests/compose.py docker compose` for image selection and deployment settings. Container CI tests both exported image variants with Docker and Podman, including switching a populated data volume between variants.

Run `python3 tests/performance.py` and `python3 tests/benchmark.py` for measurement and budget validation. See [performance budgets](performance.md) for the local container command and calibration requirements.

Use real routers and temporary stores for transport tests. Test successful requests, malformed inputs, exact size boundaries, missing data, retries, historical versions, and failure redaction. Paired HTTP and MCP checks must exercise the same shared behavior. Inspect complete version sets and retrieve their bytes when testing concurrency.

Keep private-state unit checks attached to their owning module with `cfg(test)` and a path into the test directory. Integration executables are explicit Cargo test targets; add a target when adding one. Disabling automatic integration discovery prevents private unit modules from being compiled a second time as standalone crates. Keep fixture construction in test-only helpers rather than exposing production internals.

Own every temporary directory, listener, and child process. Avoid filesystem races, existing live services, and shared mutable fixtures. Use bounded polling for process readiness. Waits that exercise deadlines must remain bounded and check the outcome. Stop owned processes and remove scratch data when a check finishes. Use deterministic filesystem faults where possible; do not add production abstractions merely to force entropy or disk-sync failures.

Capture generated results outside the documentation:

```sh
mkdir -p test-output
cargo test > test-output/tests.log 2>&1
cargo llvm-cov --json --output-path test-output/coverage.json > test-output/coverage.log 2>&1
cargo llvm-cov report --text --output-path test-output/coverage.txt
```

Coverage requires the optional `cargo-llvm-cov` tool and Rust's LLVM tools component. Review missing regions as well as lines. Coverage is evidence of execution, not proof that assertions protect behavior. Record unreachable or environment-dependent gaps in the generated report rather than hiding them or encoding current percentages in conventions.

## Database contracts

`cargo test` runs the shared storage, authentication, HTTP and MCP contracts against real SQLite. PostgreSQL integration tests require explicit user approval before running. Do not start a PostgreSQL service or connect to one for tests without that opt-in. A configured connection URL alone is not approval. After approval, run the same contracts with a unique schema per fixture:

```sh
SLOPBOX_TEST_PROVIDER=postgres DATABASE_TEST_POSTGRES_URL=postgres://user:password@127.0.0.1:5432/test_database cargo test --locked
```

Use a disposable database with schema creation permission. Tests disable TLS for this local service. CLI key tests exercise PostgreSQL only when `SLOPBOX_TEST_PROVIDER=postgres` is explicitly set. Supply a disposable database externally. Ordinary local and CI tests do not start an external database service. Fixture schemas remain until the disposable database is removed.

Database changes must cover rollback, retries, restore, restart persistence, concurrent writers, revocation, migration compatibility and redacted errors on both providers. Keep image and performance budgets unchanged unless the owner approves a measured adjustment.
