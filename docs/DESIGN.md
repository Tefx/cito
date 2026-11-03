# Cito Architecture & Protocol Specification (V1)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a notification-only signaling service that wakes or notifies opted-in coding agent conversations across:
1. **Users** (e.g., `tefx`, colleague `mingchen`),
2. **Hosts / Machines** (e.g., `mbp-work`, `mbp-personal`),
3. **Agent Harnesses** (V1: **ChatGPT / Codex Desktop App** and **Pi Coding Agent**), and
4. **Concurrent Conversations** within the same repository or working directory.

Cito follows a **"Tap on the Shoulder"** model: it transports only lightweight structured wake signals (`signal_id`, `from`, `to`, `message`, `wake`, `in_reply_to`). It never ingests, proxies, or stores business payloads from Microsoft Teams, Outlook, GitHub, or Jira. Once woken, the target agent uses its own configured tools to read external state.

### 1.2 V1 Implementation Scope vs. Deferred Scope
- **In Scope for V1 Implementation & Verification (inside `/Users/tefx/Projects/cito`)**:
  1. `cito-hub`: HTTP + WebSocket relay server with SQLite persistence, lease management, wake-quota/circuit-breaker enforcement, and optional standard OIDC JWT verification (compatible with GCP Casdoor).
  2. `cito-mcp`: Unified stdio MCP server managing stateless HTTP queries (`cito_find`, `cito_send`), lazy outbound WebSocket streaming (`cito_listen`, `cito_unlisten`), reconnect reconciliation, durable local receipt/inbox state (`cito_inbox`), and Just-in-Time (JIT) OIDC browser login.
  3. **ChatGPT / Codex Desktop App (`ChatGPT.app`) Adapter**: Deterministic caller binding via `_meta["x-codex-turn-metadata"].thread_id`, tab liveness verification via `~/.codex/ipc/ipc.sock` (`thread-owner-discovery`), queue injection via `codex queue`, and non-focus-stealing wake via `open -g "codex://threads/wake/<thread_id>"`.
  4. **Pi Coding Agent (`1.5.0`) Adapter**: Self-contained local Unix socket bridge extension (`src/pi-extension/cito-bridge.ts`) listening on `~/.cito/sockets/pi-pid-<pid>.sock` and invoking `pi.sendMessage(..., { triggerTurn, deliverAs: "followUp" })`. V1 delivers and tests the extension inside this repository (`src/pi-extension/cito-bridge.ts`) with installation instructions; it does not mutate external dotfiles repositories during tests.
  5. **`cito-banner`**: Zero-dependency macOS native `NSPanel` HUD banner (`src/macos/cito-banner.swift`) compiled via `/usr/bin/swiftc`, with lock-synchronized single-instance Unix socket coalescing (`~/.cito/sockets/banner.sock`) and click-to-jump (`codex://threads/<thread_id>`) for Codex Desktop notifications.
- **Deferred Scope (TODO — Excluded from V1 Implementation & Vectl Plan)**:
  - **Claude Code Adapter**: The target development environment does not have Claude Code installed. The Claude Code integration is documented in **Section 9** strictly as a **Proposed Design (Unverified in Target Runtime / TODO)** for future contributors. V1 `cito-hub` rejects `harness: "claude"` in `/v1/listen` with `422 UNSUPPORTED_HARNESS`, and `cito_find` only accepts `harness: "codex" | "pi"`.

### 1.3 Non-Goals
- **No Host Background Daemon (`launchd`)**: No persistent background daemon is installed on agent hosts outside the agent harnesses and their MCP subprocesses.
- **No Standalone Business CLI**: All agent-facing operations are exposed as MCP tools through `cito-mcp`.
- **No Implicit Session Scanning**: Cito never scans `~/.codex/state_5.sqlite` or `~/.pi/agent/sessions/` to advertise un-enrolled conversations. Sessions are invisible and unreachable until they explicitly call `cito_listen`.
- **No Telegram Side-Channel**: Human visibility is provided strictly via in-chat message cards, Pi status bar indicators, and the local macOS `cito-banner` HUD.
- **No Click-to-Jump for Pi Banners**: Clicking a `cito-banner` notification jumps to the target conversation window only for Codex Desktop App (`codex://threads/<thread_id>`); Pi notifications display the banner and dismiss on click without window switching.

---

## 2. System Topology, Configuration, and Module Boundaries

```text
┌────────────────────────────────────────────────────────────────────────────┐
│ Agent Host (macOS — Zero launchd daemons)                                  │
│                                                                            │
│  ┌──────────────────────┐        ┌──────────────────────────────────────┐  │
│  │ Pi Coding Agent      │        │ ChatGPT / Codex Desktop App          │  │
│  │  ├─ cito-bridge.ts   │        │  ├─ Renderer Tabs (thread_id)        │  │
│  │  │  (pi-pid-<P>.sock)│        │  ├─ ~/.codex/ipc/ipc.sock            │  │
│  │  └─ MCP Client ──────┼───────►│  └─ codex app-server (MCP Client)    │  │
│  └──────────▲───────────┘        └──────────────────┬───────────────────┘  │
│             │                                       │                      │
│             │     stdio MCP (direct or via proxy)   ▼                      │
│             │    ┌──────────────────────────────────────────────────┐      │
│             ├────┤ cito-mcp (Unified MCP Server)                    │      │
│             │    │  ├─ Stateless HTTP/HTTPS Client (find / send)    │      │
│             │    │  ├─ Lazy WS/WSS Stream Client (active listeners) │      │
│             │    │  ├─ Local State & Inbox (~/.cito/state.sqlite)   │      │
│             │    │  └─ OIDC PKCE JIT Login (~/.cito/oidc-token.json)│      │
│             │    └────────────────────────┬─────────────────────────┘      │
│             │                             │                                │
│             │                             ▼ ~/.cito/sockets/banner.sock    │
│             │                    ┌──────────────────┐                      │
│             └────────────────────┤ cito-banner      │                      │
│              wake / queue        │ (On-demand HUD)  │                      │
│                                  └──────────────────┘                      │
└───────────────────────────────────────────┼────────────────────────────────┘
                                            │ Outbound HTTP(S) / WS(S)
                                            ▼
                           ┌──────────────────────────────────┐
                           │ cito-hub (Shared Relay Service)  │
                           │  ├─ OIDC Verifier (JWKS)         │
                           │  ├─ Listener Registry & Leases   │
                           │  ├─ Wake Quota & Circuit Breaker │
                           │  └─ Replay Store (SQLite)        │
                           └──────────────────────────────────┘
```

### 2.1 Environment Variables & Configuration Contract
Invalid configuration values cause the respective process (`cito-hub` or `cito-mcp`) to fail fast at startup with exit code `1` and a diagnostic on `stderr`.

