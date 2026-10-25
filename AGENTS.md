# SlopDrive

- Keep the service as one Rust binary. HTTP, MCP, and CLI operations must use the same storage and key store.
- Keep the HTTP API and MCP surfaces in sync. Share validation, storage operations, URL construction, and error policy; add paired transport tests when shared behavior changes.
- Preserve random upload IDs, hashed API keys, authenticated writes and version lists, and public reads by known ID. Never add a global upload listing.
- Return both base and versioned URLs from upload and version-list operations. Agent-facing instructions should present the base URL by default.
- Keep the image compatible with Docker and Podman. The service only needs a mounted data directory, a public base URL, and a reverse proxy for TLS.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` for code changes. Check affected HTTP and MCP behavior against a running service when routes change.
- Keep test bodies under `tests/`, grouped by what they test. Put shared production helpers in `src/helpers.rs`; split them into `src/helpers/` when one file no longer fits.
- Start with [docs/README.md](docs/README.md) to discover contributor conventions, then read the linked documents relevant to the task. Keep durable conventions in `docs/` and generated coverage, test logs, and review reports in gitignored `test-output/`.
