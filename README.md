# Cito

Cito delivers short notifications between opted-in Codex Desktop and Pi conversations across users and machines. Official **NATS JetStream + KV** provides discovery, ordered replay, and 30-minute signal retention. Cito stores notifications, never external business payloads.

## Build and test

Requirements: Node **22.13+** on the supported even-numbered releases (see `package.json`), npm, and macOS Command Line Tools (`/usr/bin/swiftc`) for the native HUD. The Pi bridge targets **Pi 1.5.0**. No SQLite npm addon, Docker, live OIDC provider, or running Desktop application is needed for tests.

```sh
npm ci
npm run typecheck
npm run build
npm test
# Focused suites:
npm test -- tests/mcp/ tests/e2e/
```

`bin/cito-mcp` and `bin/cito-auth-callout` load their TypeScript sources through the installed `tsx` dependency, independently of the caller's working directory. `npm run build` also emits JavaScript/declarations under `dist/`.

Tests use the installed official `nats-server` **2.14.0** binary, temporary SQLite databases, real Unix sockets, a mock OIDC/JWKS provider, and executable Desktop protocol fixtures. macOS HUD tests compile and run the real Swift executable in headless mode, including click/retirement synchronization. The optional installed-Pi test uses the actual Pi 1.5.0 loader when available. Non-macOS hosts skip native HUD tests. Tests isolate `CITO_STATE_DIR` and `CITO_CODEX_HOME` and stop their processes before deleting state.

The 30-minute boundary tests supply broker-clock samples to the production lease/gap calculations; they do not wait 30 minutes. Desktop compatibility with an installed `ChatGPT.app`, visible HUD presentation, live Casdoor, and production Docker/TLS deployment require operator verification.

## NATS deployment

For a local, unauthenticated broker:

```sh
docker compose -f deploy/nats/docker-compose.yml up -d nats
```

Or run the official server directly:

```sh
CITO_NATS_STORE_DIR=/absolute/path/to/nats-data \
  nats-server -c deploy/nats/nats-server.conf
```

The default configuration publishes ports on loopback. A remote deployment needs an operator-selected interface and TLS/network controls. See [`deploy/nats/README.md`](deploy/nats/README.md) for Compose, OIDC account bootstrap, optional encrypted Auth Callout, and required service credentials. Clients verify existing stream/KV configuration and reject incompatible retention settings without destroying data.

With `CITO_AUTH_MODE=oidc`, run `bin/cito-auth-callout` alongside NATS using the OIDC deployment configuration. The responder verifies RS256/ES256 `id_token` signatures and grants the CITO account's JetStream/KV/Cito permissions, including `$JS.ACK.>`. Only the callout process receives the **SA Account seed** and exempted service password; MCP clients receive neither. Official NATS rejects an SO Operator issuer in this non-operator callout configuration.

## MCP configuration

Run the same stdio server for either harness:

```sh
/absolute/path/to/cito/bin/cito-mcp
```

For a Codex MCP configuration:

```toml
[mcp_servers.cito]
command = "/absolute/path/to/cito/bin/cito-mcp"

[mcp_servers.cito.env]
CITO_NATS_URL = "nats://127.0.0.1:4222"
CITO_AUTH_MODE = "none"
```

A Pi stdio MCP gateway can use the equivalent server definition:

```json
{
  "command": "/absolute/path/to/cito/bin/cito-mcp",
  "args": [],
  "env": {
    "CITO_NATS_URL": "nats://127.0.0.1:4222",
    "CITO_AUTH_MODE": "none"
  }
}
```

Configure this definition in your existing MCP gateway; the bridge itself does not install an MCP client into Pi. Stdio stdout contains only MCP frames. Invalid environment configuration exits with status 1 and a diagnostic on stderr. Local state initializes at startup; NATS and OIDC login connect lazily when an online tool needs them, so an offline inbox remains accessible.

### Tools