| Variable | Process | Required / Default | Validation & Semantics |
| :--- | :--- | :--- | :--- |
| `CITO_HUB_BIND` | `cito-hub` | Optional, default `"127.0.0.1:8780"` | `<host>:<port>` bind address. TLS termination (`https://` / `wss://`) is handled by an upstream reverse proxy (such as Caddy or Tailscale Serve) or local loopback HTTP/WS for local testing. |
| `CITO_HUB_DB_PATH` | `cito-hub` | Optional, default `"~/.cito/hub.sqlite"` | Path to Hub SQLite database file (created with mode `0600` in directory `0700`). Persists `hub_secret` (32 random bytes generated on initial DB creation) in `hub_meta` so `listener_id` derivation survives Hub restarts. |
| `CITO_AUTH_MODE` | `cito-hub` | Optional, default `"none"` | Enum: `"none"` \| `"oidc"`. |
| `CITO_OIDC_ISSUER` | `cito-hub`, `cito-mcp` | Required if `CITO_AUTH_MODE="oidc"`, else optional | HTTPS URL (or `http://127.0.0.1:<port>` in tests) of the OIDC Provider (e.g., `"https://auth.internal.rda.ai"`). |
| `CITO_OIDC_CLIENT_ID` | `cito-hub`, `cito-mcp` | Required if `CITO_OIDC_ISSUER` is set | Single OIDC Client ID string registered at the issuer (used as required JWT `aud` claim on Hub and as `client_id` during PKCE login on `cito-mcp`). |
| `CITO_HUB_URL` | `cito-mcp` | Optional, default `"http://127.0.0.1:8780"` | Base HTTP(S) URL of `cito-hub`. WebSocket stream URL is derived by replacing `http(s):` with `ws(s):` and appending `/v1/stream`. |
| `CITO_STATE_DIR` | `cito-mcp`, `cito-bridge.ts`, `cito-banner` | Optional, default `"~/.cito"` | Base local state directory (`0700`), containing `host-id`, `state.sqlite`, `oidc-token.json`, `oidc-login.lock`, `bin/cito-banner`, and `sockets/`. |
| `CITO_USER` | `cito-mcp` | Optional, default `os.userInfo().username` | Used only when Hub runs with `CITO_AUTH_MODE="none"`. Sanitized to `^[a-z0-9._-]{1,64}$`. |
| `CITO_HOST_NAME` | `cito-mcp` | Optional, default `os.hostname()` | Human-readable machine label (truncated to 128 ASCII chars). |
| `CITO_CODEX_BIN` | `cito-mcp` | Optional, default `"/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"` | Path to the Codex Desktop CLI binary used for `codex queue`. |
| `CITO_CODEX_HOME` | `cito-mcp` | Optional, default `"~/.codex"` | Directory containing `ipc/ipc.sock` and `state_5.sqlite`. |

---

## 3. Identity, Addressing, and Caller Resolution

### 3.1 Canonical Listener Identity & Upsert Key
Every listener registration is uniquely keyed at `cito-hub` by the 4-tuple:

$$\text{SessionKey} = (\text{principal\_id}, \text{host\_id}, \text{harness}, \text{session\_id})$$

1. **Single Canonical `principal_id` and `owner_user` Algorithm**:
   - **When `CITO_AUTH_MODE="oidc"`**: `cito-hub` validates the Bearer JWT (Section 8.1), sets immutable authorization identity `principal_id = "${claims.iss}#${claims.sub}"`, and derives the human-readable handle `owner_user` using the single deterministic rule:
     1. `claims.preferred_username` (if non-empty string), else
     2. local-part of `claims.email` before `@` (if non-empty string containing `@`), else
     3. `claims.sub`.
     The selected string is lowercased and non-`[a-z0-9._-]` characters are replaced with `_` (max 64 chars). All authenticated users under the same `CITO_OIDC_ISSUER` belong to the same Hub workspace.
   - **When `CITO_AUTH_MODE="none"`**: `cito-mcp` sends `user` in the HTTP/WSS request payload (`CITO_USER` or OS username, lowercased and validated against `^[a-z0-9._-]{1,64}$`). `principal_id` equals `"none#" + user`, and `owner_user` equals `user`. All callers on the Hub share one workspace.
2. **`host_id`**:
   - On first run, `cito-mcp` generates a lowercase UUIDv4 and writes it atomically to `${CITO_STATE_DIR}/host-id` (`0600`). Subsequent runs read this file so machine hostname changes never alter `host_id`.
3. **`listener_id` Grammar & Client Mapping Persistence**:
   - Format: `^c_[a-z2-7]{12}$` (prefix `c_` followed by 12 lowercase RFC 4648 base32 characters `[a-z2-7]`, e.g., `c_ab2def3gh4jk`).
   - Allocated deterministically by `cito-hub` as `HMAC-SHA256(hub_secret, "${principal_id}|${host_id}|${harness}|${session_id}")` where `hub_secret` is persisted in `hub.sqlite`.
   - `cito-hub` returns `listener_id` in the `POST /v1/listen` response, and `cito-mcp` persists the mapping `(harness, session_id) -> listener_id` in its local `local_listeners` table (`~/.cito/state.sqlite`). Even after `cito_unlisten` sets `local_listeners.status = 'UNLISTENED'`, the `(harness, session_id) -> listener_id` row is retained locally so `cito_inbox` can look up `listener_id` directly from SQLite without needing `hub_secret` or an active Hub connection.

### 3.2 Target-Driven Routing vs. Receiver-Only Session Binding
Cito separates **outbound operations (`cito_find`, `cito_send`)** from **receiver-side operations (`cito_listen`, `cito_unlisten`, `cito_inbox`)**:

1. **Outbound (`cito_find`, `cito_send`) Requires NO Caller Harness/Session Detection**:
   - Routing and wake dispatch depend **exclusively on the target's registered identity** (`to = listener_id`), which already records the target's `(harness, session_id, host_id)`.
   - Therefore, `cito_find` and `cito_send` never require the sender to resolve a local harness or `session_id`, and **never fail with `BINDING_REQUIRED`**.
   - **Canonical `SenderEndpointKey`** (used consistently across self-send checks, idempotency scoping, and pairwise circuit-breaking):
     - If the caller's `(harness, session_id)` is resolved: `SenderEndpointKey = "${principal_id}|${host_id}|${harness}|${session_id}"`.
     - If the caller is unbound (`harness = null, session_id = null`): `SenderEndpointKey = "${principal_id}|${host_id}|unbound|default"`. (A bound call and an unbound call have distinct `SenderEndpointKey`s and therefore distinct idempotency/pairwise namespaces).
   - When the recipient's `cito-mcp` receives a `signal` frame from `cito-hub`, it dispatches the wake driver strictly from the target registration's recorded `harness` and `session_id` (`codex:<thread_id>` or `pi:<session_id>`).

