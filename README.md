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
