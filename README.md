# Cito

Cito is a notification bus for opted-in coding-agent conversations across users, machines, harnesses, and NATS hubs. One per-user `cito daemon` owns a state directory, all hub connections and receivers, and the persistent macOS `cito-ui` companion. It serves Streamable HTTP MCP at `http://127.0.0.1:4290/mcp` and health at `/health`. Direct HTTP clients need no per-conversation Cito subprocess; `cito-mcp` remains a small stdio auto-forwarder for clients that need stdio. Pi Coding Agent and Codex Desktop ship with working drivers. Other CLI/IDE harnesses can use the socket bridge protocol or register an adapter.

## Daemon architecture and startup

```sh
npm install -g @tefx/cito
cito daemon
# From a source checkout after npm ci:
/absolute/path/to/cito/bin/cito-mcp daemon
```

The daemon binds **only `127.0.0.1`**, rejects non-loopback HTTP Origins, and uses a transactional SQLite `daemon_owner` row plus PID/health checks to prevent a second owner of the same `CITO_STATE_DIR`. Starting it again on the same port exits 0; requesting another port for that state directory exits 1. On stale ownership after a crash, a new process can reclaim the directory. Each MCP HTTP request gets an independent transport/server sharing the service; closing a client never closes the daemon.

The stdio entrypoint checks matching `/health`, starts a detached daemon when needed, and waits up to 5 seconds before forwarding JSON-RPC. Configure the daemon environment or `~/.cito/config.json` before its first start: subsequent stdio clients reuse its configuration. Startup connects all hubs non-interactively and restores matching ACTIVE listeners; OIDC hubs lacking valid/refreshable tokens show `auth_required` without a browser.

For automatic login startup, configure a user LaunchAgent with absolute `ProgramArguments` (`/absolute/path/to/cito/bin/cito-mcp`, `daemon`), `RunAtLoad: true`, and `KeepAlive: { SuccessfulExit: false }`. Put `CITO_STATE_DIR` and other daemon environment overrides in that agent. Cito does not install or modify LaunchAgents. Menu Quit exits 0 and stays stopped with this policy; crashes restart. A subsequent stdio client can auto-start it again.

The dependency graph is `shared → adapters / ui → mcp`. MCP supports `2026-07-28` alongside existing `2025-*` clients. The MCP layer passes generic caller context (metadata, headers and OS-attributed PID) into registered adapters; only `CodexDesktopAdapter.detectExplicitCaller` extracts and merges host-specific thread headers. Conflicting explicit bridge PID and thread claims fail before enrollment. `src/ui/` depends only on shared types and Node built-ins; Swift owns Menu Bar Extra and HUD rendering, with no SQLite access. Daemon-local `KeyedMutex` critical sections replace every legacy file lock.

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

`bin/cito-mcp` and `bin/cito-auth-callout` prefer compiled JavaScript under `dist/` when present and fall back to loading TypeScript through `tsx`, independently of the caller's working directory. The build emits JavaScript/declarations under `dist/`.

Tests use the installed official `nats-server` **2.14.0**, temporary SQLite databases, real Unix sockets, a local OIDC/JWKS provider, and executable Desktop protocol fixtures. macOS tests compile and run the real Swift companion headlessly, including JSON-line framing, menu actions, HUD auto-hide, click behavior, EOF and crash/respawn replay. The installed-Pi loader test uses Pi 1.5.0 when available. The complete V4 acceptance suite requires macOS and Swift. All test state and child processes are isolated and cleaned up. Tests require no Docker, live OIDC account, or running Desktop app. Visible HUD presentation, installed `ChatGPT.app` compatibility, live OIDC providers, and production TLS/deployment require operator verification.

## NATS deployment

For a local unauthenticated broker:

```sh
docker compose -f deploy/nats/docker-compose.yml up -d nats
# Or use the official server directly:
CITO_NATS_STORE_DIR=/absolute/path/to/nats-data \
  nats-server -c deploy/nats/nats-server.conf
```

The default deployment binds loopback. Remote deployments need an operator-selected interface and TLS/network controls. See [deployment instructions](deploy/nats/README.md) for Compose, OIDC account bootstrap, optional encrypted Auth Callout, and service credentials. Clients reject incompatible stream/KV retention settings without destroying broker data.

