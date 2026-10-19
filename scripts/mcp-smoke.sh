#!/usr/bin/env bash
set -euo pipefail
export LARK_APP_ID=cli_test_only
export LARK_APP_SECRET=test_only
export ADMIN_API_KEY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
export WRITE_API_KEY=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
export BIND_ADDR=127.0.0.1:39387
export DATA_FILE="$(mktemp -d)/registry.json"
target/debug/lark-docs-bot-rs >/dev/null 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; rm -rf "$(dirname "$DATA_FILE")"' EXIT
for i in $(seq 1 40); do
  if curl -fsS http://127.0.0.1:39387/health >/dev/null 2>&1; then break; fi
  sleep 0.25
done
curl -fsS http://127.0.0.1:39387/health >/dev/null
base=http://127.0.0.1:39387/mcp
payload='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"ci","version":"1"}}}'
status=$(curl -sS -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' -d "$payload" "$base")
test "$status" = 401
response=$(curl -fsS -H "Authorization: Bearer $WRITE_API_KEY" -H 'Content-Type: application/json' -d "$payload" "$base")
python3 -c 'import sys,json; x=json.load(sys.stdin); assert x["result"]["protocolVersion"]=="2025-03-26"' <<< "$response"
response=$(curl -fsS -H "Authorization: Bearer $WRITE_API_KEY" -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' "$base")
python3 -c 'import sys,json; x=json.load(sys.stdin); assert "delete_block" in [t["name"] for t in x["result"]["tools"]]' <<< "$response"
status=$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $WRITE_API_KEY" -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' "$base")
test "$status" = 202
status=$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $WRITE_API_KEY" -H 'Content-Type: application/json' -d '{"jsonrpc":"1.0","id":1,"method":"ping"}' "$base")
test "$status" = 400
status=$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $WRITE_API_KEY" -H 'Origin: https://attacker.example' -H 'Content-Type: application/json' -d "$payload" "$base")
test "$status" = 403
echo 'Stateless MCP HTTP protocol checks passed'
