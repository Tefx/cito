# Cito

Cito is a notification bus for opted-in coding-agent conversations across users, machines, harnesses, and NATS hubs. One `cito-mcp` stdio server connects directly to all configured hubs; no gateway service is required for multi-hub routing. Pi Coding Agent and Codex Desktop ship with working drivers. Other CLI/IDE harnesses can use the socket bridge protocol or register an adapter.

## Build and test

Requirements: Node **22.13+** on supported even-numbered releases (see `package.json`), npm, and macOS Command Line Tools (`/usr/bin/swiftc`) for the native HUD. The Pi bridge targets **Pi 1.5.0**.

```sh
npm ci
npm run typecheck
npm run build
npm test
# Focused suites:
npm test -- tests/mcp/ tests/e2e/
```

`bin/cito-mcp` and `bin/cito-auth-callout` load TypeScript through `tsx`, independently of the caller's working directory. The build also emits JavaScript/declarations under `dist/`.

Tests use the installed official `nats-server` **2.14.0**, temporary SQLite databases, real Unix sockets, a local OIDC/JWKS provider, and executable Desktop protocol fixtures. macOS tests compile and run the real Swift HUD headlessly, including click/retirement synchronization. The installed-Pi loader test uses Pi 1.5.0 when available. Non-macOS hosts skip native HUD tests. All test state and child processes are isolated and cleaned up. Tests require no Docker, live OIDC account, or running Desktop app. Visible HUD presentation, installed `ChatGPT.app` compatibility, live Casdoor, and production TLS/deployment require operator verification.

## NATS deployment

For a local unauthenticated broker:

```sh
docker compose -f deploy/nats/docker-compose.yml up -d nats
# Or use the official server directly:
CITO_NATS_STORE_DIR=/absolute/path/to/nats-data \
  nats-server -c deploy/nats/nats-server.conf
```

The default deployment binds loopback. Remote deployments need an operator-selected interface and TLS/network controls. See [deployment instructions](deploy/nats/README.md) for Compose, OIDC account bootstrap, optional encrypted Auth Callout, and service credentials. Clients reject incompatible stream/KV retention settings without destroying broker data.

In OIDC mode, run `bin/cito-auth-callout` alongside NATS. It verifies RS256/ES256 `id_token` signatures and grants CITO account JetStream/KV permissions, including `$JS.ACK.>`. Only the responder receives the **SA Account seed** and exempt service password; MCP clients receive neither. Official NATS rejects an SO Operator issuer in the non-operator callout configuration.

## Pi Coding Agent (`pi`)

Install the self-contained extension by adding `"npm:@tefx/cito"` to `packages` in `~/.pi/agent/settings.json`, or load the source file directly:

```sh
CITO_STATE_DIR="$HOME/.cito" pi -e /absolute/path/to/cito/src/pi-extension/cito-bridge.ts
```

For a standalone installation, copy only `src/pi-extension/cito-bridge.ts` into `~/.pi/agent/extensions/cito-bridge.ts` and restart Pi. The file needs no Cito or Pi package imports.

Configure `cito-mcp` separately in your Pi MCP client/gateway, using this stdio server definition and the **same `CITO_STATE_DIR`** as the extension:

```json
{
  "command": "/absolute/path/to/cito/bin/cito-mcp",
  "args": [],
  "env": {
    "CITO_STATE_DIR": "/Users/you/.cito",
    "CITO_NATS_URL": "nats://127.0.0.1:4222"
  }
}
```

Alternatively use `"command": "npx", "args": ["-y", "@tefx/cito@0.1.0"]` for the published package. The extension provides host delivery; your MCP client exposes the tools to the agent.

In a conversation, call `cito_listen({about: "Build verification", tag: "build"})`, then use `cito_find()` to discover other opted-in conversations and `cito_send({to: "<discovered-id>", message: "Build is ready"})` to notify one. Notifications enter the target conversation via `pi.sendMessage(..., {triggerTurn: true, deliverAs: "followUp"})`, waking idle sessions or entering a busy session's follow-up queue. Confirmed insertion automatically acknowledges the local signal. Use `cito_inbox()` to drain any unconfirmed/unavailable deliveries, and `cito_unlisten()` to opt out.

