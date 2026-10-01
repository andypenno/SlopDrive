# Storage and key conventions

Keep HTTP, MCP, and CLI on the shared stores required by [AGENTS.md](../AGENTS.md). Validate identifiers before filesystem access. Distinguish missing data from other I/O failures; unexpected errors must not become successful empty results.

Write immutable content before publishing metadata that selects it. Replace metadata atomically and serialize mutations within the service. A failed update must preserve the previous latest version and its history. Check persistence by reopening the store rather than only inspecting in-memory results.

Distinct content adds one version. Retrying the latest content preserves its metadata. Restoring historical content selects that version without duplicating it or changing its original timestamp and size.

Apply the key-storage policy in [AGENTS.md](../AGENTS.md). Print new plaintext keys only from the creation command. Read key records on authentication so revocation takes effect without a restart. Malformed or unreadable records must fail closed. Restrict stored file permissions and keep keys out of logs.
