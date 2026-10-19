# Production operations · Lark Docs Bot RS

## Target architecture

Use a **single persistent Linux host** (amd64 or arm64) with Docker Engine and Compose v2, a DNS A/AAAA record and open TCP 80/443 (optionally UDP 443). The gateway runs behind Caddy which handles automatic TLS. Gateway port 3000 is **not** published externally. The document registry lives in a named Docker volume; full Feishu document content stays in Feishu. Do not deploy this stateful configuration to ephemeral serverless storage.

## First deployment

1. Clone this repository onto the target Linux host and install Docker Engine + Compose v2.
2. Set your DNS to this host and confirm both port 80 and 443 are accessible.
3. Copy `deploy/env.production.example` to project-root `.env.production`, replace **all** placeholders, and set `chmod 600 .env.production`. Generate unique API keys with `openssl rand -hex 32`. Do not put these values in Git or issue comments.
4. Run `bash deploy/preflight.sh` from the repo root. Confirm it succeeds. This does not perform actual network checks or issue TLS certificates.
5. Start the stack: `docker compose --env-file .env.production -f compose.prod.yaml up -d --build`.
6. Check `docker compose --env-file .env.production -f compose.prod.yaml ps`, and test `curl -fsS https://YOUR_DOMAIN/health`. Only proceed to administrative requests after TLS and host-based access controls are verified.
7. Configure independent proxy/WAF IP-based rate limiting, host firewall, monitoring and off-host log storage. Application role-based in-memory counters are insufficient on their own.

## Upgrades and rollback

Run `bash deploy/backup.sh /absolute/path/to/backups` before a release. To upgrade: fetch a reviewed Git commit, run preflight, then `docker compose --env-file .env.production -f compose.prod.yaml up -d --build`. To roll back, check out the prior tested commit and rebuild. Keep the gateway's data volume; do **not** run `docker compose down -v`. If registry format changes in future releases, take a snapshot before any binary upgrade.

## Data recovery

The backup archive contains the gateway's document registry metadata, **not** the documents themselves. Restore only during downtime on a disposable or deliberately isolated host first: `docker compose ... down`, locate the named `lark-docs-data` Docker volume, restore the contents of the archive into that volume using a temporary privileged container, verify permissions (UID 10001), then start the service. Keep at least daily encrypted off-host backups and test restoration. The included backup helper archives the current volume and is not an incremental or transactionally consistent snapshot; avoid metadata changes during its run.

## Secret rotation

`APPID`/`APPSECRET` in GitHub Actions are only used for opt-in Feishu integration smoke tests. Production `LARK_APP_ID` and `LARK_APP_SECRET` must be independently provided to the host. After rotation, update both stores if they use the same Feishu app. Rotate gateway ADMIN/WRITE API keys separately; clients must then update credentials. Never include credentials in logs, issue descriptions, crash reports, or support requests.

## Operational limitations and launch gates

- No automatic HTTPS deployment occurs simply because GitHub CI or GHCR publish succeeds. A running server, DNS and production secrets are required.
- This stack is intentionally **single replica**: registry JSON mutations and in-memory rate limits are not coordinated across replicas.
- Host metrics, external health probes, alerting, log retention, backups, authentication abuse prevention and firewall policy must be operational before exposing document writes publicly.
- Rich-text multi-run API is still experimental pending its dedicated live Feishu test; do not expose this functionality for business-critical documents until validated.
- The GHCR workflow publishes `linux/amd64` and `linux/arm64` images as build artifacts, but production Compose builds from reviewed source to avoid relying on package visibility or registry pull credentials.
- Vercel Hobby/serverless is not a direct drop-in host for this persistent long-running Axum application and named-volume registry.
