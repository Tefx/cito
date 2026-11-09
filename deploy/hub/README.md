# Native Cito Hub Deployment (`cito hub`)

`cito hub` is a standalone, zero-Node.js HTTP/SSE server backed by SQLite (`${dataDir}/hub.sqlite` in WAL mode). It provides the same `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)` registry, durable signal stream, multi-channel read synchronization, and Human Channel subprocess hosting as the local daemon.

## 1. Standalone CLI

```sh
# Trusted local or Tailscale network (loopback by default)
cito hub --port 4291 --auth none --data-dir /var/lib/cito-hub

# Shared bearer token; use HTTPS or a private network for remote access
cito hub --bind 0.0.0.0 --auth token --token '<strong-secret-token>' --data-dir /var/lib/cito-hub

# Multi-user OIDC; terminate public HTTPS at a reverse proxy
CITO_OIDC_ADMIN_USERS=alice \
  cito hub --bind 0.0.0.0 --auth oidc --issuer https://id.example.com --client-id cito --data-dir /var/lib/cito-hub
```

Flags also accept `--key=value`. Their environment equivalents are `CITO_HUB_PORT`, `CITO_HUB_BIND`, `CITO_AUTH_MODE`, `CITO_HUB_TOKEN`, `CITO_OIDC_ISSUER`, `CITO_OIDC_CLIENT_ID`, and `CITO_HUB_DATA_DIR`. Blank environment variables count as unset; explicitly empty CLI values are rejected. The default data directory is `~/.cito/hub`. `SIGTERM` and `SIGINT` gracefully stop the HTTP listener, close SSE connections, and terminate managed channel subprocesses.

### HTTP & Security Contract
- **`GET /health`**: Unauthenticated health probe returning `{ "ok": true, "pid": <u32>, "version": "0.20.0", "auth_mode": "none" | "token" | "oidc" }`.
- **`POST /v1/rpc`**: JSON RPC endpoint accepting `{ "op": "...", ...params }` (or `{ "op": "...", "params": {...} }`).
- **`GET /v1/events`**: Server-Sent Events stream delivering real-time `kv`, `signal`, and `nc` (`node_ack`) frames.
- **OIDC Verification & Row-Level Security (`RLS`)**:
  - Verifies JWT signatures (`RS256`, `RS384`, `RS512`, `PS256`, `ES256`, `ES384`, `EdDSA`), `iss`, `aud`, `sub`, `exp`, and `nbf` against the issuer's `.well-known/openid-configuration` and `jwks_uri` endpoints over HTTPS.
  - Enforces per-principal Row-Level Security on private (`self` scope) registrations, signal retrieval (`stream_get_message`), and per-node SSE signal/acknowledgment streams.

## 2. Docker Compose

The multi-stage `deploy/hub/Dockerfile` builds `cito` with `rust:1-bookworm` and runs on `debian:bookworm-slim` as an unprivileged user with system SQLite, CA certificates, and the bundled Telegram channel plugin.

```sh
# 1. No authentication (published port restricted to host loopback)
docker compose -f deploy/hub/docker-compose.yml up -d --build

# 2. Pre-shared token authentication
CITO_AUTH_MODE=token CITO_HUB_TOKEN='<strong-secret-token>' \
  docker compose -f deploy/hub/docker-compose.yml up -d --build

# 3. Team OIDC authentication
CITO_AUTH_MODE=oidc CITO_OIDC_ISSUER=https://id.example.com CITO_OIDC_CLIENT_ID=cito \
  CITO_OIDC_ADMIN_USERS=alice \
  docker compose -f deploy/hub/docker-compose.yml up -d --build
```

The named volume persists `/data/hub.sqlite`. Configure a TLS reverse proxy (such as Caddy or Nginx) or a private mesh network (such as Tailscale) before exposing the port to remote clients.

## 3. Shared Human Channels (Telegram)

Hub-scoped channel plugins run as persistent subprocesses over the `human-channel` NDJSON protocol. Install additional `plugin.json` directories under `${dataDir}/plugins/<id>/` (or set `CITO_PLUGIN_DIR`).

1. **Server Bot Token**: Set `CITO_PLUGIN_TELEGRAM_BOT_TOKEN='<bot-token>'` on the Hub server (forwarded automatically by `deploy/hub/docker-compose.yml`), or configure it from `Settings…` (`⌘,`) → `Human Channels` as a user listed in `CITO_OIDC_ADMIN_USERS`. Channel list responses never expose raw Hub secrets.
2. **User Pairing**: Each user selects the target Hub in `Settings…` (`⌘,`) → `Human Channels`, clicks **`Pair Account…`**, and sends the displayed 10-character Base32 `/start cito-a1b2c3d4e5` command to the bot in a private Telegram chat.
3. **Bidirectional Delivery & Replies**: Outgoing notifications are reserved before delivery, chunked automatically if longer than 4,096 characters, and mapped so users can swipe-to-reply (or send a plain reply within 10 minutes) to route messages back to the originating agent session while synchronizing read state across all attached devices.
