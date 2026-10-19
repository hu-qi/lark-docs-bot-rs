# Lark Docs Bot RS

Rust/Axum Web editor + REST API + MCP gateway for Feishu Docx.

## Features

- Web UI: create, list, preview, append paragraphs.
- REST API: create/list/read/append, block list, access control.
- MCP JSON-RPC over HTTP POST: initialize, ping, tools/list, tools/call.
- Document allowlist with persistent JSON registry; documents private by default.
- Separate administrator and writer Bearer keys.
- Docker and GitHub Actions CI.

## Deploy

1. Copy .env.example to .env.
2. Set LARK_APP_ID and LARK_APP_SECRET to your deployed Feishu app credentials.
3. Create different, random 32+ character ADMIN_API_KEY and WRITE_API_KEY.
4. Set LARK_DOC_BASE to your Feishu tenant domain.
5. Run: docker compose up -d --build
6. Configure an HTTPS reverse proxy and rate limiting in front of localhost:3000.

GitHub Actions secrets APPID / APPSECRET are not automatically passed to your host. Map them to LARK_APP_ID / LARK_APP_SECRET at deployment time. CI itself needs no secrets.

## REST routes

| Method | Endpoint | Access |
| --- | --- | --- |
| GET | /health | anyone |
| GET | / | browser UI |
| GET | /api/docs | writer/admin |
| POST | /api/docs | admin |
| GET | /api/docs/{id} | writer/admin or explicitly public-read |
| GET | /api/docs/{id}/blocks | writer/admin or explicitly public-read |
| POST | /api/docs/{id}/append | writer/admin |
| PATCH | /api/docs/{id}/blocks/{block_id} | writer/admin |
| POST | /api/docs/{id}/access | admin |
| POST | /mcp | tool-dependent |

All privileged requests require header: Authorization: Bearer YOUR_KEY

Create: POST /api/docs JSON {"title":"Sample"}
Replace a plain-text block: PATCH /api/docs/{id}/blocks/{block_id} JSON {"text":"Revised paragraph"}
Append: POST /api/docs/{id}/append JSON {"text":"Line 1\nLine 2"}
Gateway public-read: POST /api/docs/{id}/access JSON {"public_read":true}

## MCP

Point an HTTP JSON-RPC MCP client at https://your-host/mcp and pass a Bearer token.
Supported tools: list_documents, create_document, read_document, list_blocks, append_text, update_text_block.
Creating documents requires administrator key. Protocol version 2025-03-26.

## Limitations

The Web UI appends text; the REST API also replaces existing plain-text blocks. It is NOT yet a WYSIWYG/full-replacement editor. A block containing mentions, embeds or other non-text elements cannot be replaced via this simplified API.
Gateway public-read does NOT modify native Feishu sharing permissions.
MCP supports JSON-RPC POST; full Streamable HTTP transport extras such as SSE are not implemented.
Gateway document metadata are stored in a JSON file; document bodies stay in Feishu.
Feishu permissions and actual API functionality must be validated in your tenant.
Rotate any application secret previously exposed in chat, logs, or screenshots.

See .env.example, Dockerfile, compose.yaml, and .github/workflows/ci.yml.

## Live Feishu integration smoke test

