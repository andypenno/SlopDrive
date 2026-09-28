---
name: slopbox
description: |-
  Use this skill whenever you are generating an HTML file for the user to view in a browser, such as a dashboard, visualisation, simple page, or report.
  Keywords: HTML, browser, page, dashboard, visualisation, report
---

# Publish HTML with SlopBox

Create one self-contained HTML file. Use the configured SlopBox MCP connection. For a new page, call `mcp__html_host__upload_html` with the full HTML string as `content`. For a known page, call `mcp__html_host__update_html` with its `upload_id` and full `content`. Do not ask for extra confirmation before uploading.

Give the user the returned `url`, which always points to the latest version. Only give `versioned_url` when the user explicitly requests a link to a specific version.

Call `mcp__html_host__list_versions` only when you already know the `upload_id` and need its history. If you mention history, keep the base `url` as the primary link and show version-specific links only on request. Never try to discover or list all uploads.

If MCP is unavailable and an API key is configured, send the HTML as the `file` field with content type `text/html` to `POST /upload` or `POST /upload/{upload_id}` using `Authorization: Bearer <API_KEY>`.