| Tool | Purpose |
| --- | --- |
| `cito_listen({about, scope?, allow_peer_wake?})` | Enroll the calling session. Initial scope is `self`; peer waking defaults to false. Omitted preferences survive updates. |
| `cito_unlisten({})` | Revoke enrollment, abort pending host effects, retain unread inbox signals until expiry. |
| `cito_find({query?, user?, host?, harness?, repo?, limit?})` | Discover visible active leases. Harness accepts only `codex` or `pi`; default limit 20, maximum 50. |
| `cito_send({to, message, wake?, in_reply_to?, idempotency_key?})` | Notify an exact discovered listener ID. Wake defaults to true. Idempotency keys are sender-endpoint + target scoped for 30 minutes. |
| `cito_inbox({ack_ids?, ack_all?})` | Read up to 50 unacknowledged, unexpired local signals in sequence order and optionally acknowledge them. Works offline. |

All tool inputs reject unknown properties. Results use the common MCP text JSON envelope (`isError`, `ok`, and structured retryable error codes). Messages/about accept 1–500 Unicode codepoints after control stripping and trimming; wake cards escape angle brackets.

Receiver tools require the caller's session identity. Codex uses `_meta["x-codex-turn-metadata"].thread_id` (object or JSON string), `_meta["openai/threadId"]`, or `_meta.codexThreadId`. Enrollment additionally checks the exact thread's SQLite row and live Desktop owner socket. Inbox/unlisten can resolve an archived/closed, previously registered thread locally. Pi uses `_meta.pi_pid` / `_meta.caller_pid` or a six-level parent-process socket walk. Outbound find/send remain available when this resolution fails and use a distinct unbound sender endpoint.

For in-process consumers, use `CitoService.create(config?, options?)` from `src/mcp/service.ts`, `service.call(name, args, meta)`, and `await service.close()`. `createMcpServer(service)` from `src/mcp/server.ts` attaches the same handlers to an SDK `Server`. The owner of an in-process service must close both server and service. Options allow a browser opener, Desktop URL opener executable, and banner environment for controlled integrations.

### State and delivery guarantees

- `stateDir/host-id` holds one atomically published lowercase UUIDv4 (`0600`). MCP always uses that persisted identity. The state directory is `0700`; `state.sqlite` is `0600`, uses WAL and a 5-second SQLite busy timeout.
- Listener IDs are deterministic `c_[a-z2-7]{12}` values for principal + host + harness + session. Re-enrollment increments generation; KV CAS and per-listener `flock` serialize ownership changes. Losing owners cannot overwrite the winner's local row.
- The first online tool after an MCP process restart restores matching persisted `ACTIVE` enrollments, frozen batches and pull cursors without another `cito_listen`. A receiver-lifetime `flock` prevents a second MCP process from adopting a still-live owner. Failed revocation keeps or restores the active receiver; successful revocation removes only that exact receiver instance.
- Sender flush/publish/ack waits share a 10-second operation deadline and are interrupted by service shutdown.
- Healthy listeners renew every 20 seconds. Discovery hides leases older than 60 seconds; sends can buffer through 30 minutes of disconnection. Older active entries return `LISTENER_EXPIRED`. Reattachment detects listener-specific replay gaps and reports them until `ack_all`.
- Recipient-local admission persists quota and pairwise counters in the same transaction as the signal and replay cursor. Three unattended wakes are allowed; a sender endpoint can admit two waking signals per recipient in 120 seconds. Re-enrollment or elapsed time never resets the unattended quota. Only verified human-event sequence advancement resets it.
- Requested passive signals, disabled peer wakes, stale generations and anti-loop suppression stay readable in the inbox and show a passive HUD; they never insert a Pi transcript card or run Desktop queue/wake commands.
- Waking signals debounce for 1500ms into immutable, persisted batches. Every host-effect submission holds the same listener lock across its authorization check. Revocation/fencing cancels retries and prevents a later `open -g`. Inbox acknowledgements survive driver completion independently of dispatch state.
- Pi retries a frozen batch with bridge-side deduplication. Codex queue timeouts have an unknown insertion outcome: the dispatcher records `queue_written` and only retries the separate wake URL. A crash between an external effect and its SQLite commit can duplicate a Desktop card; the batch ID identifies it.

