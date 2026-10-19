#!/usr/bin/env bash
set -euo pipefail
# Usage: bash deploy/backup.sh /absolute/path/to/backups
# This backs up existing registry data before a release. On first deployment,
# there is no named volume to back up and the step exits successfully.
backup_dir="${1:?Provide backup directory}"
mkdir -p "$backup_dir"
backup_dir="$(cd "$backup_dir" && pwd)"
umask 077
volume="$(docker compose -f compose.prod.yaml --env-file .env.production config --format json | python3 -c 'import json,sys; print(json.load(sys.stdin)["volumes"]["lark-docs-data"]["name"])')"
if ! docker volume inspect "$volume" >/dev/null 2>&1; then
  echo "Initial deployment: no existing registry volume to back up."
  exit 0
fi
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
name="lark-docs-registry-$stamp.tar.gz"
# Use a tiny, pinned archiver rather than compiling the Rust service to run a backup.
docker run --rm --read-only \
  --security-opt no-new-privileges --cap-drop ALL \
  -v "$volume:/data:ro" -v "$backup_dir:/backup" \
  alpine:3.20 sh -c 'tar -czf "/backup/$1" -C /data .' sh "$name"
test -s "$backup_dir/$name"
echo "Backup created: $backup_dir/$name"
echo "This contains gateway registry metadata, not Feishu document bodies."