In OIDC mode, run `bin/cito-auth-callout` alongside NATS. It verifies RS256, RS384, RS512, PS256, ES256, ES384 and EdDSA `id_token` signatures and grants CITO account JetStream/KV permissions, including `$JS.ACK.>`. Only the responder receives the **SA Account seed** and exempt service password; MCP clients receive neither. Official NATS rejects an SO Operator issuer in the non-operator callout configuration.

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

### Direct Session Control (`/cito` and CLI `cito listen` / `cito unlisten`)

The standalone Pi extension also registers the `/cito` slash command (with `on`, `team`, and `off` argument completions) so you can toggle or configure listening immediately without spending an LLM turn:

- `/cito`: toggle listening for the current Pi session (`cito_unlisten({})` when active, or `cito_listen({about, scope: "self"})` using the session name or working directory basename).
- `/cito on [about...]`: enable private (`self`) listening with an optional description.
- `/cito team [about...]`: enable shared (`workspace`) listening with an optional description.
- `/cito off`: stop listening for the current Pi session.

From a terminal—or inline inside Pi via `!cito listen` and `!cito unlisten`—you can also control session enrollment directly through the CLI:

```sh
cito listen [--scope self|workspace] [--team] [--hub <name>] [--tag <tag>] [--codex[=<thread_id>]] [about...]
cito unlisten [--hub <name>] [--codex[=<thread_id>]]
```

By default, `cito listen` and `cito unlisten` pass `_meta: { caller_pid: process.ppid }` so the daemon walks parent processes to locate the owning Pi or custom socket bridge. With `--codex` (or `--codex=<thread_id>` / `--codex <uuid>`), the CLI sends `_meta: { codexThreadId, cwd: process.cwd() }` instead; when `--codex` is passed without an explicit thread ID, Cito resolves the most recent matching thread from `${CITO_CODEX_HOME}/state_5.sqlite` (preferring open threads in the current `cwd`).

A gateway supporting Streamable HTTP may instead target `http://127.0.0.1:4290/mcp` after starting `cito daemon`. If the gateway runs outside Pi's ancestry, forward the Pi process PID as `_meta.endpoint_pid` (or a nested caller PID root) so discovery remains unambiguous.

The extension listens on `sockets/pi-pid-<pid>.sock` (`0600`) from `session_start` until shutdown. Discovery walks up to six processes from the OS-attributed HTTP client's PID (including the stdio forwarder's Pi parent), or uses `_meta.endpoint_pid`, `_meta.pi_pid`, or `_meta.caller_pid`. A local gateway can forward nested PID roots; explicit roots prevent fallback to an unrelated parent. The bridge returns both `endpoint_pid` and `pi_pid`. Session switches clear stale badges immediately, and a per-session 200-batch deduplication ring prevents repeated insertions on socket retries.

**Automated verification boundary:** the installed Pi 1.5.0 test loads the standalone extension through the real `loadExtensions` and `ExtensionRunner`, then observes `ExtensionRunner.bindCore`'s `sendMessage` handler. It verifies `{customType: "cito-notification", content, display: true}` with `{triggerTurn: true, deliverAs: "followUp"}` and suppression of duplicate batches. Interactive TUI transcript insertion and turn execution remain operator checks; the fixture does not perform or prove those later effects.

## ChatGPT / Codex Desktop (`codex`)

Start `cito daemon`, then configure direct Streamable HTTP in `~/.codex/config.toml` (no Cito child process per tab):

```toml
[mcp_servers.cito]
url = "http://127.0.0.1:4290/mcp"
```

Alternatively configure the stdio auto-forwarder in `~/.codex/config.toml` (choose one configuration):

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

For direct HTTP, the host may supply `x-codex-turn-metadata` as an HTTP header. The daemon merges it with `_meta` and rejects conflicting thread IDs; transport/PID metadata comes exclusively from the server's OS socket attribution, using a 1-second `lsof` lookup cached per socket. `x-codex-turn-metadata` may also be a JSON string or contain `session_id`; `openai/threadId` and `codexThreadId` are accepted top-level aliases. Multiple distinct thread claims or a thread claim combined with a top-level bridge PID return `INVALID_INPUT`. Cito never guesses a thread from recent activity or window focus.

