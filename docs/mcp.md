# MCP conventions

Tool schemas are client contracts. Enforce required fields, types, and allowed properties at runtime. Optional fields must reject invalid types rather than silently becoming absent. Keep schema discovery and dispatch consistent.

Validate the JSON-RPC envelope before dispatch. Accept string, numeric, or null request IDs; reject structured and boolean IDs. Params must be an object or array when present. Notifications receive an empty acknowledgement and do not execute publishing tools.

Keep protocol failures separate from tool failures. Invalid JSON, invalid envelopes, unknown methods, and invalid tool arguments use JSON-RPC error responses. A valid tool call whose shared operation fails returns a tool result with `isError` set. Authentication failures remain HTTP failures before protocol parsing.

Expose the same publishing, versioning, and URL behavior as the HTTP API, as required by [AGENTS.md](../AGENTS.md). Use the shared error policy from [HTTP API conventions](api.md); never leak storage details in tool text.

For successful calls, text content and structured content must describe the same result. Apply the URL and enumeration policies in [AGENTS.md](../AGENTS.md).