The extension listens on `sockets/pi-pid-<pid>.sock` (`0600`) from `session_start` until shutdown. Discovery walks the MCP process's parent chain, or uses `_meta.endpoint_pid`, `_meta.pi_pid`, or `_meta.caller_pid`. A local gateway can forward nested PID roots; explicit roots prevent fallback to an unrelated parent. The bridge returns both `endpoint_pid` and `pi_pid`. Session switches clear stale badges immediately, and a per-session 200-batch deduplication ring prevents repeated insertions on socket retries.

## ChatGPT / Codex Desktop (`codex`)

Configure the stdio server in `~/.codex/config.toml`:

```toml
[mcp_servers.cito]
command = "/absolute/path/to/cito/bin/cito-mcp"
args = []

[mcp_servers.cito.env]
CITO_STATE_DIR = "/Users/you/.cito"
CITO_NATS_URL = "nats://127.0.0.1:4222"
CITO_AUTH_MODE = "none"
```

For the published package use `command = "npx"` and `args = ["-y", "@tefx/cito@0.1.0"]` instead.

The host must supply the exact conversation ID in MCP request `_meta` using one of:

```json
{ "x-codex-turn-metadata": { "thread_id": "<thread-id>" } }
```

`x-codex-turn-metadata` may also be a JSON string or contain `session_id`; `openai/threadId` and `codexThreadId` are accepted top-level aliases. Multiple distinct thread claims or a thread claim combined with a top-level bridge PID return `INVALID_INPUT`. Cito never guesses a thread from recent activity or window focus.

Call `cito_listen({about: "Review service changes"})` from the conversation. Enrollment reads only that thread from `CITO_CODEX_HOME/state_5.sqlite`, checks that it is unarchived, and verifies its live owner through `ipc/ipc.sock`. On notification, the driver runs:

1. `codex queue --thread <id> --message <card>` with `CODEX_HOME` set to the configured home.
2. `/usr/bin/open -g codex://threads/wake/<id>` to trigger processing.

A clean queue exit plus successful wake confirms delivery and auto-acknowledges the batch. A SIGTERM queue timeout has an unknown insertion result: Cito records the queue milestone and proceeds to the wake URL without repeating queue insertion. Failed wake attempts or uncertain insertion produce `delivered_unconfirmed`; these signals stay unread in `cito_inbox()`. Exhausted queue failures stay readable with `held_reason: "driver_unavailable"`.

`cito_inbox()` drains up to 50 unread signals by default. Use `cito_inbox({ack_all: false})` to peek. Inbox and unlisten support archived or missing threads via retained local session metadata. The native HUD opens `codex://threads/<id>` when clicked; it never reads or writes SQLite.

## Multi-hub configuration and scoped addressing

Create `~/.cito/config.json` (or set `CITO_CONFIG_PATH`) to connect to multiple hubs simultaneously:

```json
{
  "default_hub": "personal",
  "hubs": {
    "personal": {
      "natsUrl": "nats://personal-tailnet-host:4222",
      "authMode": "none",
      "user": "alice"
    },
    "rda": {
      "natsUrl": "wss://nats.company.example/ws",
      "authMode": "oidc",
      "oidcIssuer": "https://login.company.example",
      "oidcClientId": "cito",
      "oidcScopes": ["openid", "profile", "email", "offline_access"]
    }
  }
}
```

Hub names match `^[a-z0-9][a-z0-9_-]{0,31}$`. Each entry defaults to `authMode: "none"` and the environment/OS username. OIDC entries require issuer and client ID. The selected default is `CITO_HUB` (trimmed), then `default_hub`, then the sole configured hub. Multiple hubs require an explicit default. A specified missing or invalid config file fails startup. Without a file, `CITO_*` environment variables synthesize the single hub `default`.

