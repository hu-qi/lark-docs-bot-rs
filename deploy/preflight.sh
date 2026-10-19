#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
env_file="${1:-.env.production}"
if [ ! -f "$env_file" ]; then echo "Missing $env_file; copy deploy/env.production.example" >&2; exit 2; fi
set -a
. "./$env_file"
set +a
for name in SITE_DOMAIN ACME_EMAIL LARK_APP_ID LARK_APP_SECRET ADMIN_API_KEY WRITE_API_KEY; do
  if [ -z "${!name:-}" ]; then echo "Missing: $name" >&2; exit 2; fi
done
if [ "${#ADMIN_API_KEY}" -lt 32 ] || [ "${#WRITE_API_KEY}" -lt 32 ] || [ "$ADMIN_API_KEY" = "$WRITE_API_KEY" ]; then
  echo "ADMIN_API_KEY and WRITE_API_KEY must be distinct, 32+ characters" >&2; exit 2
fi
if [[ "$SITE_DOMAIN" == *://* || "$SITE_DOMAIN" == */* || "$SITE_DOMAIN" == *" "* ]]; then
  echo "SITE_DOMAIN must be a bare DNS name" >&2; exit 2
fi
command -v docker >/dev/null || { echo "Docker not found" >&2; exit 2; }
docker compose version >/dev/null || { echo "Docker Compose v2 not found" >&2; exit 2; }
docker compose -f compose.prod.yaml --env-file "$env_file" config --quiet
echo "Configuration validated. Verify DNS, inbound 80/443 and host backups before deploying."
