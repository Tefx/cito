# Native Cito Hub Deployment

`cito hub` runs the Rust HTTP/SSE server and the same system-SQLite-backed `HubEngine` used by the local daemon. It needs no Node.js runtime or external broker. The database is `${dataDir}/hub.sqlite`, opened in WAL mode; upgrades add missing tables and columns without replacing existing data.

## Standalone CLI

```sh
# Trusted local network (loopback by default)
cito hub --port 4291 --auth none --data-dir /var/lib/cito-hub

# Shared bearer token; use HTTPS or a private network for remote access
cito hub --bind 0.0.0.0 --auth token --token '<strong-secret-token>' --data-dir /var/lib/cito-hub

# Multi-user OIDC; terminate public HTTPS at a reverse proxy
CITO_OIDC_ADMIN_USERS=alice \
  cito hub --bind 0.0.0.0 --auth oidc --issuer https://id.example.com --client-id cito --data-dir /var/lib/cito-hub
```

Flags also accept `--key=value`. Their environment equivalents are `CITO_HUB_PORT`, `CITO_HUB_BIND`, `CITO_AUTH_MODE`, `CITO_HUB_TOKEN`, `CITO_OIDC_ISSUER`, `CITO_OIDC_CLIENT_ID`, and `CITO_HUB_DATA_DIR`. Blank environment variables count as unset; explicitly empty CLI values are errors. Configuration-validation diagnostics identify the applicable environment setting without echoing credential values. The default data directory is `~/.cito/hub`. `SIGTERM` and `SIGINT` stop the HTTP listener, close SSE connections, and close/reap channel subprocesses.

`GET /health` is unauthenticated and returns `ok`, `pid`, `version`, and `auth_mode`. RPCs use `POST /v1/rpc` and retain the flat `{ "op": "...", ...params }` wire format; `{ "op": "...", "params": {...} }` is also accepted. `/v1/events` sends the existing `kv`, `signal`, and `nc` SSE frames. OIDC issuer, discovery, and JWKS endpoints require HTTPS, with the compatible `http://127.0.0.1` exception for local test fixtures. Other plain-HTTP hosts are rejected. The server verifies RS256, RS384, RS512, PS256, ES256, ES384, and EdDSA signatures, issuer, audience, subject, expiration, and `nbf`. Clock tolerance is 30 seconds, with strict `exp > now - 30` and `nbf <= now + 30`. Cache misses share a refresh, with a 60-second refresh cooldown; known cached keys remain usable after refresh failures. Discovery and JWKS requests each have a five-second timeout and reject redirects. Discovery/JWKS failures during a cache miss return transient `AUTH_UNAVAILABLE` (HTTP 503); malformed JWT headers and signature/claim denials remain authorization failures (HTTP 401). Only a node's owner receives its signal/ack SSE events in OIDC mode. Signal message retrieval permits only the recipient owner or sender, including messages addressed to workspace-visible nodes. Workspace directory entries remain discoverable without granting access to message content or other users' signal/ack SSE events. Verified OIDC identities retain access to legacy `<issuer>#<sub>` ownership records while new identities use `oidc:<issuer>|<sub>`. Existing channel-binding namespaces and single-message external ID mappings survive the upgrade. KV and stream payloads remain byte-transparent, including empty values, NUL, and non-UTF-8 bytes. `data_b64` carries canonical binary payloads; raw `data` inputs accept UTF-8 text, byte arrays, serialized Buffer forms, or JSON values. Registration/signal metadata is parsed opportunistically for ownership checks without rewriting stored payloads. Publishing `nc` signals uses the durable signal stream; `nc` acknowledgments retain their original byte payloads. `account_info.streams` counts only persisted stream configurations. Reading built-in `stream_info` defaults does not create managed stream records; explicit `stream_add` or `stream_update` persists them.

## Docker Compose

The multi-stage Dockerfile builds with `rust:1-bookworm`, then runs on `debian:bookworm-slim` as an unprivileged user. The runtime contains system SQLite, CA certificates, the native executable, and the native bundled channel plugin.

```sh
# No authentication; published port is restricted to host loopback
 docker compose -f deploy/hub/docker-compose.yml up -d --build

CITO_AUTH_MODE=token CITO_HUB_TOKEN='<strong-secret-token>' \
  docker compose -f deploy/hub/docker-compose.yml up -d --build

CITO_AUTH_MODE=oidc CITO_OIDC_ISSUER=https://id.example.com CITO_OIDC_CLIENT_ID=cito \
  CITO_OIDC_ADMIN_USERS=alice \
  docker compose -f deploy/hub/docker-compose.yml up -d --build
```