After normal CI passes, open [Feishu API Live Smoke Test](https://github.com/hu-qi/lark-docs-bot-rs/actions/workflows/feishu-smoke.yml), select **Run workflow** on main, and run it manually. This action uses the repository's APPID / APPSECRET secrets, obtains a tenant access token, creates a labelled test document, appends a text block, verifies raw-content readback, updates that block, and verifies the updated text. It intentionally does not run automatically on every push and does not delete test documents. The workflow prints the created document identifier and URL, **not** application credentials or tenant tokens. Only run in a test tenant or if creating one test document in the production tenant is acceptable.

If the live smoke fails with an authorization error, inspect Feishu app scopes, publication/installation status, and access permissions. Errors such as Feishu 91403 often indicate application document permission is missing. The live workflow checks create/read/append/update and leaves its test document in Feishu for inspection.


## Web editing (experimental)

The editor reads the first page of blocks (up to 100), displays editable plain-text blocks and updates them via PATCH. Rich content, nested structures and additional pagination are not yet supported by the browser UI. All block edits require the writer/admin key and registered-document access. Because replacing a text block can alter formatting, avoid editing styled or mixed content until richer element-aware updates are implemented.


### MCP edit example

Use `tools/call` with `name: "update_text_block"` and arguments `{"id":"DOCUMENT_ID","block_id":"TEXT_BLOCK_ID","text":"New paragraph"}`. The tool requires a writer/admin key and applies the same plain-text-only validation as the REST PATCH route. Obtain `block_id` from `list_blocks`. The current MCP endpoint supports JSON-RPC POST only (not full Streamable HTTP sessions).

**Deletion is not enabled yet:** Feishu deletes blocks by parent and child index range rather than by standalone block ID. Safe deletion must verify the current parent order and document revision to avoid deleting the wrong block when collaborators edit concurrently. See [Feishu Docx guide](https://open.larkoffice.com/document/server-docs/docs/docs/docx-v1/guide).


## Revision-pinned block deletion (experimental)

Deletion is deliberately not available in the browser UI. This API requires an existing registered document, writer/admin credentials, the **exact child block ID and index**, its parent block ID, and an explicit nonnegative `document_revision_id`. Inspect the document via `GET /api/docs/{id}/metadata` (document revision) and `GET /api/docs/{id}/blocks/{parent_id}/children` (child IDs and order). Then call `POST /api/docs/{id}/blocks/delete` with JSON:

```json
{"parent_id":"PARENT_BLOCK_ID","block_id":"CHILD_BLOCK_ID","index":0,"document_revision_id":123}
```

The gateway queries the same revision, rejects mismatched child IDs and incomplete child pages, and sends the supplied revision to Feishu's `batch_delete` API. Never use `-1` (latest) for deletion. An external concurrent edit may cause Feishu to reject the supplied document revision; do not retry automatically with the latest revision. MCP exposes the same guarded `delete_block` tool. **Deleting is destructive; verify on a disposable document before production use.**


### Optional live deletion verification (manual only)

The **Feishu API Live Smoke Test** workflow now has a `test_delete` input, **off by default**. When explicitly enabled, the smoke binary creates a NEW document, appends/updates its test block, reads that document's revision and child order, then deletes only the block it created and verifies readback. It never accepts existing document IDs from workflow inputs. The test document itself is preserved for inspection. Run this only in a tenant where writing a disposable test document is permitted; this is a destructive integration test of the test block, not a production document cleanup operation.


### MCP stateless HTTP security and compatibility

The `/mcp` POST endpoint now requires a **writer/admin Bearer key even for initialize and tools/list**, validates JSON-RPC 2.0 request IDs and methods, rejects cross-origin browser requests, and marks JSON responses `Cache-Control: no-store`. It returns `MCP-Protocol-Version: 2025-03-26` and HTTP 202 for accepted notifications. Existing write and document allowlist authorization continues to apply to individual tools.

This is a **stateless JSON-only HTTP implementation**. It does **not** yet implement resumable SSE, session management, batch JSON-RPC, or all MCP transport negotiation behaviors. Clients must support a JSON-only, stateless HTTP MCP endpoint; some MCP clients may require additional protocol support. Configure your HTTPS reverse proxy to preserve `Host` and reject unexpected origins; do not expose plain HTTP remotely.


### Request limits and mutation audit (single instance)

The Rust HTTP gateway applies configurable per-minute counters **per API-key role (admin, writer, anonymous)** and request class (read vs non-GET). Defaults: `READ_LIMIT_PER_MINUTE=120`, `WRITE_LIMIT_PER_MINUTE=30`. HTTP 429 is returned when exceeded. This is a coarse **in-memory, per-process** limiter, not per-IP throttling, tenant quotas, or a distributed production abuse-prevention system. Put an independently configured IP-based rate limiter/WAF at the HTTPS reverse proxy, especially for public endpoints. Each mutation records its role, request path and response status through tracing; no document bodies, tokens or keys are intentionally recorded. Logs are not a durable audit ledger and must be shipped to a secure log system for retention. Do not use multiple replicas without centralized state and coordination.


### Structured block creation (Web, REST, MCP)

The Web editor now has a **structured content queue** for paragraphs, headings (H1-H3), unordered and ordered list items, and quotes. Add blocks to the queue and confirm the document before submitting. The server validates a maximum of 50 blocks per request and 2,000 characters per block; each block may optionally use bold and italic styles. Existing blocks are not overwritten. API: `POST /api/docs/{id}/append-structured` (writer/admin Bearer token) with body:

```json
{"blocks":[{"kind":"heading2","text":"Roadmap"},{"kind":"bullet","text":"Ship the editor","bold":true}]}
```

MCP exposes `append_structured` with the same `id` and `blocks` fields. This stage supports creating structured blocks, **not lossless rich-text editing of existing blocks**; images, tables and advanced embedded content remain unsupported. Integration with the live Feishu API should be verified before relying on new block types in production.


### Optional real structured-content verification

The manual [Feishu API Live Smoke Test](https://github.com/hu-qi/lark-docs-bot-rs/actions/workflows/feishu-smoke.yml) has a `test_structured` checkbox (off by default). When enabled it creates a disposable document, inserts heading, unordered-list, ordered-list and quote blocks, and confirms their text can be read back. It does not touch existing documents. The Rust CI validates serialization and HTTP protocol behavior, but does not replace this live Feishu round-trip.


### Editing existing structured blocks (experimental)

The Web editor and REST `PATCH /api/docs/{id}/blocks/{block_id}` now support single-text-run paragraphs, H1-H3 headings, ordered/unordered list entries and quote blocks. Requests may include `expected_text` alongside `text`; if the block's current content no longer matches, the gateway returns HTTP 409 rather than overwriting unexpectedly. MCP `update_text_block` accepts the same optional `expected_text`. The gateway preserves the existing text-run element attributes when replacing its content. Multi-run text, embeds, and other complex document blocks are intentionally not supported. This is a best-effort read-before-write check, not an atomic Feishu compare-and-swap; concurrent writers may still race between the read and update. Use Feishu's native editor for highly concurrent or complex rich-text changes.

The optional live smoke test's `test_structured` mode also verifies updating a freshly created heading and re-reading it. No existing document is modified by the test.


### Long-document block pagination

`GET /api/docs/{id}/blocks` supports optional `page_token` forwarding for Feishu's cursor-based pagination (100 blocks per page). The gateway URL-encodes opaque tokens and rejects malformed control characters. The Web editor now follows up to **10 pages / 1,000 blocks**, avoids repeated cursors, and indicates when a document exceeds that limit. This page limit is deliberately bounded to reduce unexpected Feishu API calls. Existing REST/MCP callers can continue consuming pages explicitly; nested child blocks are not recursively expanded in the browser.


### Web editor visual preview and quick toolbar

The browser editor now offers a **semantic, read-only rich preview** of supported Feishu text blocks (paragraphs, H1–H3, bullet/numbered lists and blockquotes) alongside the existing raw-text view. Draft content has a live preview and shortcut buttons to choose heading/list/quote types and toggle bold/italic for **new blocks**. Preview text is inserted through DOM `textContent` rather than parsed as HTML, so embedded markup is not executed. Unsupported embedded elements are shown as placeholders, not silently rendered as plain text. Loading a different document invalidates stale refresh responses. Preview is approximate rather than pixel-identical with Feishu, and style fidelity is intentionally limited for mixed-run blocks. CI runs `scripts/web-editor-test.cjs` to check syntax, safe DOM rendering and list grouping.