Each hub has its own connection, identity, OIDC cache (`oidc-token-<hub>.json`), login lock (`oidc-login-<hub>.lock`), and receivers. All hubs share the host ID and one local SQLite database. The `default` hub also accepts the old `oidc-token.json` cache. No secondary OIDC login is triggered by offline inbox reads: unauthenticated secondary hubs are skipped, while targeting one explicitly returns `UNAUTHORIZED`.

IDs returned to the agent use `<hub>:<id>` whenever multiple hubs or a named single hub are configured. Only a single hub literally named `default` returns unprefixed IDs for compatibility. Both input styles work; bare input IDs select the default hub unless `hub` is supplied. Prefixes and explicit `hub` must agree. Unknown/conflicting hubs are rejected before network or database activity. NATS payloads and SQLite ID columns always remain unprefixed.

```js
cito_listen({about: "Personal build", hub: "personal"})
cito_listen({about: "Corporate review", hub: "rda"})
cito_find()                          // parallel query of every configured hub
cito_find({hub: "rda", harness: "pi"})
cito_send({to: "rda:review-pbtjxcxkgw3y", message: "Review ready"})
// Equivalent addressing:
cito_send({hub: "rda", to: "review-pbtjxcxkgw3y", message: "Review ready"})
cito_inbox()                         // drain all authorized local hub inboxes
cito_inbox({hub: "rda", ack_all: false}) // peek at one hub
cito_inbox({ack_ids: ["rda:sig_..."]}) // acknowledge only these, without default draining
cito_unlisten({hub: "personal"})
cito_unlisten()                      // revoke all active hubs for this session
```

`in_reply_to` accepts the same optional prefix as signal IDs, and must refer to the send's hub. Scoped `ack_ids` must lie in the selected authorized inbox; bare signal IDs matching more than one hub return `INVALID_INPUT` without acknowledging anything. Inbox output sorts by creation time, hub, sequence, and signal ID. `cito_find` merges by descending revision, then hub and canonical ID, with a default limit of 20 and maximum 50. Find and unlisten include `warnings: [{hub, code, message}]` on partial hub failure. Find throws when all hubs fail; unlisten retains/restores active receivers on failed hubs and throws when all targets fail.

## Tools, identities and delivery guarantees

| Tool | Contract |
| --- | --- |
| `cito_listen({about, tag?, hub?, scope?})` | Opt in on one hub; default scope `self`. Omitted tag/scope preserve existing choices. |
| `cito_unlisten({hub?})` | Revoke one hub or every active hub, aborting pending host effects. |
| `cito_find({query?, hub?, user?, host?, harness?, repo?, limit?})` | Discover visible active leases across hubs; malformed KV records are skipped. |
| `cito_send({to, hub?, message, in_reply_to?, idempotency_key?})` | Always request delivery; no wake flag or unattended/pairwise quota. |
| `cito_inbox({hub?, ack_ids?, ack_all?})` | Read up to 50 unread, unexpired signals. Without `ack_ids`, omitted `ack_all` means true; with `ack_ids`, it means false. |

Inputs reject unknown properties. Messages/about contain 1–500 sanitized Unicode codepoints. `scope: "self"` restricts visibility and delivery to the same principal; `workspace` allows other users. Self-send protection compares principal, host, harness, and session. Find/send can use a distinct unbound sender when caller discovery is unavailable; conflicting explicit claims always fail.

Public listener IDs use `<slug>-<fingerprint>`. `tag` accepts up to 64 characters and normalizes to at most 20 lowercase ASCII letters/digits/hyphens; an empty normalized tag is invalid. Without a tag, the slug uses repository, directory basename, conversation title, then harness. Slug renames preserve the deterministic canonical `c_<fingerprint>` identity and route through old public aliases. Both canonical and public IDs work with hub prefixes.

