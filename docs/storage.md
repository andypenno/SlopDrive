# Storage and key conventions

Keep HTTP, MCP, and CLI on the shared stores required by [AGENTS.md](../AGENTS.md). Validate identifiers before filesystem access. Distinguish missing data from other I/O failures; unexpected errors must not become successful empty results.

One service owns an upload directory at a time through an OS file lock. A second service using that data path must fail at startup. Key creation and revocation remain available while the service holds the upload lock.

Write immutable content before publishing metadata that selects it. Replace metadata atomically and serialize mutations within the service. Pre-publication failures must preserve the previous latest version and history. A failed directory sync after publication is an uncertain commit; HTTP returns 503 and MCP reports that the write may have committed. Clients should retry the same content. Check persistence by reopening the store rather than only inspecting in-memory results.

Distinct content adds one version. Retrying the latest content preserves its metadata. Restoring historical content selects that version without duplicating it or changing its original timestamp and size.

Apply the key-storage policy in [AGENTS.md](../AGENTS.md). Print new plaintext keys only from the creation command. Read key records on authentication so revocation takes effect without a restart. Malformed or unreadable records must fail closed. Restrict stored file permissions and keep keys out of logs.

Metadata reads reject empty histories, invalid or duplicate content hashes, sizes outside 1 byte through 1 MiB, and a latest hash absent from history. Normal reads do not rehash every historical file. Operators should check content hashes and recorded sizes against files when checking storage integrity.

Known pre-publication failures remove newly reserved upload directories and content absent from recorded history. Uncertain metadata commits retain their files. Interrupted processes can still leave orphan files. Stop the service before an integrity check; preserve backups and reconcile files against validated metadata before removing unreferenced content. Never remove recorded history to reclaim space automatically.

SIGTERM and SIGINT stop new connections and allow up to 10 seconds for active requests and detached filesystem jobs to drain. Runtime shutdown allows one further second before abandoning remaining jobs. Configure the container stop grace period to at least 15 seconds. A drain timeout exits with an error; interrupted writes still require the orphan reconciliation above.
