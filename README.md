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
Supported tools: list_documents, create_document, read_document, append_text.
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

After normal CI passes, open [Feishu API Live Smoke Test](https://github.com/hu-qi/lark-docs-bot-rs/actions/workflows/feishu-smoke.yml), select **Run workflow** on main, and run it manually. This action uses the repository's APPID / APPSECRET secrets, obtains a tenant access token, creates a labelled test document, appends a text block, and verifies raw-content readback. It intentionally does not run automatically on every push and does not delete test documents. The workflow prints the created document identifier and URL, **not** application credentials or tenant tokens. Only run in a test tenant or if creating one test document in the production tenant is acceptable.

If the live smoke fails with an authorization error, inspect Feishu app scopes, publication/installation status, and access permissions. Errors such as Feishu 91403 often indicate application document permission is missing. This workflow checks create/read/append only; block modification still requires a separate live test.