Call `cito_listen({about: "Review service changes"})` from the conversation. Enrollment optionally reads that exact thread from `CITO_CODEX_HOME/state_5.sqlite`. Archived threads fail enrollment; a missing/unreadable database or missing row/schema falls back to `_meta.cwd`, the HTTP client's OS working directory, retained session cwd, then the daemon's cwd, with Git metadata enrichment. Cito never probes Desktop's IPC socket or requires a thread to be focused. Liveness checks whether ChatGPT/Codex Desktop is running; archived threads close definitively. On notification, the driver runs:

1. `codex queue --resume --thread <id> --message <card>` with `CODEX_HOME` set to the configured home.
2. `/usr/bin/open -g codex://threads/wake/<id>` to trigger processing.

A clean queue exit plus successful wake confirms delivery and auto-acknowledges the batch. A SIGTERM queue timeout has an unknown insertion result: Cito records the queue milestone and proceeds to the wake URL without repeating queue insertion. Failed wake attempts or uncertain insertion produce `delivered_unconfirmed`; these signals stay unread in `cito_inbox()`. Exhausted queue failures stay readable with `held_reason: "driver_unavailable"`.

`cito_inbox()` drains up to 50 unread signals by default. Use `cito_inbox({ack_all: false})` to peek. Inbox and unlisten support archived or missing threads via retained local session metadata. The native HUD opens `codex://threads/<id>` when clicked; it never reads or writes SQLite.

## macOS Menu Bar Extra (`cito-ui`)

The daemon compiles `src/macos/cito-ui.swift` on demand into `${CITO_STATE_DIR}/bin/cito-ui`, then keeps one companion connected over UTF-8 newline-delimited JSON pipes. The menu shows ACTIVE listeners with a matching live receiver in a 380pt Control-Center custom AppKit card panel hosted in `NSMenuItem.view` (360×58pt or 360×72pt listener cards with harness badge, title, optional `about` subtitle, abbreviated cwd/branch, and middle-truncated `slug#fp4` copy pill). Copy stores the full scoped ID and displays green `✓ Copied` feedback for 1.2 seconds without closing the menu. Inline `Self` / `Team`, open-in-Codex and stop buttons replace submenus. `Stop All` revokes all local listeners; `Quit Cito ⌘Q` stops the daemon. `makeCitoStatusIcon` draws an 18×18pt monochrome template vector with a C-resonator, core node, outbound wave, and warning cutout dot, followed by a monospaced unique-session count when nonzero.

Opening the menu (`NSMenuDelegate.menuWillOpen`) or changing `${CITO_STATE_DIR}/sockets` triggers on-demand discovery of local unenrolled sessions (`AVAILABLE SESSIONS`), combining live socket bridges (`pi`, custom harnesses) ordered by interaction recency (`updated_at_ms`) and recent open user threads in `${CITO_CODEX_HOME}/state_5.sqlite` when ChatGPT/Codex Desktop is running (filtering out archived and subagent threads, up to 8 candidates per harness). Candidates are grouped into collapsible per-harness sections (`Pi · N open`, `Codex · M recent`) with icon-free rows, alongside a collapsible `MY OTHER HOSTS` section for the current user's remote sessions and a `Search Network Directory…` launcher that opens a 560×440pt Spotlight-style `KeyPanel` backed by the daemon's `HubDirectoryIndex`. Clicking `+ Listen` on any candidate row enrolls that session in 1 click with `scope: "self"` and identity fencing (`expected_harness` / `expected_session_id`) so a session switch between discovery and click fails safely with `SESSION_NOT_OPEN`. When active listeners or available candidates exceed 4 items, the menu caps their viewports inside top-down `NSScrollView` containers (`active_scroll` at 260pt and `available_scroll` / `available_scroll:<harness>` at 144pt). Incoming state frames are delivered in `RunLoop.Mode.eventTracking` and guarded by an `Equatable` state diff (`hasRenderedState` / `pendingMutation`) so open menus update in place without flicker.