2. **Receiver Binding (`cito_listen`, `cito_unlisten`, `cito_inbox`)**:
   Only when a session calls `cito_listen` (to record its own wake target in its identity), `cito_unlisten`, or `cito_inbox` does `cito-mcp` resolve the caller's own `(harness, session_id, cwd)`. If resolution fails on these three receiver tools, the tool returns an MCP error (`isError: true`, `code: "BINDING_REQUIRED"` | `"SESSION_NOT_OPEN"` | `"PI_BRIDGE_UNAVAILABLE"`) without guessing from window focus or recent timestamps:

   - **Path A — Codex Desktop App (`harness = "codex"`)**:
     - Inspects `request.params._meta["x-codex-turn-metadata"]` (parsed as JSON if string) or `_meta["openai/threadId"]` / `_meta["codexThreadId"]`.
     - If a valid UUID `thread_id` is present:
       - Queries `${CITO_CODEX_HOME}/state_5.sqlite` (read-only, `timeout = 1000ms`):
         `SELECT cwd, git_branch, git_origin_url, COALESCE(NULLIF(name,''), title) AS title, archived FROM threads WHERE id = ?`.
       - If `archived == 1` (for `cito_listen`), fails with `SESSION_NOT_OPEN`.
       - For `cito_listen`: probes `${CITO_CODEX_HOME}/ipc/ipc.sock` with `thread-owner-discovery` (Section 7.3, `1500ms` timeout). If the Desktop App returns `no-client-found`, fails with `SESSION_NOT_OPEN`.
   - **Path B — Pi Coding Agent (`harness = "pi"`)**:
     - `cito-mcp` locates the owning Pi process using two mechanisms in order:
       1. Explicit `_meta` hint: if `request.params._meta` contains `pi_pid` or `caller_pid` matching an existing `${CITO_STATE_DIR}/sockets/pi-pid-<pid>.sock`.
       2. **OS Process-Tree Ancestor Walk (Gateway-Agnostic)**: `cito-mcp` walks up its parent PID chain (`process.ppid` $\to$ parent's `ppid` via `/bin/ps -o ppid= -p <pid>`, up to 6 levels) until it finds an ancestor `<pid>` whose socket `${CITO_STATE_DIR}/sockets/pi-pid-<pid>.sock` exists.
     - `cito-mcp` connects to `pi-pid-<pid>.sock` (`1500ms` timeout) and sends `{"id":"1","op":"identify"}\n`.
     - `cito-bridge.ts` replies with `{ "id": "1", "ok": true, "pi_pid": <pid>, "session_id": "<live_uuid>", "session_name": "...", "cwd": "...", "is_streaming": true, "human_event_seq": 2 }`.
     - If no ancestor socket exists or the socket fails to respond within `1500ms`, fails with `PI_BRIDGE_UNAVAILABLE` (or `BINDING_REQUIRED` if neither Codex metadata nor a Pi ancestor is present).

### 3.3 Repository & Metadata Normalization
- For both Codex and Pi, if `git_origin_url` or `git -C <cwd> remote get-url origin` is available, `cito-mcp` normalizes SSH (`git@github.com:Owner/Repo.git`) and HTTPS (`https://github.com/Owner/Repo.git`) URLs to `"Owner/Repo"` (stripping `.git` and trailing slashes, preserving case for display and comparing case-insensitively in `cito_find`). If no Git remote exists, `repo` is `null`.
- For Pi, `branch` is read via `git -C <cwd> rev-parse --abbrev-ref HEAD` (or `null`), and `title` is `session_name || basename(cwd)`.

---

## 4. Public MCP Tool Contracts & Error Envelope

### 4.1 Common MCP Result & Error Representation
- Every successful tool call returns `isError: false` and `content: [{ type: "text", text: JSON.stringify(payload, null, 2) }]`.
- Every failed tool call returns `isError: true` and `content: [{ type: "text", text: JSON.stringify({ ok: false, error: { code, message, retryable } }, null, 2) }]`.
- **Canonical Error Codes & HTTP Mapping**:

| Error Code | HTTP Status (if Hub-originated) | `retryable` | Trigger Condition |
| :--- | :--- | :--- | :--- |
| `BINDING_REQUIRED` | Local (`N/A`) | `false` | Receiver tool (`cito_listen`, `cito_unlisten`, `cito_inbox`) called without Codex `_meta` or a reachable Pi ancestor socket. |
| `SESSION_NOT_OPEN` | Local (`N/A`) | `false` | `cito_listen` called for a closed or archived Codex Desktop thread. |
| `PI_BRIDGE_UNAVAILABLE` | Local (`N/A`) | `true` | Pi ancestor socket exists but timed out (`1500ms`) or returned an error. |
| `INVALID_INPUT` | `422` | `false` | Schema validation failure or `message` / `about` empty after control-character stripping and trimming. |
| `UNSUPPORTED_HARNESS` | `422` | `false` | `harness` is not `"codex"` or `"pi"` (e.g., `"claude"` in V1). |
| `UNAUTHORIZED` | `401` | `true` | OIDC token missing/invalid and JIT refresh/login failed or was cancelled. |
| `OIDC_LOGIN_TIMEOUT` | Local (`N/A`) | `true` | OIDC lock wait + refresh + browser PKCE flow exceeded the 45-second deadline. |
| `AUTH_UNAVAILABLE` | `503` | `true` | Hub failed to reach OIDC discovery / `jwks_uri` during cache miss. |
| `FORBIDDEN_TARGET_SCOPE` | `403` | `false` | `sender.principal_id !== target.principal_id` and `target.scope !== "workspace"`. |
| `LISTENER_NOT_FOUND` | `404` | `false` | Unknown `listener_id` or target is in `UNLISTENED` state. |
| `LISTENER_EXPIRED` | `410` | `false` | Target listener is in `EXPIRED` state ($> 30\text{m}$ disconnected). |
| `SELF_SEND_FORBIDDEN` | `422` | `false` | `SenderEndpointKey` equals target `SessionKey`. |
| `IDEMPOTENCY_KEY_CONFLICT` | `409` | `false` | Same `(SenderEndpointKey, to, idempotency_key)` reused within 30m with different `(message, wake, in_reply_to)`. |
| `STALE_GENERATION` | `409` | `true` | Request carried `binding_generation < current_generation`. |
| `HUB_UNAVAILABLE` | `502` / `503` / Network | `true` | Network failure or timeout (`10s`) reaching `cito-hub`. |

### 4.2 `cito_listen`
Opts the calling conversation into discovery and wake notifications, or updates its existing registration.

- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "about": {
        "type": "string",
        "minLength": 1,
        "maxLength": 500,
        "description": "Concise description of this session's role, responsibility, or current task."
      },
      "scope": {
        "type": "string",
        "enum": ["self", "workspace"],
        "description": "Visibility and send permission scope. Defaults to 'self' on initial registration; if omitted on update, preserves the existing scope."
      },
      "allow_peer_wake": {
        "type": "boolean",
        "description": "When scope is 'workspace', whether signals from other users may automatically wake this session. Defaults to false on initial registration; if omitted on update, preserves the existing value."
      }
    },
    "required": ["about"],
    "additionalProperties": false
  }
  ```
- **Output Schema (All fields required, `repo` and `branch` nullable)**:
  ```json
  {
    "ok": true,
    "status": "listening",
    "listener_id": "c_ab2def3gh4jk",
    "binding_generation": 3,
    "user": "tefx",
    "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
    "host_name": "tefx-mbp-work",
    "harness": "codex",
    "session_id": "01a19c75-6a12-72d2-bbca-fca2fc745dff",
    "about": "跟进 RCATwin Issue #3 三人 Teams 群复测与手册 PR",
    "repo": "cap-dcwiz/RCATwin",
    "branch": "keppel-delivery",
    "title": "审查最新的keppel-delivery和platform分支",
    "scope": "self",
    "allow_peer_wake": false,
    "unattended_wakes_remaining": 3,
    "lease_ttl_seconds": 60
  }
  ```

### 4.3 `cito_unlisten`
Revokes the calling conversation's listener registration and stops receiving notifications.

- **Input Schema**: `{"type": "object", "properties": {}, "additionalProperties": false}`
- **Behavior**:
  - If the caller session has no active registration in `local_listeners` (or is already `'UNLISTENED'`), returns `{ "ok": true, "status": "not_listening", "listener_id": null }`.
  - If active: sends `POST /v1/unlisten` to Hub, cancels any pending debounce or retry timer, atomically transitions any undispached `state IN ('RECEIVED', 'DISPATCHING')` signals for that session in `local_signals` to `state = 'STORED_PASSIVE', held_reason = 'unlistened_before_dispatch'` (so neither initial dispatch nor any in-flight retry can wake the session after revocation, while keeping all signals readable and acknowledgeable via `cito_inbox` until expiry), updates `local_listeners.status = 'UNLISTENED'` (preserving the `(harness, session_id) -> listener_id` mapping row), and (for Pi) clears the `cito` status bar badge.
- **Output Schema**:
  ```json
  {
    "ok": true,
    "status": "unlistened",
    "listener_id": "c_ab2def3gh4jk"
  }
  ```

### 4.4 `cito_find`
Searches active listeners (`state == "ACTIVE"`, `lease_expires_at > now`).

- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "maxLength": 200,
        "description": "Whitespace-separated keywords; every keyword must match (case-insensitive substring) across concatenated [about, repo, branch, title, user, host_name]."
      },
      "user": { "type": "string", "maxLength": 64, "description": "Exact case-insensitive match on owner_user." },
      "host": { "type": "string", "maxLength": 128, "description": "Matches exact host_id or case-insensitive substring of host_name." },
      "harness": { "type": "string", "enum": ["codex", "pi"] },
      "repo": { "type": "string", "maxLength": 200, "description": "Case-insensitive substring or exact owner/repo match." },
      "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 20 }
    },
    "additionalProperties": false
  }
  ```
- **Authorization & Visibility Predicate at Hub**:
  - Only listeners with `state == "ACTIVE"` and `lease_expires_at > now` are searched.
  - A listener with `scope == "self"` is included **if and only if** `caller.principal_id === listener.principal_id`.
  - A listener with `scope == "workspace"` is included for any authenticated caller in the Hub workspace.
  - Results are ordered deterministically by `last_seen_at DESC, listener_id ASC`.