The named volume persists `/data/hub.sqlite`. Back up that database with SQLite's online backup facilities or stop the container before copying its database/WAL files. Configure a reverse proxy or private network before exposing the port to remote clients.

## Shared Human Channels

Hub-scoped plugins run as persistent subprocesses over the HumanChannel NDJSON protocol. Install additional `plugin.json` directories under `${dataDir}/plugins/<id>/`; `CITO_PLUGIN_DIR` adds another discovery root. User-scoped desktop UI plugins remain daemon-owned.

For the bundled channel, set `CITO_PLUGIN_TELEGRAM_BOT_TOKEN='<bot-token>'` on the server. Compose forwards this variable. Environment defaults are derived generically from each manifest's hub fields (`CITO_PLUGIN_<ID>_<SNAKE_CASE_FIELD>`). In OIDC mode, only configured admin usernames or subjects in `CITO_OIDC_ADMIN_USERS` may modify hub credentials through any configure, pair, or test RPC. Users can still save their own account configuration. Channel-list responses never expose hub secrets.

Users choose **Pair Account…** on the target Hub and follow the plugin-provided instruction. Codes contain the `cito-` routing prefix plus ten uppercase Base32 (`A-Z2-7`) characters encoding six CSPRNG bytes (48 bits), without `=` padding, and expire after ten minutes. Instructions and redemption preserve token case. The exact instruction persists across restarts. Pairing requires a private conversation and binds both external conversation and user IDs. Newly enrolled remote human nodes default to private (`self`) scope. Pair an existing manually configured or pre-upgrade account again if it lacks a verified external user identity; delivery configuration alone does not authorize inbound replies.

Each outgoing notification acquires a persisted reservation before subprocess I/O. Failed earlier notifications remain eligible even after later notifications succeed; pre-binding history is excluded by a durable initial sequence boundary. Multipart external IDs all map to their source signal. An abandoned reservation becomes retryable after 60 seconds. Delivery across a process crash remains **at least once**: an external send can succeed before its result is committed. Inbound reply deduplication, reply publication, and original-message business acknowledgment commit together before `reply_ack`. Replies reject missing, revoked, private-to-another-user, or expired recipients; disconnected registrations inside the 30-minute retention window remain valid queued recipients.

Disabling every binding and clearing the hub configuration stops the plugin. Closing the parent pipe triggers the plugin-side stdin EOF exit rule; the host also enforces a bounded shutdown and reaps the process. Pending `format_pair`/`test` requests fail immediately when stdout closes.

## Embedded Daemon Integration

Use one `HubEngine::open(state_dir)` for every `local://` slot. To expose that same engine over LAN, call `engine.start_server(bind, port, HubAuth::new(...))`; never open a second database or engine for the listener. `HubServer::shutdown()` stops only that listener, preserving the engine for local callers. Standalone services can use `HubEngine::open_with_remote(data_dir, true)` so restored or newly paired human nodes retain remote-private defaults.

`engine.rpc(op, params, principal)` accepts a verified principal or trusted local `None`. HTTP authentication supplies verified OIDC principals; callers must never treat decoded-but-unverified JWT claims as trusted input. When matching persisted ownership outside the engine, use `auth::principal_matches` / `principal_ids_equal` to recognize the verified legacy OIDC identity alias. `engine.subscribe(principal)` supplies filtered wire events; a slow subscriber is disconnected so its client reconnects and resumes from durable consumer state.

Shutdown order: stop admitting new daemon work, drain in-flight operations, stop/drop each `HubServer`, then call `engine.shutdown()`. Shutdown prevents new channel subprocesses from starting, closes event subscribers, closes plugin stdin, reaps subprocesses, and joins the background retry worker. The server borrows a clone of the engine and never shuts down another owner's local engine.

## Regression Checks

```sh
CARGO_TARGET_DIR="$PWD/.scratch/native-hub/target" \
  cargo test --manifest-path crates/cito-native/Cargo.toml --test native-hub
```

Tests use disposable SQLite databases, local HTTPS and HTTP-loopback discovery/JWKS fixtures, real signatures for all seven supported asymmetric algorithms, and a protocol-only channel subprocess. They do not contact an external identity provider or messaging service. Python 3 and OpenSSL are test prerequisites; neither is required by the native Hub runtime.