Interactive semantic-color hub pills show status and user. Click a pill to expand its action drawer; authentication errors and disconnected hubs expand automatically. Drawers offer Sign In (OIDC), Reconnect and Disconnect. Disconnect aborts pending login, closes NATS and receivers, clears their badges, and retains ACTIVE SQLite registrations for reconnect. Foreground tools cannot silently reconnect a manually disconnected hub. Reconnect uses cached/refreshable credentials; Login permits interactive PKCE. Account changes fence the previous principal's ACTIVE/REBINDING listeners before restoration.

Unlisten cancels locally and makes pending signals passive immediately, then updates NATS best-effort. Scope changes update NATS KV first without changing listener ID, owner or generation; receiver renewal and restoration reconcile SQLite to the matching remote scope. Quit closes receivers, hub connections and companion stdin; the companion exits on EOF. If the companion crashes, the daemon respawns it after 500ms and replays the latest state.

Notifications use the companion's **in-process** 388×136pt `.vibrantDark` top-right nonactivating HUD panel. It hides after 6 seconds without terminating the menu bar process. `CITO_UI_HEADLESS=1` (or `CITO_BANNER_HEADLESS=1`) suppresses all windows/status items while exercising the compiled protocol, actions and timers; `CITO_BANNER_DISMISS_MS` overrides the timer for tests. Headless diagnostics use stderr; stdout contains actions only.

## Multi-Hub & standard vendor-neutral OIDC configuration

Create `~/.cito/config.json` (or set `CITO_CONFIG_PATH`) to connect to multiple hubs simultaneously:

```json
{
  "daemonPort": 4290,
  "default_hub": "personal",
  "hubs": {
    "personal": {
      "natsUrl": "nats://personal-tailnet-host:4222",
      "authMode": "none",
      "user": "alice"
    },
    "work": {
      "natsUrl": "wss://nats.example.com/ws",
      "authMode": "oidc",
      "oidcIssuer": "https://id.example.com",
      "oidcClientId": "cito",
      "oidcRedirectPort": 4291,
      "oidcScopes": ["openid", "profile", "email", "offline_access"]
    }
  }
}
```

Hub names match `^[a-z0-9][a-z0-9_-]{0,31}$`. Each entry defaults to `authMode: "none"` and the environment/OS username. OIDC entries require issuer and client ID. The selected default is `CITO_HUB` (trimmed), then `default_hub`, then the sole configured hub. Multiple hubs require an explicit default. A specified missing or invalid config file fails startup. Without a file, `CITO_*` environment variables synthesize the single hub `default`.

Each hub has its own connection, identity, OIDC cache (`oidc-token-<hub>.json` only), optional scopes/redirect port, and receivers. All hubs share the host ID and one local SQLite database. Login uses an in-memory mutex, serializing hubs sharing a fixed redirect port. `oidcRedirectPort` (alias `redirect_port`) binds the callback to `127.0.0.1:<port>`; omit it for an ephemeral port. Ports must be integers in 1..65535. Providers may require registering the exact `http://127.0.0.1:<port>/callback` URI.

OIDC uses standard discovery, S256 PKCE, signature verification and silent refresh with any conforming provider (e.g. Okta, Auth0, Keycloak, Entra ID, Google or another conforming provider). When `oidcScopes` is omitted, default `openid profile email offline_access` scopes are filtered against advertised `scopes_supported`; an advertised list without `openid` fails with `AUTH_UNAVAILABLE`. Explicit scopes are sent unchanged. No provider-specific API or account ID is required. No secondary OIDC login is triggered by offline inbox reads: unauthenticated secondary hubs are skipped, while targeting one explicitly returns `UNAUTHORIZED`.

IDs returned to the agent use `<hub>:<id>` whenever multiple hubs or a named single hub are configured. Only a single hub literally named `default` returns unprefixed IDs for compatibility. Both input styles work; bare input IDs select the default hub unless `hub` is supplied. Prefixes and explicit `hub` must agree. Unknown/conflicting hubs are rejected before network or database activity. NATS payloads and SQLite ID columns always remain unprefixed.

```js
cito_listen({about: "Personal build", hub: "personal"})
cito_listen({about: "Corporate review", hub: "work"})
cito_find()                          // parallel query of every configured hub
cito_find({hub: "work", harness: "pi"})
cito_send({to: "work:review-pbtjxcxkgw3y", message: "Review ready"})
// Equivalent addressing:
cito_send({hub: "work", to: "review-pbtjxcxkgw3y", message: "Review ready"})
cito_inbox()                         // drain all authorized local hub inboxes
cito_inbox({hub: "work", ack_all: false}) // peek at one hub
cito_inbox({ack_ids: ["work:sig_..."]}) // acknowledge only these, without default draining
cito_unlisten({hub: "personal"})
cito_unlisten()                      // revoke all active hubs for this session
```