- **Output Schema (`caller.harness`, `caller.session_id`, and `caller.listener_id` are `string | null` — `null` when caller is unbound)**:
  ```json
  {
    "ok": true,
    "caller": {
      "user": "tefx",
      "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
      "host_name": "tefx-mbp-work",
      "harness": "pi",
      "session_id": "01a1a1ef-1c42-77ea-b86f-2e0e3734eb24",
      "listener_id": null
    },
    "listeners": [
      {
        "listener_id": "c_ab2def3gh4jk",
        "user": "tefx",
        "host_id": "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f",
        "host_name": "tefx-mbp-personal",
        "harness": "codex",
        "about": "跟进 RCATwin Issue #3 三人 Teams 群复测与手册 PR",
        "repo": "cap-dcwiz/RCATwin",
        "branch": "keppel-delivery",
        "title": "审查最新的keppel-delivery和platform分支",
        "scope": "self",
        "allow_peer_wake": false,
        "unattended_wakes_remaining": 3,
        "last_seen_at": "2026-11-03T18:30:00Z"
      }
    ]
  }
  ```

### 4.5 `cito_send`
Sends a notification signal to an opted-in listener.

- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "to": {
        "type": "string",
        "pattern": "^c_[a-z2-7]{12}$",
        "description": "Exact target listener_id returned by cito_find or cito_listen."
      },
      "message": {
        "type": "string",
        "minLength": 1,
        "maxLength": 500,
        "description": "Concise notification text explaining why the target is being notified."
      },
      "wake": {
        "type": "boolean",
        "default": true,
        "description": "True to wake the target session into a new turn; false for passive notifications."
      },
      "in_reply_to": {
        "type": "string",
        "maxLength": 64,
        "description": "Optional signal_id this notification is replying to (informational context; unknown/expired IDs are accepted as opaque strings)."
      },
      "idempotency_key": {
        "type": "string",
        "minLength": 1,
        "maxLength": 128,
        "description": "Optional caller-supplied idempotency key (scoped per SenderEndpointKey + target listener_id for 30 minutes)."
      }
    },
    "required": ["to", "message"],
    "additionalProperties": false
  }
  ```
- **Sanitization & Validation Order**:
  1. Strip ASCII control characters (`0x00–0x08`, `0x0B`, `0x0C`, `0x0E–0x1F`, `0x7F`) from `message` and `about`, preserving `\n` (`0x0A`) and `\t` (`0x09`).
  2. Trim leading/trailing whitespace. If the resulting string is empty or exceeds 500 Unicode code points, reject with `422 INVALID_INPUT` **before** storing or sending.
  3. Escape `<` as `&lt;` and `>` as `&gt;` when rendering into wake cards.
- **Idempotency & Milestone Semantics**:
  - Keyed by `(SenderEndpointKey, target_listener_id, idempotency_key)` for 30 minutes.
  - If an identical key is submitted with the exact same `(message, wake, in_reply_to)`, Hub returns `200 OK` with the existing `signal_id` and its **current** delivery `status` without re-charging wake quota or creating a second signal.
  - If an identical key is submitted with different `(message, wake, in_reply_to)`, Hub rejects with `409 IDEMPOTENCY_KEY_CONFLICT`.
  - **Delivery Milestones (`status`)**:
    - `"wake_accepted"`: `effective_wake == true`, and the recipient's `cito-mcp` persisted the signal to `~/.cito/state.sqlite` and returned a WSS `receipt` frame within `2000ms` of `POST /v1/send`. (Proves durable local receipt and scheduling for Stage-2 wake dispatch; does not claim the LLM turn has completed).
    - `"queued_for_reconnect"`: `effective_wake == true`, Hub durably stored the signal in `hub.sqlite`, and either the target listener is in `DISCONNECTED_RETAINED` or its WSS `receipt` did not arrive within `2000ms`. (If a `receipt` arrives after the `2000ms` HTTP response window, Hub still records it so a later idempotent retry of `POST /v1/send` returns `"wake_accepted"`).
    - `"stored_passive"`: `effective_wake == false`; `held_reason` is one of `"passive_requested"` | `"peer_wake_disabled"` | `"wake_quota_exhausted"` | `"pairwise_rate_limited"`.
- **Output Schema**:
  ```json
  {
    "ok": true,
    "signal_id": "sig_019a4b8c7d2e70008123456789abcdef",
    "seq": 42,
    "to": "c_ab2def3gh4jk",
    "requested_wake": true,
    "effective_wake": true,
    "status": "wake_accepted",
    "held_reason": null,
    "expires_at": "2026-11-03T19:00:00Z"
  }
  ```

### 4.6 `cito_inbox`
Reads and acknowledges signals stored in local SQLite (`~/.cito/state.sqlite`) for the calling conversation.

- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "ack_ids": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Specific signal_ids to mark as acknowledged."
      },
      "ack_all": {
        "type": "boolean",
        "default": false,
        "description": "When true, marks all returned unacknowledged signals for this session as acknowledged."
      }
    },
    "additionalProperties": false
  }
  ```
- **Transactional Execution Order**:
  1. Resolve caller `(harness, session_id)` (Section 3.2) and look up `listener_id` in local `local_listeners` table (`WHERE harness = ? AND session_id = ?`). If the session has never registered locally, return `{ "ok": true, "listener_id": null, "replay_gap_detected": false, "replay_gap_range": null, "signals": [], "acknowledged_count": 0 }`.
  2. Select all rows in `local_signals` where `listener_id = ? AND state IN ('RECEIVED', 'DISPATCHING', 'DISPATCHED', 'STORED_PASSIVE') AND expires_at > now` ordered by `seq ASC` (limit 50).
  3. Determine the set of IDs to acknowledge: if `ack_all == true`, all selected `signal_id`s plus any additional IDs in `ack_ids` belonging to this `listener_id`; if `ack_all == false`, only IDs in `ack_ids` belonging to this `listener_id` (unknown IDs or IDs belonging to other sessions are ignored).
  4. Update those rows to `state = 'ACKED', acked_at = now` (if any were in `'RECEIVED'`, they will no longer be selected by the Stage-2 wake dispatcher).
  5. If `replay_gap_detected == 1` on `local_listeners`, include `replay_gap_detected: true` and `replay_gap_range` in the response, and clear `replay_gap_detected = 0, replay_gap_range = NULL` when `ack_all == true`.
- **Output Schema (`from.harness`, `from.listener_id`, and `from.about` are `string | null`)**:
  ```json
  {
    "ok": true,
    "listener_id": "c_ab2def3gh4jk",
    "replay_gap_detected": false,
    "replay_gap_range": null,
    "signals": [
      {
        "signal_id": "sig_019a4b8c...",
        "seq": 42,
        "from": {
          "user": "mingchen",
          "host_name": "mc-mbp",
          "harness": "codex",
          "listener_id": "c_mn5pqr6st7uv",
          "about": "RCATwin solver"
        },
        "message": "我已在 Teams 群发出最新压测数据，请读取并继续复测",
        "effective_wake": false,
        "held_reason": "peer_wake_disabled",
        "in_reply_to": null,
        "created_at": "2026-11-03T18:31:00Z"
      }
    ],
    "acknowledged_count": 1
  }
  ```

---

## 5. Listener State Machine, Wire Protocol, and Crash-Safe Cursor

### 5.1 Listener Lifecycle State Machine at `cito-hub`
Each `listener_id` at `cito-hub` has a monotonic `binding_generation` (starts at `1`), a monotonic per-listener signal sequence `latest_seq` (starts at `0`, **never resets across generations or re-enrollments**), a persistent `unattended_wake_count` (starts at `0`), and one of four states:

