# Testing conventions

Run the checks required by [AGENTS.md](../AGENTS.md) before committing code. A bug fix needs a check that fails on the original behavior. Exercise observable results and preservation of data, not only status codes or counts.

Run `node tests/homepage.cjs` for homepage clipboard behavior. This dependency-free check executes the page's script with successful, denied, and unavailable clipboard access. Also check the page in a browser for layout, keyboard access, and actual clipboard permissions.

Use real routers and temporary stores for transport tests. Test successful requests, malformed inputs, exact size boundaries, missing data, retries, historical versions, and failure redaction. Paired HTTP and MCP checks must exercise the same shared behavior. Inspect complete version sets and retrieve their bytes when testing concurrency.

Keep private-state unit checks attached to their owning module with `cfg(test)` and a path into the test directory. Integration executables are explicit Cargo test targets; add a target when adding one. Disabling automatic integration discovery prevents private unit modules from being compiled a second time as standalone crates. Keep fixture construction in test-only helpers rather than exposing production internals.

Own every temporary directory, listener, and child process. Avoid filesystem races, sleeps, live services, and shared mutable fixtures. Stop owned processes and remove scratch data when a check finishes. Use deterministic filesystem faults where possible; do not add production abstractions merely to force entropy or disk-sync failures.

Capture generated results outside the documentation:

```sh
mkdir -p test-output
cargo test > test-output/tests.log 2>&1
cargo llvm-cov --json --output-path test-output/coverage.json > test-output/coverage.log 2>&1
cargo llvm-cov report --text --output-path test-output/coverage.txt
```

Coverage requires the optional `cargo-llvm-cov` tool and Rust's LLVM tools component. Review missing regions as well as lines. Coverage is evidence of execution, not proof that assertions protect behavior. Record unreachable or environment-dependent gaps in the generated report rather than hiding them or encoding current percentages in conventions.