`in_reply_to` accepts the same optional prefix as signal IDs, and must refer to the send's hub. Scoped `ack_ids` must lie in the selected authorized inbox; bare signal IDs matching more than one hub return `INVALID_INPUT` without acknowledging anything. Inbox output sorts by creation time, hub, sequence, and signal ID. `cito_find` merges by descending revision, then hub and canonical ID, with a default limit of 20 and maximum 50. Find and unlisten include `warnings: [{hub, code, message}]` on partial hub failure. Find throws when all hubs fail. Unlisten commits local cancellation even when remote KV updates fail; stale remote leases expire without renewal.

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
- **Local schema reset:** `PRAGMA user_version = 4` uses `local_listeners`, `local_signals`, and `local_sent_idempotency`, with hub-qualified primary keys, plus the singleton `daemon_owner` metadata table. Opening an older local schema atomically drops its local registrations, inbox, idempotency data, and legacy pairwise table. Re-enroll after upgrading. Broker data is not reset by this operation.
- KV CAS, generations, owner tokens and daemon-local keyed mutexes fence concurrent operations. Daemon startup restores matching persisted active registrations and replay cursors without interactive login. The singleton owns all in-memory receivers.
- Healthy listeners renew every 20 seconds. Find hides leases older than 60 seconds; sends buffer through 30 minutes of disconnection. Reattachment records detected replay gaps until an `ack_all` drain.
- Admission persists the signal and replay cursor atomically. Duplicate `(hub, signal_id)` does not redeliver. Stale generations remain passive. Unauthorized private messages advance the cursor and drop silently.
- Admitted signals debounce for 1500ms into immutable, persisted batches with hub-qualified batch IDs. New batches contain at most 50 signals and 256 KiB of JSON-serialized card text; remaining signals dispatch immediately after each batch. A single oversized card stays passive in the inbox with `driver_unavailable`. Recovered batches retain their original frozen card. Every external effect rechecks ownership under its keyed listener mutex. Acknowledging before dispatch suppresses that effect; cancellation stops retries and subsequent wake effects.
- Confirmed delivery sets `DISPATCHED` and auto-acknowledges; unconfirmed delivery sets `DISPATCHED` and remains unread. Unavailable drivers retain passive inbox signals. Session switches revoke the old conversation's bindings on all hubs.
- Pi deduplicates frozen batches. Codex queue milestones survive restart. A crash between a Desktop effect and its local commit can duplicate a card; the persisted batch ID identifies it.
- Flush/publish/ack waits share a 10-second deadline and are interrupted by shutdown. Network acceptance does not imply confirmed insertion into a conversation.

### Wake cards and native HUD

```text
🔔 Cito · alice@mac [↩ work:review-pbtjxcxkgw3y]

Review is ready.
```

Cards escape angle brackets, strip `.local` from hosts, omit duplicate sender titles, and include scoped reply IDs. Batches use the English header `🔔 Cito · N notifications → Target` and numbered senders. There is no acknowledgement footer. Pi's self-contained renderer adds the host theme's background, header accent, and left body bar.

Pi's badge aggregates all active hubs for its session (`🔔 <slug>` or `🔔 <slug> (N)` when unread notifications exist, while `replay_gap_detected` and `replay_gap_range` are surfaced via `cito_inbox`). Auto-acks, inbox commits, and enrollment/revocation refresh it under a session-wide lock. Only ACTIVE rows with a live, non-aborted receiver matching `owner_token` contribute to badges and menu state. Disconnecting one hub preserves other live hubs' badges.

The macOS HUD uses a dark 388×136pt nonactivating panel. It compiles on demand and has **no SQLite dependency**. The state pill reads `Notification` or `Passive`, with ` · ×N` for batches; passive subtitles start with `[Passive] `. The Codex action reads `Open in Codex ↗`; other harnesses read `<harness> session · Click to dismiss` and dismiss on click. Counts aggregate for the exact target/session/wake tuple. Headless diagnostics report the same presentation fields used by the UI.

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