| State | Entry Condition | Visibility in `cito_find` | `POST /v1/send` Behavior | Reconnect / Renewal Rule |
| :--- | :--- | :--- | :--- | :--- |
| **`ACTIVE`** | `POST /v1/listen` (`lease_expires_at = now + 60s`) | **Visible** (subject to `scope`) | Admitted; pushed over WSS (`2s` receipt wait $\to$ `wake_accepted` or `queued_for_reconnect`). | Renewed (`lease_expires_at = now + 60s`) on each valid WSS `bind` or `heartbeat` carrying matching `(lease_token, binding_generation)`. |
| **`DISCONNECTED_RETAINED`** | `lease_expires_at <= now` or WSS disconnects without `unlisten` (retained for **30 minutes** from disconnect time) | **Hidden** | Admitted and stored in Hub SQLite; returns `status: "queued_for_reconnect"` (or `"stored_passive"`). | Client resumes directly to **`ACTIVE`** via WSS `bind` using the existing `(lease_token, binding_generation)` without calling `/v1/listen`. |
| **`EXPIRED`** | In `DISCONNECTED_RETAINED` for $> 30\text{ minutes}$ | **Hidden** | Rejected with `410 LISTENER_EXPIRED`. | Requires a fresh `POST /v1/listen` (`binding_generation + 1`, new `lease_token`). |
| **`UNLISTENED`** | Explicit `POST /v1/unlisten` or WSS `unlisten` frame | **Hidden** | Rejected with `404 LISTENER_NOT_FOUND`. | `lease_token` is invalidated. Repeating `POST /v1/unlisten` with the same `binding_generation` returns `200 {"ok":true,"status":"unlistened"}` idempotently; `< current_generation` returns `409 STALE_GENERATION`. Re-enrolling requires a fresh `POST /v1/listen` (`binding_generation + 1`). |

### 5.2 Complete Hub HTTP & WSS Wire Protocol

#### HTTP Endpoints (`Content-Type: application/json`, unknown fields rejected with `422 INVALID_INPUT`)
1. **`POST /v1/listen`**:
   - Request:
     ```json
     {
       "user": "tefx",
       "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
       "host_name": "tefx-mbp-work",
       "harness": "codex",
       "session_id": "01a19c75-6a12-72d2-bbca-fca2fc745dff",
       "about": "跟进 RCATwin Issue #3 三人 Teams 群复测与手册 PR",
       "scope": "self",
       "allow_peer_wake": false,
       "repo": "cap-dcwiz/RCATwin",
       "branch": "keppel-delivery",
       "title": "审查最新的keppel-delivery和platform分支",
       "human_event_seq": 1
     }
     ```
     *Note*: `scope` and `allow_peer_wake` are optional on update (when omitted, Hub preserves existing values; on initial insert, defaults to `"self"` and `false`). `user` is required in `none` mode and ignored in `oidc` mode.
   - Response (`200 OK`):
     ```json
     {
       "ok": true,
       "status": "listening",
       "listener_id": "c_ab2def3gh4jk",
       "lease_token": "lt_019a4b...",
       "binding_generation": 3,
       "user": "tefx",
       "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
       "host_name": "tefx-mbp-work",
       "harness": "codex",
       "session_id": "01a19c75-6a12-72d2-bbca-fca2fc745dff",
       "about": "跟进 RCATwin Issue #3 三人 Teams 群复测与手册 PR",
       "repo": "cap-dcwiz/RCATwin",
       "branch": "keppel-delivery",
       "title": "审查最新的keppel-delivery和platform分支",
       "scope": "self",
       "allow_peer_wake": false,
       "unattended_wakes_remaining": 3,
       "lease_ttl_seconds": 60
     }
     ```
2. **`POST /v1/unlisten`**:
   - Request: `{ "listener_id": "c_ab2def3gh4jk", "lease_token": "lt_019a4b...", "binding_generation": 3 }`
   - Response (`200 OK`): `{ "ok": true, "status": "unlistened", "listener_id": "c_ab2def3gh4jk" }`
3. **`POST /v1/find`**:
   - Request:
     ```json
     {
       "caller": {
         "user": "tefx",
         "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
         "host_name": "tefx-mbp-work",
         "harness": "pi",
         "session_id": "01a1a1ef-1c42-77ea-b86f-2e0e3734eb24"
       },
       "query": "RCATwin",
       "user": null,
       "host": null,
       "harness": null,
       "repo": null,
       "limit": 20
     }
     ```
     *Note*: `caller.harness` and `caller.session_id` are nullable (`null` when caller is unbound).
   - Response (`200 OK`): Same shape as `cito_find` output in Section 4.4.
4. **`POST /v1/send`**:
   - Request:
     ```json
     {
       "sender": {
         "user": "tefx",
         "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
         "host_name": "tefx-mbp-work",
         "harness": "pi",
         "session_id": "01a1a1ef-1c42-77ea-b86f-2e0e3734eb24"
       },
       "to": "c_ab2def3gh4jk",
       "message": "配置已更新并推送，请拉取最新分支",
       "wake": true,
       "in_reply_to": null,
       "idempotency_key": "idem_01"
     }
     ```
     *Note*: `sender.harness` and `sender.session_id` are nullable.
   - Response (`200 OK`): Same shape as `cito_send` output in Section 4.5.

#### WSS Stream Frames (`WS(S) /v1/stream`)
- **Client $\to$ Hub Frames**:
  1. `bind`: `{ "type": "bind", "listener_id": "c_ab2def3gh4jk", "lease_token": "lt_...", "binding_generation": 3, "persisted_seq": 41 }`
  2. `heartbeat`: `{ "type": "heartbeat", "listeners": [{ "listener_id": "c_ab2def3gh4jk", "lease_token": "lt_...", "binding_generation": 3, "persisted_seq": 42, "human_event_seq": 2 }] }`
  3. `receipt`: `{ "type": "receipt", "listener_id": "c_ab2def3gh4jk", "lease_token": "lt_...", "binding_generation": 3, "signal_id": "sig_019a4b8c...", "persisted_seq": 42 }`
  4. `unlisten`: `{ "type": "unlisten", "listener_id": "c_ab2def3gh4jk", "lease_token": "lt_...", "binding_generation": 3, "reason": "tab_closed" }`
- **Hub $\to$ Client Frames**:
  1. `bound`: `{ "type": "bound", "listener_id": "c_ab2def3gh4jk", "binding_generation": 3, "replay_gap": false, "min_retained_seq": 1, "latest_seq": 42 }`
  2. `heartbeat_ack`: `{ "type": "heartbeat_ack", "renewed_listener_ids": ["c_ab2def3gh4jk"] }`
  3. `signal`:
     ```json
     {
       "type": "signal",
       "listener_id": "c_ab2def3gh4jk",
       "binding_generation": 3,
       "seq": 42,
       "signal_id": "sig_019a4b8c...",
       "from": {
         "user": "tefx",
         "host_name": "tefx-mbp-work",
         "harness": "pi",
         "listener_id": "c_mn5pqr6st7uv",
         "about": "dotfiles configuration"
       },
       "trust_tier": "self",
       "message": "配置已更新并推送，请拉取最新分支",
       "effective_wake": true,
       "held_reason": null,
       "in_reply_to": null,
       "created_at": "2026-11-03T18:31:00Z",
       "expires_at": "2026-11-03T19:01:00Z"
     }
     ```
  4. `fenced`: `{ "type": "fenced", "listener_id": "c_ab2def3gh4jk", "current_generation": 4, "code": "STALE_GENERATION", "reason": "superseded_by_newer_binding" }`
     - **Client Fencing Transition**: When `cito-mcp` receives `fenced` for `listener_id`, if `local_listeners.binding_generation <= frame.current_generation`, `cito-mcp` atomically sets `local_listeners.status = 'FENCED_INACTIVE'`, cancels any debounce or retry timer, and transitions all `state IN ('RECEIVED', 'DISPATCHING')` rows for `listener_id` with `binding_generation <= frame.current_generation` to `state = 'STORED_PASSIVE', held_reason = 'fenced_before_dispatch'`. (If another process already updated `local_listeners.binding_generation > frame.current_generation`, the stale `fenced` frame is ignored).
  5. `error`: `{ "type": "error", "code": "INVALID_FRAME", "message": "..." }`
  6. `auth_expired`: `{ "type": "auth_expired" }` followed by WebSocket close `4401`.