## Environment

| Variable | Default / requirement |
| --- | --- |
| `CITO_NATS_URL` | `nats://127.0.0.1:4222`; supports `nats://`, `ws://`, `wss://`. |
| `CITO_AUTH_MODE` | `none` or `oidc`; default `none`. |
| `CITO_OIDC_ISSUER` | Required in OIDC mode; HTTPS issuer (loopback HTTP allowed for tests). |
| `CITO_OIDC_CLIENT_ID` | Required with an issuer; registered OIDC client/audience. |
| `CITO_STATE_DIR` | `~/.cito`; use the same value for Pi host and MCP processes. |
| `CITO_USER` | OS username; 1–64 letters, digits, dots, underscores or hyphens, canonicalized to lowercase for local identity. |
| `CITO_HOST_NAME` | OS hostname, truncated to 128 printable ASCII characters. |
| `CITO_CODEX_BIN` | `/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex`. |
| `CITO_CODEX_HOME` | `~/.codex`; explicit-thread reads only, never advertising un-enrolled sessions. |
| `CITO_AUTH_CALLOUT_ISSUER_SEED` | Auth responder only: required `SA...` Account signing seed. |
| `CITO_AUTH_CALLOUT_PASSWORD` | Auth responder and OIDC broker bootstrap only: exempt service password. |
| `CITO_AUTH_CALLOUT_XKEY_SEED` | Optional responder `SX...`; pair with the broker's matching public XKey. |

OIDC clients verify signatures before caching `oidc-token.json` (`0600`), including the canonical principal/user used by an offline inbox. Online connections verify cached tokens again. Rejected-token recovery holds `oidc-login.lock`, rechecks for another process's different fresh token, silently refreshes, and falls back to S256 loopback browser PKCE only on missing refresh credentials or HTTP 400/401. Refresh network/5xx failures return `AUTH_UNAVAILABLE` without opening a browser. Lock wait + login share a 45-second deadline; the rejected operation retries authentication at most once. Broker reconnect authorization failures defer login until the next online tool call.

## Install the Pi 1.5.0 bridge

The extension imports only Node built-ins. Load it directly:

```sh
CITO_STATE_DIR="$HOME/.cito" pi -e /absolute/path/to/cito/src/pi-extension/cito-bridge.ts
```

For persistent installation, copy that single file into your Pi extension directory (for example `~/.pi/agent/extensions/cito-bridge.ts`) and restart Pi. No Cito package dependency is needed in that directory. Configure the stdio MCP server separately through your MCP gateway, with the same `CITO_STATE_DIR`. Parent-PID discovery works through intermediate local gateways; explicit `pi_pid` metadata is available when the MCP subprocess is outside that ancestor chain.

The bridge creates `sockets/pi-pid-<pid>.sock` (`0600`) on `session_start`, exposes identify/status/deliver operations, counts only interactive human input, and removes its socket on normal shutdown. Human counters and the 200-ID delivery-dedup ring are kept separately for each session, including when switching A → B → A. It delivers waking cards with `pi.sendMessage(..., { triggerTurn: true, deliverAs: "followUp" })` and displays unread status badges. Pi banner clicks dismiss without switching windows; Codex banner clicks increment the local human-event sequence before opening the target conversation.

## Scope

V1 supports **Codex Desktop (`ChatGPT.app`) and Pi 1.5.0**. Claude Code is a deferred proposal in [`docs/DESIGN.md`](docs/DESIGN.md) §9 and returns `UNSUPPORTED_HARNESS` when requested through find. Cito installs no `launchd` daemon, implements no custom HTTP/WebSocket broker, and never uses Telegram as a notification side channel.
