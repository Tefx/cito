# Host a Cito Hub

A shared Hub connects Agent Session Nodes and Human Nodes across machines. `cito hub` runs the same `HubEngine` as the embedded `local://` Hub: SQLite WAL storage, durable signals, leases, read synchronization, and managed channel subprocesses. No Node.js runtime or external message broker is required. The macOS menu bar app remains a client; the standalone Hub also runs on Linux.

For daily use, see the [User Guide](https://tefx.github.io/cito/docs/guide.html). For the browser version of this operator guide, see [Host a Hub](https://tefx.github.io/cito/docs/hosting.html).

## Standalone server

Install a native `cito` executable on the server. For a Linux source build, run `cargo build --release --bin cito-native` at the repository root and install `target/release/cito-native` as `/usr/local/bin/cito`. Bundled channel executables must also be built and available through plugin discovery; Docker provides a packaged alternative.

```sh
# Local trusted access only; default bind is loopback for auth=none.
cito hub --port 4291 --auth none --data-dir /var/lib/cito-hub

# Shared token; terminate HTTPS at a proxy for public access.
cito hub --bind 127.0.0.1 --auth token --token '<strong-secret-token>' --data-dir /var/lib/cito-hub

# Multi-user OIDC with an explicit Hub administrator.
CITO_OIDC_ADMIN_USERS=alice cito hub --bind 127.0.0.1 --auth oidc --issuer https://id.example.com --client-id cito --data-dir /var/lib/cito-hub
```

Create the data directory with ownership for the service account. Flags accept both `--key value` and `--key=value` and take precedence over the corresponding environment variable. Blank standalone Hub environment values count as unset; empty CLI values are rejected. `SIGTERM` and `SIGINT` close the listener, SSE streams, and managed channels gracefully. Use `cito --version` to inspect the installed release.

On a client Mac, join a token Hub with:

```sh
cito setup --hub work --url https://cito.example.com --auth token --token '<strong-secret-token>' --default
```

Use a distinct token for each independently operated Hub. Token access and Trusted (`none`) access do not provide OIDC's verified per-user identity. Keep unauthenticated Hubs on a trusted private network; never expose them publicly.

## Docker Compose

The repository's `deploy/hub/docker-compose.yml` builds a Linux native Hub with bundled channel plugins. It publishes only `127.0.0.1:4291` on the host, stores data in the `cito-hub-data` named volume at `/data`, and restarts unless explicitly stopped. Run from the repository root:

```sh
export CITO_AUTH_MODE=token
export CITO_HUB_TOKEN='<strong-secret-token>'
docker compose -f deploy/hub/docker-compose.yml up -d --build
docker compose -f deploy/hub/docker-compose.yml logs --tail=50 cito-hub
curl http://127.0.0.1:4291/health
```

The Compose file defaults to `none`; set authentication before exposing the service through a proxy. For OIDC, set `CITO_AUTH_MODE=oidc`, `CITO_OIDC_ISSUER`, `CITO_OIDC_CLIENT_ID`, and `CITO_OIDC_ADMIN_USERS` instead. It also forwards `CITO_PLUGIN_TELEGRAM_BOT_TOKEN`. Add other plugin-specific variables explicitly to the service's `environment` mapping; a host environment variable alone does not enter a container. Recreate the container after changing its environment. User plugin overrides belong at `/data/plugins/<id>/`; bundled defaults live at `/app/plugins` via `CITO_BUILTIN_PLUGINS_DIR`.

## Linux systemd

Create a dedicated `cito` service account and `/var/lib/cito-hub` owned by that account. Install the executable and plugins before starting the unit. Save credentials in `/etc/cito-hub.env`, owned by root with mode `0600`:

```ini
CITO_AUTH_MODE=token
CITO_HUB_TOKEN=<strong-secret-token>
CITO_HUB_BIND=127.0.0.1
CITO_HUB_PORT=4291
CITO_HUB_DATA_DIR=/var/lib/cito-hub
# Set to the directory containing bundled <id>/plugin.json files.
CITO_BUILTIN_PLUGINS_DIR=/opt/cito/plugins
```

Install `/etc/systemd/system/cito-hub.service`:

```ini
[Unit]
Description=Cito Hub
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=cito
Group=cito
WorkingDirectory=/var/lib/cito-hub
EnvironmentFile=/etc/cito-hub.env
ExecStart=/usr/local/bin/cito hub
Restart=on-failure
RestartSec=2
TimeoutStopSec=15
UMask=0077
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now cito-hub
sudo systemctl status cito-hub
journalctl -u cito-hub -n 50 --no-pager
```

Keep secrets out of unit files, shell history, screenshots, and support logs. Restart the service after changing its environment file.

## Reverse proxy and HTTP contract

Any TLS reverse proxy can serve the Hub. Preserve `Authorization`, `Content-Type`, and the public `Host`; stream `/v1/events` without response buffering or caching, allow long-lived connections, and route `/v1/rpc` and `/health` to the same Hub. Do not add a mismatched browser `Origin`. A reconnecting SSE client must be able to pass `Last-Event-ID` through the proxy.

- `GET /health` is unauthenticated and returns `ok`, `pid`, `version`, and `auth_mode`. In OIDC mode it also returns `oidc` with `issuer`, `client_id`, and optional `redirect_port`; other modes omit `oidc`.
- `POST /v1/rpc` requires `Content-Type: application/json` (`415` otherwise); protected operations require authentication.
- `GET /v1/events` is the authenticated SSE stream.
- Authenticated Hubs (`token` or `oidc`) accept any syntactically valid `Host` authority when `Origin` is absent, including `cito.example.com`, whether the upstream binds loopback or a wildcard address. Cross-origin browser requests return `403`.
- Unauthenticated Hubs (`none`) retain DNS-rebinding protection: a loopback-bound Hub requires a loopback `Host` with its listening port; a wildcard-bound Hub requires a valid `Host` with that port.
- The local `cito daemon` on `:4290` always requires a loopback `Host` matching its daemon port (`127.0.0.1`, `localhost`, or `[::1]`). Proxy the Hub on `:4291`, never the daemon.

Optional Caddy example (authenticated upstream on loopback):

```caddyfile
cito.example.com {
    reverse_proxy 127.0.0.1:4291 {
        flush_interval -1
    }
}
```

Optional Nginx location inside an HTTPS `server` block with your TLS certificate configured:

```nginx
location / {
    proxy_pass http://127.0.0.1:4291;
    proxy_http_version 1.1;
    proxy_set_header Host $http_host;
    proxy_set_header Authorization $http_authorization;
    proxy_set_header Content-Type $http_content_type;
    proxy_set_header Connection "";
    proxy_buffering off;
    proxy_cache off;
    proxy_read_timeout 1h;
}
```

## OIDC sign-in (PKCE)

Register a public/native OIDC client supporting Authorization Code with PKCE (`S256`), discovery, and ID tokens. Use an HTTPS issuer such as `https://id.example.com`; do not embed a client secret. Register `http://127.0.0.1:8400/callback` as an allowed loopback redirect. This callback runs on each client Mac, independently of the public Hub URL and Hub port. Client `redirectPort` accepts `0..=65535`; explicit `0` selects an ephemeral loopback port. When absent, `CITO_OIDC_REDIRECT_PORT` supplies a fallback; if both are absent, Cito selects an ephemeral port, which requires provider support for dynamic loopback redirects.

Set `CITO_AUTH_MODE=oidc`, `CITO_OIDC_ISSUER=https://id.example.com`, and `CITO_OIDC_CLIENT_ID=cito` on the server. Optionally set `CITO_OIDC_REDIRECT_PORT=8400` or pass `cito hub --redirect-port 8400` to advertise the callback port via `/health`; the server accepts `1..=65535`. Set `CITO_OIDC_ADMIN_USERS=alice,bob` to grant shared channel administration to those verified owner usernames or OIDC subjects. Other users can pair their own accounts after an administrator configures the shared service.

Join from a Mac with `cito setup --hub work --url https://hub.example.com --default`, or enter the URL using **Join from Clipboard** / **Add Hub…**. The client discovers OIDC settings from `/health`; explicit settings override discovered values. If the Hub is unreachable, unspecified authentication falls back to `none`; explicitly requested OIDC requires an issuer and client ID. A discovered token Hub requires a supplied token. Then use **Sign In…** in Cito. To override the callback port, use `cito setup --hub work --url https://hub.example.com --redirect-port 8400` or configure `~/.cito/config.json`:

```json
{
  "default_hub": "work",
  "hubs": {
    "work": {
      "url": "https://cito.example.com",
      "authMode": "oidc",
      "issuer": "https://id.example.com",
      "clientId": "cito",
      "redirectPort": 8400,
      "scopes": ["openid", "profile", "email"],
      "enabled": true
    }
  }
}
```

## Telegram shared service and personal pairing

1. Create a bot with Telegram's `@BotFather` and retain the Bot Token as a server secret.
2. Configure the token once per Hub. An authorized administrator can save it in **Settings → Human Channels → Shared Hub Service**. For unattended startup, set `CITO_PLUGIN_TELEGRAM_BOT_TOKEN` in the Hub's environment.
3. Each user chooses **Pair Account…** in **Settings → Human Channels** on that Hub and sends the live `/start cito-<code>` command to the bot in a private chat. Codes expire after ten minutes; use the generated command rather than an example code.
4. Use **Send Test** to confirm delivery. Users manage personal bindings and inbox Pause/Resume separately from the shared token. Agents address people with `@self` or `@username` through ordinary Hub signals.

Run exactly one poller per Bot Token. Telegram returns **409 Conflict** if another Hub, an old service instance, or another consumer is polling the same bot. Stop the duplicate poller (and remove any active webhook before using polling); use a separate bot for each independent Hub. Restarting several replicas against the same token cannot provide active-active polling.

Startup seeding follows `CITO_PLUGIN_<ID>_<KEY>` for declared **Hub-scoped** fields only. Uppercase the plugin ID and replace hyphens with underscores; convert camelCase field keys to uppercase snake case (`telegram` + `botToken` → `CITO_PLUGIN_TELEGRAM_BOT_TOKEN`). The uppercase unsplit key (`BOTTOKEN`) is a compatibility fallback. Non-empty values overwrite persisted service fields on every Hub startup; remove a seed if later UI changes should persist across restarts. Personal Chat IDs remain per-user settings, outside this mechanism.

## SQLite WAL backup and restore

The standalone data directory contains `hub.sqlite` plus live `hub.sqlite-wal` and `hub.sqlite-shm` sidecars. It also contains user plugins and their configuration. Channel credentials and personal bindings are sensitive; protect and encrypt backups.

For an online consistent database snapshot, use SQLite's backup API through an installed `sqlite3` CLI (the service account needs access):

```sh
sqlite3 /var/lib/cito-hub/hub.sqlite ".backup '/srv/backups/cito-hub.sqlite'"
sqlite3 /srv/backups/cito-hub.sqlite 'PRAGMA integrity_check;'
```

Create the backup directory with appropriate permissions first. Back up custom plugins and service environment files separately. Do not copy only `hub.sqlite` from a running service: committed transactions may still be in the WAL. For a whole-directory or Docker-volume backup, stop the service/container, verify shutdown, then snapshot the entire data directory or named volume. Do not use `docker compose down -v`, which deletes the volume.

To restore: stop the Hub, preserve the current data directory for rollback, restore the verified snapshot as `hub.sqlite` in an empty data directory, restore custom plugins as needed, and assign ownership to the service account. Never reuse stale `-wal`/`-shm` files alongside a restored snapshot. For a cold whole-directory backup, restore the complete consistent directory instead. Start one Hub instance and check `/health`, client sign-in, Directory, and a test signal. Use a compatible Cito release; do not assume a database downgrade is safe.

## Environment Variable Reference

These are production environment controls read by the native runtime, macOS UI, or bundled plugins. CLI flags override their standalone Hub counterparts. Client settings normally live in `config.json`; variables must be present in the process that reads them (restarting an existing daemon is necessary to change its environment). The tables include local developer overrides; leave diagnostic controls unset for normal use.

### Hub Server

| Variable | Default / behavior |
| --- | --- |
| `CITO_HUB_PORT` | `4291`; standalone `--port` (`0` requests an ephemeral port). |
| `CITO_HUB_BIND` | `127.0.0.1` with `none`, `0.0.0.0` with `token`/`oidc`; standalone `--bind`, an IP address. |
| `CITO_HUB_DATA_DIR` | `~/.cito/hub`; standalone `--data-dir`. Docker uses `/data`. |
| `CITO_AUTH_MODE` | `none`; standalone `--auth`: `none`, `token`, or `oidc`. Client config validation also recognizes this variable; configure per-Hub client authentication in `config.json`. |
| `CITO_HUB_TOKEN` | Required shared bearer credential in token mode; standalone `--token`. |
| `CITO_OIDC_ISSUER` | Required HTTPS issuer in OIDC mode; standalone `--issuer`. |
| `CITO_OIDC_CLIENT_ID` | Required public client ID in OIDC mode; standalone `--client-id`. |
| `CITO_OIDC_REDIRECT_PORT` | Optional advertised OIDC callback port, `1..=65535`; standalone `--redirect-port` takes precedence. |
| `CITO_OIDC_ADMIN_USERS` | Empty by default; comma-separated verified owner usernames or OIDC subjects allowed to administer shared channel fields. |
| `CITO_JWKS_COOLDOWN_MS` | `60000`; minimum cooldown between forced JWKS refreshes for unknown signing keys. |
| `CITO_PLUGIN_<ID>_<KEY>` | No default; seeds declared Hub-scoped channel fields at startup, including `CITO_PLUGIN_TELEGRAM_BOT_TOKEN`. |

### Client & Daemon

| Variable | Default / behavior |
| --- | --- |
| `CITO_STATE_DIR` | `~/.cito`; local state, sockets, staged runtime, and plugins. |
| `CITO_CONFIG_PATH` | `<state-dir>/config.json`; explicit multi-Hub configuration path. |
| `CITO_HUB` | Overrides the configured default Hub name (otherwise `local`). |
| `CITO_HUB_URL` | Overrides the active Hub URL; use `local://`, `http://`, or `https://`. |
| `CITO_NATS_URL` | Compatibility alias for `CITO_HUB_URL`, used only when the latter is absent; no NATS broker is involved. |
| `CITO_USER` | Local username override; otherwise configured user or OS username. OIDC identity comes from verified tokens. |
| `CITO_HOST_NAME` | Overrides the host display name; otherwise the system hostname. |
| `CITO_DAEMON_PORT` | `4290`; loopback daemon HTTP port, overrides `daemonPort` in config. |
| `CITO_OIDC_REDIRECT_PORT` | Loopback callback fallback when `redirectPort` is absent; integer `1..=65535`. An explicit `redirectPort: 0` selects an ephemeral port. |
| `CITO_BROWSER_BIN` | Browser opener used for OIDC login; platform default (`open` on macOS). |
| `CITO_TAILSCALE_HOST` | Overrides advertised Tailscale host discovery for Hub invitations. |
| `CITO_DISABLE_TAILSCALE_PROBE` | Set `1` to skip the Tailscale CLI probe. |
| `CITO_DISABLE_UI` | Set `1` to suppress automatic macOS companion launch. |
| `CITO_UI_BINARY` | Overrides the discovered macOS companion executable path. |
| `CITO_UI_HEADLESS` | Set `1` for headless UI diagnostics and a private pasteboard; also disables Tailscale probing. Leave unset for everyday use. |
| `CITO_CODESIGN_IDENTITY` | Signing identity for `cito setup --build-app`; falls back to `APPLE_SIGNING_IDENTITY`, then ad-hoc signing for local development. |

### Agent & Channel Plugins

| Variable | Default / behavior |
| --- | --- |
| `CITO_PLUGIN_PATH` | Colon-separated plugin-root search directories, highest priority, left-to-right. |
| `CITO_PLUGIN_DIR` | Legacy user plugin-root override, below `CITO_PLUGIN_PATH`. |
| `CITO_BUILTIN_PLUGINS_DIR` | Overrides bundled plugin-root discovery; Docker sets `/app/plugins`. |
| `CITO_PI_HOME` | `~/.pi/agent`; Pi configuration and session root. |
| `CITO_CODEX_HOME` | `~/.codex`; Codex configuration and thread database root. |
| `CITO_CLAUDE_HOME` | `~/.claude`; Claude configuration and transcript root. |
| `CITO_CODEX_BIN` | Overrides Codex executable discovery for wake delivery. |
| `CITO_OPEN_BIN` | Overrides `/usr/bin/open` for the Codex macOS open action. |
| `CITO_CLAUDE_CHANNEL` | Set `1` in Claude's MCP environment to enable its streaming wake sidecar; setup writes this automatically. |
| `CITO_ENDPOINT_PID` | Endpoint attribution hint set by MCP launch shims; live ancestry checks determine the enclosing harness. Do not set a global unrelated PID. |
| `CITO_PI_PROCESS_DISCOVERY` | `0` disables process-only hints; `1` permits process-only resolution when the bridge is not required. Normal discovery still prefers live bridges. |
| `CITO_PI_REQUIRE_BRIDGE` | Set `1` to require a live Pi bridge even with process-only resolution enabled. |
| `CITO_PI_ALLOW_HEADLESS_DISCOVERY` | Set `1` only in controlled synthetic fixtures to bypass the interactive-TTY discovery gate; leave unset for normal sessions. |

For plugin protocol fields and operation budgets, see [Plugin Authoring](https://tefx.github.io/cito/docs/plugins.html). For Node identity, signaling, and CLI/MCP interfaces, see [Architecture & Reference](https://tefx.github.io/cito/docs/architecture.html).