For non-socket drivers, implement `HarnessAdapter` from `src/adapters/types.ts`, then call `registerAdapter(customAdapter)` from `src/adapters/index.ts` in your in-process entrypoint. Implement `probe`, `deliver`, and optional `updateBadge`; delivery returns `delivered`, `delivered_unconfirmed`, `driver_unavailable`, or `session_switched`. Use `ctx.runEffect` for each external effect so retries preserve cancellation and ownership fencing. Only return `delivered` after confirmed host insertion. Use `CitoService.create(config?, options?)`, `service.call(name, args, meta)`, and `await service.close()`; `createMcpServer(service)` supplies SDK handlers and `startHttpServer(config, service)` exposes them over loopback HTTP. An optional `detectExplicitCaller(ctx, config)` hook can resolve non-socket identity using generic context; multiple explicit adapter claims or a concurrent bridge PID claim return `INVALID_INPUT`. Socket-backed harnesses use the existing identity discovery protocol.

### Claude Code (Proposed Design Suggestion Only — Not Yet Implemented)

**Claude Code integration is a Proposed Design Suggestion Only — Not Yet Implemented.** See [DESIGN.md §6](docs/DESIGN.md#6-proposed-design-for-claude-code-integration-design-suggestion-only-not-yet-implemented) for the architectural design suggestion. No Claude Code adapter registration, CLI flag, or UI code is implemented in `src/`.

## Environment

| Variable | Default / requirement |
| --- | --- |
| `CITO_CONFIG_PATH` | Optional explicit hub config JSON; missing/invalid paths fail startup. |
| `CITO_HUB` | Override the default hub from the file. |
| `CITO_NATS_URL` | `nats://127.0.0.1:4222`; `nats://`, `ws://`, `wss://` supported. Also overrides the selected file hub unless `CITO_CONFIG_PATH` is set. |
| `CITO_AUTH_MODE` | Single-default-hub fallback: `none` (default) or `oidc`. |
| `CITO_USER` | OS username; 1–64 letters/digits/dots/underscores/hyphens. |
| `CITO_OIDC_ISSUER` / `CITO_OIDC_CLIENT_ID` | Single-hub OIDC issuer and client/audience. HTTPS required except loopback tests. |
| `CITO_OIDC_SCOPES` | Single-hub fallback explicit space-separated scopes; omitted defaults negotiate against discovery. |
| `CITO_OIDC_REDIRECT_PORT` | Optional single-hub fixed callback port, 1..65535; omitted selects ephemeral. |
| `CITO_DAEMON_PORT` | `4290`, or config `daemonPort`; 1..65535, loopback only. |
| `CITO_STATE_DIR` | `~/.cito`; share between bridge and MCP. |
| `CITO_HOST_NAME` | OS hostname, at most 128 printable ASCII characters. |
| `CITO_CODEX_HOME` | `~/.codex`; reads only explicitly identified threads. |
| `CITO_CODEX_BIN` | `/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex`. |
| `CITO_OPEN_BIN` | `/usr/bin/open`; configurable Codex wake executable. |
| `CITO_AUTH_CALLOUT_ISSUER_SEED` | Responder only: required `SA...` Account signing seed. |
| `CITO_AUTH_CALLOUT_PASSWORD` | Responder/bootstrap only: exempt service password. |
| `CITO_AUTH_CALLOUT_XKEY_SEED` | Optional responder `SX...`, paired with the broker's public XKey. |

OIDC verifies signatures before caching tokens and the offline principal (`0600`). Online connections verify cached tokens again. Per-hub rejected-token recovery rechecks under its in-memory mutex, refreshes silently, and falls back to S256 loopback PKCE only for missing refresh credentials or HTTP 400/401. Refresh network/5xx failures return `AUTH_UNAVAILABLE` without opening a browser. Lock wait and login share a 45-second deadline. Broker reconnect authorization failures defer login until the next online tool call.

Cito installs no LaunchAgent automatically and uses NATS for cross-host delivery. The local HTTP endpoint exposes MCP only; keep it on loopback. See [DESIGN.md](docs/DESIGN.md) for the V8 architecture specification and the unimplemented Claude Code design suggestion.