### 5.3 Bounded Continuous Liveness Sweep, Auto-Reconnect, and Crash-Safe Dispatch
1. **Bounded Continuous Liveness Sweep (Every 20s, Per-Listener Isolated)**:
   Before sending each 20-second `heartbeat`, `cito-mcp` probes all locally `ACTIVE` listeners concurrently via `Promise.allSettled`, with a strict **`1500ms` timeout per listener** so one hung socket never delays lease renewal for other listeners:
   - **Codex Desktop App (`harness == "codex"`)**:
     - Queries `state_5.sqlite` (`archived`) and probes `${CITO_CODEX_HOME}/ipc/ipc.sock` (`thread-owner-discovery`, Section 7.3).
     - Definitive closure (`archived == 1`, `no-client-found`, or `ChatGPT.app` process absent): immediately sends WSS `unlisten(reason: "tab_closed")` and transitions local state to `'UNLISTENED'`.
     - Transient error/timeout: increments `consecutive_probe_failures`; on the **3rd** consecutive failure (`60s`), revokes locally and sends `unlisten(reason: "probe_timeout")`.
   - **Pi (`harness == "pi"`)**:
     - Probes `${CITO_STATE_DIR}/sockets/pi-pid-${pi_pid}.sock` with `{"id":"hb","op":"identify"}\n` (`1500ms` timeout).
     - **Session Switched (`resp.session_id !== registered_session_id`)**: User ran `/new`, `/resume`, or `/fork` in that Pi window. `cito-mcp` immediately sends `unlisten(reason: "session_switched")` for the old `listener_id` and transitions its local status to `'UNLISTENED'`.
     - **Process Exited (`ENOENT` / `ECONNREFUSED` and `kill(pi_pid, 0)` fails with `ESRCH`)**: Immediately sends `unlisten(reason: "process_exited")` and transitions to `'UNLISTENED'`.
     - **Transient Timeout / EOF**: Increments `consecutive_probe_failures`; revokes on the **3rd** consecutive failure.
2. **Gap-Safe Cursor & Immutable Batch Dispatch (`~/.cito/state.sqlite`)**:
   - **Gap Rebasing**: On receiving `bound` with `replay_gap: true` (`persisted_seq + 1 < min_retained_seq`), `cito-mcp` atomically sets `replay_gap_detected = 1`, `replay_gap_range = "${persisted_seq + 1}-${min_retained_seq - 1}"`, and rebases `persisted_seq = min_retained_seq - 1`.
   - **Stage 1 (`RECEIVED` / `STORED_PASSIVE`)**: Incoming `signal` frames matching `local_listeners.binding_generation` are inserted (`state = 'RECEIVED'` if `effective_wake == true`, else `'STORED_PASSIVE'`). `persisted_seq` advances contiguously and `receipt` is sent.
   - **Stage 2 (`DISPATCHING` $\to$ `DISPATCHED` with Immutable Batch Membership, Per-Step Revocation Gate & Expiry Guards)**:
     1. When the `1500ms` debounce timer fires (or on startup recovery), the dispatcher first expires stale rows (`UPDATE local_signals SET state = 'EXPIRED_UNSENT' WHERE state IN ('RECEIVED', 'DISPATCHING') AND expires_at <= now`), then acquires the SQLite listener row lease (`dispatcher_token = uuidv4()`, `lease_until_ms = now + 30_000` — exceeding the maximum multi-attempt sequence of `24.5s`) and verifies `local_listeners.status == 'ACTIVE'`. Each active dispatch registers an in-memory `AbortController` keyed by `(listener_id, binding_generation)` that is immediately aborted (killing any spawned child process via `SIGTERM`) when `cito_unlisten` or `fenced` occurs.
     2. **Retry Existing Frozen Batch First**: If unexpired rows (`expires_at > now`) already exist in `state = 'DISPATCHING'` for `(listener_id, binding_generation)`, `cito-mcp` retries that exact `batch_id` without merging newly arrived `'RECEIVED'` rows into it.
     3. **Freeze New Batch**: Otherwise, selects all current `state = 'RECEIVED' AND expires_at > now` rows for `(listener_id, binding_generation)`, computes `batch_id = "batch_${listener_id}_${min_seq}_${max_seq}_${binding_generation}"`, and atomically updates those rows to `state = 'DISPATCHING', batch_id = ?`.
     4. **Pre-Effect Gate (`assertDispatchStillAuthorized`)**: Immediately before **every** external host-effect step (before Pi socket `deliver`, before `codex queue`, AND between `codex queue` completion and `open -g "codex://threads/wake/<id>"`), `cito-mcp` checks `!abortSignal.aborted` and queries SQLite to confirm `local_listeners.status == 'ACTIVE' AND binding_generation == batch.binding_generation AND dispatcher_token == my_token` and extends `lease_until_ms = now + 30_000`. If revoked, fenced, or expired at any step boundary, execution stops immediately without invoking the next external command.
     5. **Driver Execution & Failure Handling**:
        - **Pi**: Sends `deliver` with `batch_id` (`3000ms` per-attempt socket timeout). On `{"ok": true}`, commits `state = 'DISPATCHED'` (guarded by `WHERE dispatcher_token = my_token`). On `{"ok": false, "error": "SESSION_MISMATCH"}`, marks listener `'UNLISTENED'`, moves the batch to `'STORED_PASSIVE'` (`held_reason = 'session_switched'`), and sends `unlisten`. On socket timeout/error, retains `'DISPATCHING'` and retries with backoff (`1s, 2s`, up to 3 total attempts; if all 3 fail, moves to `'STORED_PASSIVE'` with `held_reason = 'driver_unavailable'`).
        - **Codex Desktop**: Runs `codex queue --thread <id> --message "<card>"` (`5000ms` timeout, bound to `abortSignal`).
          - If `codex queue` fails to spawn or exits non-zero (definite pre-insertion failure): retries `codex queue` with backoff (`1s, 2s`, up to 3 total attempts, calling `assertDispatchStillAuthorized` before each attempt); if all 3 fail, transitions the batch to `'STORED_PASSIVE'` (`held_reason = 'driver_unavailable'`).
          - If `codex queue` succeeds OR times out at `5000ms` (insertion may have succeeded): records `queue_written = 1` so `codex queue` is **never** re-executed for this `batch_id`, calls `assertDispatchStillAuthorized` (aborting before `open -g` if `cito_unlisten` or `fenced` occurred while `codex queue` was running), and invokes `open -g "codex://threads/wake/<id>"` (`3000ms` timeout, bound to `abortSignal`). If `open -g` fails, retries **only `open -g`** once after `500ms` (re-checking `assertDispatchStillAuthorized`), then commits `state = 'DISPATCHED'` (guarded by `WHERE dispatcher_token = my_token`).
        - **Banner Isolation**: `cito-banner` is invoked asynchronously best-effort; a banner error or timeout never blocks or fails Stage-2 wake dispatch.

---

## 6. Anti-Loop Protection & Human-Turn Reset Pathway

### 6.1 Persistent Unattended Wake Quota (`max_unattended_wakes = 3`)
- Stored durably at `cito-hub` per `SessionKey = (principal_id, host_id, harness, session_id)` in `hub.sqlite`.
- **No Time-Based or Re-Enrollment Bypass**: `unattended_wake_count` does **not** reset when calling `cito_unlisten` $\to$ `cito_listen`, when updating `about`, or when a 30-minute offline window expires. Once a `SessionKey` has been enrolled, its `unattended_wake_count` resets to `0` **only** via a verified monotonic `human_event_seq` advancement (Section 6.2).
- Wakes 1, 2, and 3 are admitted (`effective_wake = true`). Starting with the **4th** consecutive wake without an intervening verified human event, Hub forces `effective_wake = false` (`status: "stored_passive"`, `held_reason: "wake_quota_exhausted"`).

### 6.2 Monotonic Human-Event Watermark (`human_event_seq`)
Each listener tracks a monotonically increasing integer `human_event_seq` (starts at `0` in `local_listeners` and `hub.sqlite`):