- `host-id` holds one atomically published UUIDv4 (`0600`). State directories use `0700`; `state.sqlite` uses `0600`, WAL, and a 5-second busy timeout.
- **Local schema reset:** `PRAGMA user_version = 4` uses only `local_listeners`, `local_signals`, and `local_sent_idempotency`, with hub-qualified primary keys. Opening an older local schema atomically drops its local registrations, inbox, idempotency data, and legacy pairwise table. Re-enroll after upgrading. Broker data is not reset by this operation.
- KV CAS, generations, owner tokens, and hub-qualified listener/receiver locks fence concurrent owners. The first online tool after restart restores matching persisted active registrations and replay cursors. Receiver-lifetime locks prevent duplicate adoption.
- Healthy listeners renew every 20 seconds. Find hides leases older than 60 seconds; sends buffer through 30 minutes of disconnection. Reattachment records detected replay gaps until an `ack_all` drain.
- Admission persists the signal and replay cursor atomically. Duplicate `(hub, signal_id)` does not redeliver. Stale generations remain passive. Unauthorized private messages advance the cursor and drop silently.
- Admitted signals debounce for 1500ms into immutable, persisted batches with hub-qualified batch IDs. Every external effect rechecks ownership under its listener lock. Acknowledging before dispatch suppresses that effect; cancellation stops retries and subsequent wake effects.
- Confirmed delivery sets `DISPATCHED` and auto-acknowledges; unconfirmed delivery sets `DISPATCHED` and remains unread. Unavailable drivers retain passive inbox signals. Session switches revoke the old conversation's bindings on all hubs.
- Pi deduplicates frozen batches. Codex queue milestones survive restart. A crash between a Desktop effect and its local commit can duplicate a card; the persisted batch ID identifies it.
- Flush/publish/ack waits share a 10-second deadline and are interrupted by shutdown. Network acceptance does not imply confirmed insertion into a conversation.

### Wake cards and native HUD

```text
🔔 Cito · alice@mac [↩ rda:review-pbtjxcxkgw3y]

Review is ready.
```

Cards escape angle brackets, strip `.local` from hosts, omit duplicate sender titles, and include scoped reply IDs. Batches use numbered senders. There is no acknowledgement footer. Pi's self-contained renderer adds the host theme's background, header accent, and left body bar.

Pi's badge aggregates all active hubs for its session: `🔔 <slug>`, unread count `(N)`, replay gap `(!)`, or both `(N!)`. Auto-acks, inbox commits, and enrollment/revocation refresh it under a session-wide lock. Closing one hub cannot clear another active hub's badge; closing an observer cannot clear another process's live ownership.

The macOS HUD uses a dark 388×136pt nonactivating panel. It compiles on demand and has **no SQLite dependency**. The Codex action reads “点击跳转至 Codex 对话 ↗”; other harnesses read “<harness> 终端会话 · 点击关闭横幅” and dismiss on click. Counts aggregate for the exact target/session/wake tuple. Headless diagnostics report the same presentation fields used by the UI.

## Extending Cito with custom harnesses

Harness identifiers are open strings matching `^[a-z][a-z0-9_-]{0,31}$`. New socket-backed harnesses need no edits in `src/mcp/` and no registry entry. Provide a Unix socket at `${CITO_STATE_DIR}/sockets/<harness>-pid-<pid>.sock`. For example, an IDE companion can expose `cursor-pid-12345.sock`, and forward `_meta.endpoint_pid: 12345` to MCP calls.

The protocol is one newline-delimited JSON request/reply per connection:

```js
// identify (1500ms deadline)
{op: "identify"}
// reply
{ok: true, endpoint_pid: 12345, session_id: "stable-session", session_name: "Review", cwd: "/repo"}
// insert into the conversation / follow-up queue (3000ms deadline)
{op: "deliver", session_id: "stable-session", batch_id: "batch_...", wake: true, content: "🔔 Cito ..."}
// reply only after insertion; deduplicate batch_id per session
{ok: true, delivered: true, deduplicated: false}
// optional status badge
{op: "status", session_id: "stable-session", badge: "🔔 review (2)"}
{ok: true}
```

Return `{ok: false, error: "SESSION_NOT_OPEN"}` when no session exists, and `SESSION_MISMATCH` before status/delivery if the session changed. Omitted `badge` clears it. Keep socket state private (`0700` directory, `0600` socket); remove the endpoint on shutdown. Parent-process discovery considers registered socket adapters first, then any matching harness socket, walking only supplied PID roots or the default parent chain through depth 5. Git metadata ignores inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, and `GIT_COMMON_DIR`.

