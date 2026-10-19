#!/usr/bin/env bash
set -euo pipefail
# Usage: bash deploy/backup.sh /absolute/path/to/backups
# Run from repository root with production Compose project active.
backup_dir="${1:?Provide backup directory}"
mkdir -p "$backup_dir"
umask 077
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
outfile="$backup_dir/lark-docs-registry-$stamp.tar.gz"
# Archive named data volume as root inside disposable container.
docker compose -f compose.prod.yaml --env-file .env.production run --rm --no-deps \
  --entrypoint sh --user 0 -v "$backup_dir:/backup" lark-docs \
  -c "tar -czf /backup/$(basename "$outfile") -C /data ." 
test -s "$outfile"
echo "Backup created: $outfile"
echo "Backups contain gateway registry/metadata, not Feishu document content."