1. **In Pi (`cito-bridge.ts`)**:
   - Subscribes to `pi.on("input", (event) => ...)`.
   - When `event.source === "interactive"` (an actual human keyboard submission in the TUI, as opposed to `source === "extension"` from `pi.sendMessage`), `cito-bridge.ts` increments its monotonic `human_event_seq += 1` and returns it in `identify` responses to `cito-mcp`.
2. **In Codex Desktop App (Positive Human Evidence Only — No Timing Heuristics)**:
   - Elapsed time after a Cito wake is **never** used to infer human activity.
   - `human_event_seq` for a Codex thread is incremented by 1 when the user clicks the `cito-banner` for that `thread_id` (`cito-banner` atomically increments the thread's `human_event_seq` in `~/.cito/state.sqlite` before opening `codex://threads/<thread_id>`).
3. **Monotonic Hub Consumption (Out-of-Order Safe)**:
   - `cito-hub` stores `last_consumed_human_seq` (integer, default `0`) per `SessionKey`.
   - When `/v1/listen` or WSS `heartbeat` reports `human_event_seq > last_consumed_human_seq`, Hub atomically sets `last_consumed_human_seq = human_event_seq`, resets `unattended_wake_count = 0`, and clears pairwise rate counters for that listener. Delayed or out-of-order heartbeats carrying `human_event_seq <= last_consumed_human_seq` are no-ops.

### 6.3 Pairwise Ping-Pong Circuit Breaker & Suppression Precedence
- For any unordered pair `{SenderEndpointKey, TargetSessionKey}`, Hub tracks admitted `effective_wake == true` signals within a rolling **120-second** window.
- **Exact Threshold**: Up to **4 admitted waking signals** (2 full round-trips) within 120 seconds are permitted. On the **5th** requested wake between that pair within 120 seconds, Hub forces `effective_wake = false` (`held_reason = "pairwise_rate_limited"`).
- **Deterministic `held_reason` Precedence**:
  1. `"passive_requested"` (`request.wake === false`)
  2. `"peer_wake_disabled"` (`sender.principal_id !== target.principal_id && !target.allow_peer_wake`)
  3. `"wake_quota_exhausted"` (`target.unattended_wake_count >= 3`)
  4. `"pairwise_rate_limited"` (pair waking count in last 120s $\ge 4$)

---

## 7. Host Wake Drivers & Human Visibility (V1: Codex Desktop & Pi)

### 7.1 Passive vs. Waking Behavior Matrix

| Signal Type | Stored in `cito_inbox`? | Pi TUI Transcript (`sendMessage`) | Codex Queue + Wake URL | `cito-banner` HUD Shown? |
| :--- | :--- | :--- | :--- | :--- |
| **`effective_wake == true`** | Yes (`RECEIVED` $\to$ `DISPATCHED`) | Yes (`display: true, triggerTurn: true, deliverAs: "followUp"`) | Yes (`codex queue` + `open -g codex://threads/wake/<id>`) | **Yes** |
| **`effective_wake == false`** | Yes (`STORED_PASSIVE`) | **No** (status bar badge updates unread count: `📡 cito: c_ab2def3gh4jk (1 unread)`) | **No** | **Yes** (subtitle prefixed `[仅知会]`) |

### 7.2 Lock-Synchronized Single-Instance `cito-banner` Lifecycle
To guarantee that two `cito-banner` windows/processes can never coexist and no notification is dropped during startup or retirement, coordination uses two locks (`banner.lock` for sender serialization and `banner-instance.lock` held by the running `cito-banner` process for its entire lifetime):

1. **Process-Lifetime Instance Lock (`banner-instance.lock`)**:
   - Immediately upon launch, `cito-banner` acquires an exclusive `flock` on `${CITO_STATE_DIR}/sockets/banner-instance.lock` (waiting up to `2000ms` if a retiring instance is finishing its `250ms` fade-out) and holds that file descriptor open until `exit(0)` (when the OS kernel automatically releases the lock).
2. **Sender Protocol (`cito-mcp` — Non-Blocking Instance Probe to Prevent Lock Cycles)**:
   - Acquires exclusive blocking `flock` on `${CITO_STATE_DIR}/sockets/banner.lock` (timeout `2500ms`).
   - While holding `banner.lock`, attempts to connect to `${CITO_STATE_DIR}/sockets/banner.sock` (`300ms` timeout), writes `JSON.stringify(payload) + "\n"`, and awaits `{"ok":true}\n` (`500ms` timeout).
   - If connect or read/write fails, while **still holding `banner.lock`**, `cito-mcp` tests `${CITO_STATE_DIR}/sockets/banner-instance.lock` with a **non-blocking** `flock(LOCK_EX | LOCK_NB)`:
     - **Case A (`banner-instance.lock` is busy — `EWOULDBLOCK`)**: An existing `cito-banner` process is alive (either still finishing startup or mid-retirement). `cito-mcp` does **not** block on `banner-instance.lock` while holding `banner.lock`. Instead, it polls `banner.sock` connect every `50ms` for up to `500ms`; if `banner.sock` connects, it delivers `payload` and releases `banner.lock`. If `banner.sock` remains unreachable, `cito-mcp` releases `banner.lock` for `300ms` to let the retiring instance acquire `banner.lock` and exit, then re-acquires `banner.lock`, retries connecting to `banner.sock` once (`300ms` timeout), and if still unreachable, **re-probes `banner-instance.lock` with non-blocking `flock(LOCK_EX | LOCK_NB)`**: it enters Case B **only if** `banner-instance.lock` is now free; if `banner-instance.lock` is still busy, it logs a best-effort banner skip and releases `banner.lock` without unlinking `banner.sock` or spawning a duplicate process.
     - **Case B (`banner-instance.lock` is free — acquired and immediately released by `cito-mcp`)**: Confirms zero `cito-banner` processes are running. `cito-mcp` unlinks any stale `banner.sock`, spawns `${CITO_STATE_DIR}/bin/cito-banner` detached, polls `banner.sock` readiness (every `25ms` up to `1000ms`), writes `payload + "\n"`, awaits `{"ok":true}\n`, and always releases `banner.lock` before returning.
3. **Unified Retirement Protocol (Timer Expiry AND Click Dismissal)**:
   - Both the 6-second auto-dismiss timer and user click dismissal execute the same synchronized retirement sequence:
     1. Attempt a non-blocking `flock(LOCK_EX | LOCK_NB)` on `banner.lock`. (For timer expiry, if `EWOULDBLOCK`, abort retirement and process the incoming sender's update; for click dismissal, wait up to `500ms` to acquire `banner.lock`).
     2. While holding both `banner.lock` and `banner-instance.lock`: unlink `banner.sock`, close the listening socket, perform the click action (for Codex: atomically increment `human_event_seq` for `<thread_id>` in `~/.cito/state.sqlite` and open `codex://threads/<thread_id>`) or `250ms` fade-out, order out the window, and call `exit(0)` (which atomically releases both locks after the window and process are gone).

### 7.3 ChatGPT / Codex Desktop App Driver (`ChatGPT.app`)
1. **Desktop IPC Liveness Probe (`${CITO_CODEX_HOME}/ipc/ipc.sock`)**:
   - Framing: 4-byte UInt32LE length + UTF-8 JSON. Timeout: `1500ms`.
   - Sends `initialize` (`version: 1, method: "initialize", params: { clientType: "cli" }`) $\to$ extracts `clientId`.
   - Sends `thread-owner-discovery` (`version: 1, method: "thread-owner-discovery", params: { hostId: "local", conversationId: thread_id }`):
     - `resultType == "success"` $\to$ `ALIVE`.
     - `resultType == "error" && error == "no-client-found"` $\to$ `CLOSED`.
2. **Queue & Wake Execution**:
   - Runs `execFile(CITO_CODEX_BIN, ["queue", "--thread", thread_id, "--message", formattedBatchCard], { timeout: 5000 })`.
   - Runs `execFile("/usr/bin/open", ["-g", "codex://threads/wake/" + thread_id], { timeout: 3000 })`.

### 7.4 Pi Socket Bridge Protocol (`src/pi-extension/cito-bridge.ts`)
- **Transport**: Unix domain socket `${CITO_STATE_DIR}/sockets/pi-pid-${process.pid}.sock` (mode `0600`).
- **Framing**: One short-lived connection per request; one `\n`-terminated JSON request line $\to$ one `\n`-terminated JSON response line.
- **Operations**:
  1. `{"id":"1","op":"identify"}` $\to$ `{"id":"1","ok":true,"pi_pid":51204,"session_id":"01a1a1ef...","session_name":"...","cwd":"...","is_streaming":false,"human_event_seq":2}`
  2. `{"id":"2","op":"status","session_id":"01a1a1ef...","badge":"📡 cito: c_ab2def3gh4jk"}`:
     - If `req.session_id !== currentSessionId`, returns `{"id":"2","ok":false,"error":"SESSION_MISMATCH"}`. Otherwise updates `ctx.ui.setStatus("cito", req.badge)` and returns `{"id":"2","ok":true}`.
  3. `{"id":"3","op":"deliver","session_id":"01a1a1ef...","batch_id":"batch_c_ab2def3gh4jk_41_42_3","content":"...","wake":true}`:
     - If `req.session_id !== currentSessionId`, returns `{"id":"3","ok":false,"error":"SESSION_MISMATCH"}`.
     - If `processedBatchIds.has(req.batch_id)` (ring buffer capacity 200), returns `{"id":"3","ok":true,"deduplicated":true}`.
     - Calls `pi.sendMessage({ customType: "cito-notification", content: req.content, display: true }, { triggerTurn: req.wake, deliverAs: "followUp" })`.
     - **Only after `pi.sendMessage` returns without throwing**, adds `req.batch_id` to `processedBatchIds` and returns `{"id":"3","ok":true,"deduplicated":false}`.

---

## 8. Optional Pure-OIDC Authentication (GCP Casdoor)

### 8.1 Hub Token Verification (`CITO_AUTH_MODE="oidc"`)
- Fetches `${CITO_OIDC_ISSUER}/.well-known/openid-configuration` (`5000ms` timeout) and caches JWKS keys from `jwks_uri`. If JWKS fetch fails on an unknown `kid`, returns `503 AUTH_UNAVAILABLE` (`retryable: true`).
- Verifies Bearer JWT signature (`RS256` / `ES256`), `iss === CITO_OIDC_ISSUER`, `exp > now - 30`, and `aud` contains `CITO_OIDC_CLIENT_ID`.
- If a token expires while a WSS stream is open, Hub sends `{"type":"auth_expired"}` and closes with `4401`, prompting `cito-mcp` to refresh its token and reconnect.

### 8.2 Rejection-Aware Double-Checked JIT Login in `cito-mcp`
- **Token Cache File (`${CITO_STATE_DIR}/oidc-token.json`, mode `0600`)**:
  Stores `{ issuer, client_id, id_token, access_token, refresh_token, token_sha256, expires_at }`.
- **Recovery Flow on Missing/Expired Token or Hub `401`**:
  1. Record `rejected_sha256 = used_token_sha256` (or `null`) and set `deadline = monotonic_now() + 45_000`.
  2. Acquire exclusive `flock` on `${CITO_STATE_DIR}/oidc-login.lock` (polling every `100ms` until `deadline`; on timeout, fail with `OIDC_LOGIN_TIMEOUT`).
  3. **Rejection-Aware Double-Check**: Re-read `oidc-token.json`. Reuse the cached token **only if** `issuer === CITO_OIDC_ISSUER`, `client_id === CITO_OIDC_CLIENT_ID`, `expires_at > now + 60`, **and `token_sha256 !== rejected_sha256`**.
  4. **Silent Refresh**: If `refresh_token` is present, POST `grant_type=refresh_token` to `token_endpoint`. On success, atomically write `oidc-token.json`, release lock, and retry the Hub call **at most once**.
  5. **Interactive Browser PKCE**: Otherwise bind `http://127.0.0.1:0/callback`, generate PKCE `S256` challenge and `state`, open the browser via `/usr/bin/open`, exchange `code` at `token_endpoint` before `deadline`, atomically write `oidc-token.json`, release lock, and retry the Hub call **at most once**.

---

## 9. Deferred Scope: Claude Code Adapter (Proposed Design — TODO / Unverified)

> **Status Notice (Out of V1 Scope)**: Claude Code is not installed in the current environment. The design below is recorded strictly as a **Proposed Specification (Unverified in Target Runtime)** for future implementation by a contributor with a Claude Code environment. It is **excluded** from the V1 codebase and Vectl plan.

1. **Proposed Identity Resolution (`SessionStart` Hook)**:
   - Configure a `SessionStart` hook in `~/.claude/settings.json` that reads `{ session_id, transcript_path, cwd }` from stdin JSON and writes `~/.cito/claude-pids/<PPID>.json`.
2. **Proposed Idle Wake Mechanism (`Stop` Hook with `asyncRewake: true`)**:
   - Configure a background `Stop` hook with `"asyncRewake": true` that waits on `~/.cito/sockets/claude-<session_id>.sock` and exits with code `2` (writing the notification card to `stderr`) when an `effective_wake == true` signal arrives.

---

## 10. Verification & Acceptance Criteria (V1)

1. **Identity, Lifecycle, and Anti-Loop Contract Tests**:
   - Upsert stability: repeating `cito_listen` preserves `listener_id` (`^c_[a-z2-7]{12}$`, e.g., `c_ab2def3gh4jk`), increments `binding_generation`, preserves omitted `scope`/`allow_peer_wake`, and does **not** reset `unattended_wake_count`.
   - Persistent wake quota & monotonic `human_event_seq`: `unattended_wake_count` persists across `unlisten -> listen` and across 30-minute expiry; only `human_event_seq > last_consumed_human_seq` resets it to `0` (out-of-order smaller/equal sequences are ignored).
   - Pairwise circuit breaker: 4 waking signals between `{SenderEndpointKey, TargetSessionKey}` within 120s succeed (`effective_wake: true`); the **5th** signal within 120s is downgraded to `effective_wake: false` with `held_reason: "pairwise_rate_limited"`.
   - Fencing, revocation, and mid-flight abort: calling `cito_unlisten` or receiving `fenced` transitions both `RECEIVED` and `DISPATCHING` rows to `STORED_PASSIVE` (`held_reason: "unlistened_before_dispatch"` / `"fenced_before_dispatch"`), aborts any in-flight driver step before `open -g` wake submission, and keeps those signals readable and acknowledgeable in `cito_inbox`.
   - Gap-rebasing cursor: when Hub reports `replay_gap: true` with `min_retained_seq = 100` while client `persisted_seq = 41`, client records `replay_gap_detected = 1`, rebases `persisted_seq` to `99`, and processes `100..110` contiguously.
2. **OIDC & Security Tests**:
   - Canonical `owner_user` & `principal_id` extraction and cross-user `403 FORBIDDEN_TARGET_SCOPE` enforcement.
   - Rejection-aware double-checked `flock`: when an unexpired token is rejected with `401`, the lock holder does not reuse the same `token_sha256`, refreshes/re-authenticates once, and concurrent waiters reuse the newly written token.
3. **Pi & Codex Desktop Adapter Tests**:
   - Target-driven outbound routing: unbound callers invoke `cito_find` and `cito_send` without `BINDING_REQUIRED`.
   - Pi process-tree ancestor walk resolves `pi-pid-<ancestor_pid>.sock`, handles `identify`, `status`, continuous 20s liveness sweep (revoking on `session_id` change), and `deliver` with frozen `batch_id` retry idempotency.
   - Codex adapter extracts `_meta["x-codex-turn-metadata"].thread_id`, verifies tab liveness over `ipc.sock` (`thread-owner-discovery`), and invokes `codex queue` + `open -g codex://threads/wake/<thread_id>`.
4. **`cito-banner` Lock-Synchronized Lifecycle Tests**:
   - Concurrent cold-start and retirement-boundary banner invocations coordinate via `banner.lock` + `banner.sock` without losing notifications or overlapping processes.