For hosts that only confirm queue/channel submission, register `new SocketBridgeAdapter("cursor", {confirmDelivery: false})` before creating the service. It returns `delivered_unconfirmed`, retaining inbox fallback.

For non-socket drivers, implement `HarnessAdapter` from `src/adapters/types.ts`, then call `registerAdapter(customAdapter)` from `src/adapters/index.ts` in your in-process entrypoint. Implement `probe`, `deliver`, and optional `updateBadge`; delivery returns `delivered`, `delivered_unconfirmed`, `driver_unavailable`, or `session_switched`. Use `ctx.runEffect` for each external effect so retries preserve cancellation and ownership fencing. Only return `delivered` after confirmed host insertion. Use `CitoService.create(config?, options?)`, `service.call(name, args, meta)`, and `await service.close()`; `createMcpServer(service)` supplies SDK stdio handlers. Non-socket caller identity currently uses explicit Codex thread metadata; custom harnesses still need an identity socket for discovery even if delivery uses another registered driver.

### Claude Code (`claude`) — 尚未实现 / Not Yet Implemented

Cito does **not** ship a runtime Claude Code adapter. [DESIGN.md §8](docs/DESIGN.md#8-proposed-design-for-claude-code-adapter-设计建议--仅建议尚未实现) proposes a companion `<harness>-pid-<pid>.sock` bridge using `SocketBridgeAdapter`, MCP Channel Push for idle wakeup, and a turn-start `cito_inbox()` drain when push delivery is unconfirmed. The open harness schema permits `claude` as an identifier; it does not install that proposed host integration.

## Environment

| Variable | Default / requirement |
| --- | --- |
| `CITO_CONFIG_PATH` | Optional explicit hub config JSON; missing/invalid paths fail startup. |
| `CITO_HUB` | Override the default hub from the file. |
| `CITO_NATS_URL` | `nats://127.0.0.1:4222`; `nats://`, `ws://`, `wss://` supported. Also overrides the selected file hub unless `CITO_CONFIG_PATH` is set. |
| `CITO_AUTH_MODE` | Single-default-hub fallback: `none` (default) or `oidc`. |
| `CITO_USER` | OS username; 1–64 letters/digits/dots/underscores/hyphens. |
| `CITO_OIDC_ISSUER` / `CITO_OIDC_CLIENT_ID` | Single-hub OIDC issuer and client/audience. HTTPS required except loopback tests. |
| `CITO_OIDC_SCOPES` | Space-separated fallback scopes; default `openid profile email offline_access`. |
| `CITO_STATE_DIR` | `~/.cito`; share between bridge and MCP. |
| `CITO_HOST_NAME` | OS hostname, at most 128 printable ASCII characters. |
| `CITO_CODEX_HOME` | `~/.codex`; reads only explicitly identified threads. |
| `CITO_CODEX_BIN` | `/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex`. |
| `CITO_OPEN_BIN` | `/usr/bin/open`; configurable Codex wake executable. |
| `CITO_AUTH_CALLOUT_ISSUER_SEED` | Responder only: required `SA...` Account signing seed. |
| `CITO_AUTH_CALLOUT_PASSWORD` | Responder/bootstrap only: exempt service password. |
| `CITO_AUTH_CALLOUT_XKEY_SEED` | Optional responder `SX...`, paired with the broker's public XKey. |

OIDC verifies signatures before caching tokens and the offline principal (`0600`). Online connections verify cached tokens again. Per-hub rejected-token recovery rechecks under its login lock, refreshes silently, and falls back to S256 loopback PKCE only for missing refresh credentials or HTTP 400/401. Refresh network/5xx failures return `AUTH_UNAVAILABLE` without opening a browser. Lock wait and login share a 45-second deadline. Broker reconnect authorization failures defer login until the next online tool call.

Cito installs no `launchd` daemon, implements no custom HTTP/WebSocket broker, and uses no external messaging side channel for delivery.
