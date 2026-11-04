# Cito Architecture & Protocol Specification (V1 — NATS JetStream Edition)

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
  1. **NATS JetStream Relay & Optional OIDC Auth Callout (`deploy/nats/` & `src/auth-callout/`)**:
     - Uses official `nats-server` (`>= 2.10`) with **JetStream** enabled (`cito_listeners` KV bucket for presence/leases and `CITO_SIGNALS` stream for 30-minute durable retention, sequence ordering, and `Nats-Msg-Id` deduplication).
     - Optional stateless NATS **Auth Callout** responder (`src/auth-callout/`) that validates standard OIDC Bearer JWTs (compatible with GCP Casdoor) via `.well-known/openid-configuration` + `jwks_uri` and issues scoped NATS user permissions.
  2. **`cito-mcp` (`src/mcp/`)**: Unified stdio MCP server exposing `cito_listen`, `cito_unlisten`, `cito_find`, `cito_send`, and `cito_inbox`. Connects to NATS over TCP (`nats://`) or WebSocket (`ws://` / `wss://`), enforces recipient-side anti-loop wake quotas and pairwise circuit breaking, manages two-stage crash-safe local batch dispatch (`~/.cito/state.sqlite`), runs 20-second bounded local liveness sweeps, and performs Just-in-Time (JIT) OIDC browser login.
  3. **ChatGPT / Codex Desktop App (`ChatGPT.app`) Adapter (`src/adapters/codex.ts`)**: Deterministic caller binding via `_meta["x-codex-turn-metadata"].thread_id`, tab liveness verification via `~/.codex/ipc/ipc.sock` (`thread-owner-discovery`), queue injection via `codex queue`, and non-focus-stealing wake via `open -g "codex://threads/wake/<thread_id>"`.
  4. **Pi Coding Agent (`1.5.0`) Adapter (`src/pi-extension/cito-bridge.ts` & `src/adapters/pi.ts`)**: Self-contained local Unix socket bridge extension listening on `~/.cito/sockets/pi-pid-<pid>.sock` and invoking `pi.sendMessage(..., { triggerTurn, deliverAs: "followUp" })`. V1 delivers and tests the extension inside this repository without mutating external dotfiles repositories during tests.
  5. **`cito-banner` (`src/macos/cito-banner.swift` & `src/banner/`)**: Zero-dependency macOS native `NSPanel` HUD banner compiled via `/usr/bin/swiftc`, with lock-synchronized single-instance Unix socket coalescing (`~/.cito/sockets/banner.sock`) and click-to-jump (`codex://threads/<thread_id>`) for Codex Desktop notifications.
- **Deferred Scope (TODO — Excluded from V1 Implementation & Vectl Plan)**:
  - **Claude Code Adapter**: The target development environment does not have Claude Code installed. The Claude Code integration is documented in **Section 9** strictly as a **Proposed Design (Unverified in Target Runtime / TODO)** for future contributors. V1 rejects `harness: "claude"` with `422 UNSUPPORTED_HARNESS`, and `cito_find` only accepts `harness: "codex" | "pi"`.

### 1.3 Non-Goals
- **No Custom Message Broker Re-Implementation**: Cito uses NATS (JetStream + KV) rather than building a custom HTTP/WebSocket message broker from scratch.
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
│             │    │  ├─ NATS Client (JetStream + KV over NATS/WSS)   │      │
│             │    │  ├─ Recipient Wake Quota & Pairwise Breaker      │      │
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
                                            │ Outbound NATS / WS(S)
                                            ▼
                   ┌──────────────────────────────────────────────────┐
                   │ NATS Server (JetStream Enabled, nats:2.10-alpine)│
                   │  ├─ KV Bucket: cito_listeners (Presence & State) │
                   │  ├─ JS Stream: CITO_SIGNALS (30m Replay & Dedup) │
                   │  ├─ Core NATS: cito.ack.<signal_id> (Live Ack)   │
                   │  └─ Optional: cito-auth-callout (Casdoor OIDC)   │
                   └──────────────────────────────────────────────────┘
```

### 2.1 Environment Variables & Configuration Contract
Invalid configuration values cause the respective process (`cito-mcp` or `cito-auth-callout`) to fail fast at startup with exit code `1` and a diagnostic on `stderr`.

| Variable | Process | Required / Default | Validation & Semantics |
| :--- | :--- | :--- | :--- |
| `CITO_NATS_URL` | `cito-mcp`, `cito-auth-callout` | Optional, default `"nats://127.0.0.1:4222"` | NATS connection URL (`nats://host:port`, `ws://host:port`, or `wss://host:port`). |
| `CITO_AUTH_MODE` | `cito-mcp`, `cito-auth-callout` | Optional, default `"none"` | Enum: `"none"` \| `"oidc"`. |
| `CITO_OIDC_ISSUER` | `cito-mcp`, `cito-auth-callout` | Required if `CITO_AUTH_MODE="oidc"`, else optional | HTTPS URL (or `http://127.0.0.1:<port>` in tests) of the OIDC Provider (e.g., `"https://auth.internal.rda.ai"`). |
| `CITO_OIDC_CLIENT_ID` | `cito-mcp`, `cito-auth-callout` | Required if `CITO_OIDC_ISSUER` is set | Single OIDC Client ID string registered at the issuer (used as required JWT `aud` claim in `cito-auth-callout` and as `client_id` during PKCE login in `cito-mcp`). |
| `CITO_AUTH_CALLOUT_ISSUER_SEED` | `cito-auth-callout` | Required for `cito-auth-callout` | NATS Account/Operator NKey seed (`SA...` / `SO...`) used to sign NATS Authorization Response JWTs. |
| `CITO_AUTH_CALLOUT_XKEY_SEED` | `cito-auth-callout` | Optional | Curve25519 XKey seed (`SX...`) when NATS Auth Callout encryption is enabled in `nats-server.conf`. |
| `CITO_STATE_DIR` | `cito-mcp`, `cito-bridge.ts`, `cito-banner` | Optional, default `"~/.cito"` | Base local state directory (`0700`), containing `host-id`, `state.sqlite`, `oidc-token.json`, `oidc-login.lock`, `bin/cito-banner`, and `sockets/`. |
| `CITO_USER` | `cito-mcp` | Optional, default `os.userInfo().username` | Used when `CITO_AUTH_MODE="none"`. Sanitized to `^[a-zA-Z0-9._-]{1,64}$`. |
| `CITO_HOST_NAME` | `cito-mcp` | Optional, default `os.hostname()` | Human-readable machine label (truncated to 128 ASCII chars). |
| `CITO_CODEX_BIN` | `cito-mcp` | Optional, default `"/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"` | Path to the Codex Desktop CLI binary used for `codex queue`. |
| `CITO_CODEX_HOME` | `cito-mcp` | Optional, default `"~/.codex"` | Directory containing `ipc/ipc.sock` and `state_5.sqlite`. |

---

## 3. Identity, Addressing, and Caller Resolution

### 3.1 Canonical Storage Identity and Meaningful Public Listener IDs
Every listener registration is uniquely keyed by the 4-tuple:

$$\text{SessionKey} = (\text{principal\_id}, \text{host\_id}, \text{harness}, \text{session\_id})$$

1. **Single Canonical `principal_id` and `owner_user` Algorithm**:
   - **When `CITO_AUTH_MODE="oidc"`**: Verified from the OIDC `id_token` JWT (cryptographically verified against the issuer's JWKS by both `cito-auth-callout` and `cito-mcp`):
     - Immutable authorization principal: `principal_id = "${claims.iss}#${claims.sub}"`.
     - Display handle `owner_user`:
       1. `claims.preferred_username` (if non-empty string), else
       2. local-part of `claims.email` before `@` (if non-empty string containing `@`), else
       3. `claims.sub`.
       Lowercased and non-`[a-z0-9._-]` characters replaced with `_` (max 64 chars). All authenticated users under the same `CITO_OIDC_ISSUER` belong to the same workspace.
   - **When `CITO_AUTH_MODE="none"`**: `owner_user` is `CITO_USER` or OS username (lowercased, validated against `^[a-z0-9._-]{1,64}$`), and `principal_id = "none#" + owner_user`.
2. **`host_id`**:
   - On first run, `cito-mcp` generates a lowercase UUIDv4 and writes it atomically to `${CITO_STATE_DIR}/host-id` (`0600`). Subsequent runs read this file so machine hostname changes never alter `host_id`.
3. **Keyless Deterministic Canonical ID** (`listenerId(sessionKey)`):
   - Format: `^c_[a-z2-7]{12}$` (prefix `c_` followed by 12 lowercase RFC 4648 base32 characters `[a-z2-7]`, e.g., `c_ab2def3gh4jk`).
   - Computed deterministically without any server-side secret as:
     $$\text{listener\_id} = \text{"c\_"} + \text{base32\_lower}\big(\text{SHA256}(\text{"cito-v1|"} + \text{principal\_id} + \text{"|"} + \text{host\_id} + \text{"|"} + \text{harness} + \text{"|"} + \text{session\_id})[0..7]\big)[0..12]$$
   - This 60-bit value is `canonical_id = "c_" + fp12`; the formula above names the internal `listener_id`. It remains the key for every local listener/signal/pairwise/idempotency row, lock (`listen-c_<fp12>.lock`), NATS KV entry (`cito_listeners.c_<fp12>`) and JetStream subject (`cito.signal.c_<fp12>`). It is computable offline and never changes when display metadata changes.
4. **Public Display ID**: `public_id = "${slug}-${canonical_id.slice(2)}"`, matching `^[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12}$`, for example `rcatwin-teams-pbtjxcxkgw3y`.
   - `normalizeSlug(raw)` lowercases, replaces each run of non-ASCII-alphanumeric characters with `-`, trims edge hyphens, truncates to 20 characters, then removes trailing hyphens again.
   - `cito_listen.tag?: string` accepts at most 64 characters. An explicit tag with no ASCII letter/digit after normalization fails with `INVALID_INPUT: tag must contain at least one ASCII letter or digit`.
   - With no new tag, choose the first nonempty normalized candidate: existing explicit SQLite tag, repository name after the last `/`, `basename(cwd)`, conversation title, harness (`codex` / `pi`). Directory names deliberately outrank conversation titles.
   - SQLite `local_listeners` stores `tag: string | null` and `public_id: string`. Opening an existing database inspects `PRAGMA table_info(local_listeners)`, adds missing columns with `ALTER TABLE` under a transaction, and backfills display IDs without rewriting canonical keys, quota, cursors or ownership. KV registrations add optional `public_id` and `tag` fields so old registrations remain readable.
   - Every public MCP `listener_id`, including `cito_find.caller.listener_id` and `signals[].from.listener_id`, uses the public ID (or `null` for an unbound sender). Discovery derives one for legacy registrations; an unregistered offline inbox derives one from caller metadata. The send result's `to` uses the target's current public ID.
   - `toCanonicalListenerId` accepts either `^c_[a-z2-7]{12}$` unchanged or a public ID and extracts its final fingerprint; all other forms fail with `INVALID_INPUT`. Slugs are display hints: an old or renamed slug still addresses the same canonical destination. KV lookup, self-send checks, publication, receiver recipient checks and sender idempotency use the canonical form.
5. **Collision Guard**: Before cancelling a receiver or changing SQLite in `listen`, inspect the canonical KV entry. Its `(principal_id, host_id, harness, session_id)` must match the caller exactly; otherwise fail with `STALE_GENERATION: Listener ID collision`. The local tuple is checked too; CAS retries recheck remote identity.

### 3.2 Target-Driven Routing vs. Receiver-Only Session Binding
Cito separates **outbound operations (`cito_find`, `cito_send`)** from **receiver-side operations (`cito_listen`, `cito_unlisten`, `cito_inbox`)**:

1. **Outbound (`cito_find`, `cito_send`) Requires NO Caller Harness/Session Detection**:
   - Routing and wake dispatch depend **exclusively on the target's registered identity** (`to = listener_id`), which is published to the NATS subject `cito.signal.<listener_id>` and already records the target's `(harness, session_id, host_id)` at the recipient.
   - Therefore, `cito_find` and `cito_send` never require the sender to resolve a local harness or `session_id`, and **never fail with `BINDING_REQUIRED`**.
   - **Canonical `SenderEndpointKey`** (used consistently across self-send checks, idempotency scoping, and pairwise circuit-breaking):
     - If the caller's `(harness, session_id)` is resolved: `SenderEndpointKey = "${principal_id}|${host_id}|${harness}|${session_id}"`.
     - If the caller is unbound (`harness = null, session_id = null`): `SenderEndpointKey = "${principal_id}|${host_id}|unbound|default"`. (Bound and unbound calls have distinct `SenderEndpointKey`s and distinct idempotency/pairwise namespaces).
2. **Receiver Binding (`cito_listen`, `cito_unlisten`, `cito_inbox`)**:
   Only when a session calls `cito_listen`, `cito_unlisten`, or `cito_inbox` does `cito-mcp` resolve the caller's own `(harness, session_id, cwd)`. If resolution fails on these three receiver tools, the tool returns an MCP error (`isError: true`, `code: "BINDING_REQUIRED"` | `"SESSION_NOT_OPEN"` | `"PI_BRIDGE_UNAVAILABLE"`) without guessing from window focus or recent timestamps:

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
     - `cito-bridge.ts` replies with `{ "id": "1", "ok": true, "pi_pid": <pid>, "session_id": "<live_uuid>", "session_name": "...", "cwd": "...", "is_streaming": true, "bridge_epoch": "e1...", "human_event_count": 2 }`.
     - If no ancestor socket exists or the socket fails to respond within `1500ms`, fails with `PI_BRIDGE_UNAVAILABLE` (or `BINDING_REQUIRED` if neither Codex metadata nor a Pi ancestor is present).

### 3.3 Repository & Metadata Normalization
- For both Codex and Pi, if `git_origin_url` or `git -C <cwd> remote get-url origin` is available, `cito-mcp` normalizes SSH (`git@github.com:Owner/Repo.git`) and HTTPS (`https://github.com/Owner/Repo.git`) URLs to `"Owner/Repo"` (stripping `.git` and trailing slashes, preserving case for display and comparing case-insensitively in `cito_find`). If no Git remote exists, `repo` is `null`.
- For Pi, `branch` is read via `git -C <cwd> rev-parse --abbrev-ref HEAD` (or `null`), and `title` is `session_name || basename(cwd)`.

---

## 4. Public MCP Tool Contracts & Error Envelope

### 4.1 Common MCP Result & Error Representation
- Every successful tool call returns `isError: false` and `content: [{ type: "text", text: JSON.stringify(payload, null, 2) }]`.
- Every failed tool call returns `isError: true` and `content: [{ type: "text", text: JSON.stringify({ ok: false, error: { code, message, retryable } }, null, 2) }]`.
- **Canonical Error Codes**:

| Error Code | `retryable` | Trigger Condition |
| :--- | :--- | :--- |
| `BINDING_REQUIRED` | `false` | Receiver tool (`cito_listen`, `cito_unlisten`, `cito_inbox`) called without Codex `_meta` or a reachable Pi ancestor socket. |
| `SESSION_NOT_OPEN` | `false` | `cito_listen` called for a closed or archived Codex Desktop thread. |
| `PI_BRIDGE_UNAVAILABLE` | `true` | Pi ancestor socket exists but timed out (`1500ms`) or returned an error. |
| `INVALID_INPUT` | `false` | Schema validation failure or `message` / `about` empty after control-character stripping and trimming. |
| `UNSUPPORTED_HARNESS` | `false` | `harness` is not `"codex"` or `"pi"` (e.g., `"claude"` in V1). |
| `UNAUTHORIZED` | `true` | NATS OIDC authentication rejected and JIT refresh/login failed or was cancelled. |
| `OIDC_LOGIN_TIMEOUT` | `true` | OIDC lock wait + refresh + browser PKCE flow exceeded the 45-second deadline. |
| `AUTH_UNAVAILABLE` | `true` | OIDC discovery / `jwks_uri` unreachable during cache miss. |
| `FORBIDDEN_TARGET_SCOPE` | `false` | `sender.principal_id !== target.principal_id` and `target.scope !== "workspace"`. |
| `LISTENER_NOT_FOUND` | `false` | Unknown `listener_id` in NATS KV `cito_listeners` or target `state == "UNLISTENED"`. |
| `LISTENER_EXPIRED` | `false` | Target listener entry exists in NATS KV with `state == "ACTIVE"`, but its broker-recorded `kv_entry.created` age exceeds 30 minutes (`broker_age_ms > 1_800_000`). |
| `SELF_SEND_FORBIDDEN` | `false` | `SenderEndpointKey` equals target `SessionKey`. |
| `IDEMPOTENCY_KEY_CONFLICT` | `false` | Same `(SenderEndpointKey, canonical_to, idempotency_key)` reused within 30m with different `(message, wake, in_reply_to)`. |
| `STALE_GENERATION` | `true` | Stale generation, takeover/CAS conflict, or canonical fingerprint collision with a different session tuple. |
| `NATS_UNAVAILABLE` | `true` | Network failure or timeout (`10s`) reaching NATS Server. |

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
      "tag": { "type": "string", "maxLength": 64, "description": "Optional display slug override; omission preserves an existing explicit tag." },
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
    "listener_id": "rcatwin-ab2def3gh4jk",
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
  - If active: updates the NATS KV entry `cito_listeners.<listener_id>` to `state: "UNLISTENED"` (incrementing `binding_generation`), stops the NATS JetStream consumer for `<listener_id>`, aborts any in-flight `AbortController`, cancels any pending debounce or retry timer, atomically transitions any undispached `state IN ('RECEIVED', 'DISPATCHING')` signals for that session in `local_signals` to `state = 'STORED_PASSIVE', held_reason = 'unlistened_before_dispatch'` (so neither initial dispatch nor any retry can wake the session after revocation, while keeping all signals readable and acknowledgeable via `cito_inbox` until expiry), updates `local_listeners.status = 'UNLISTENED'`, and (for Pi) clears the `cito` status bar badge.
- **Output Schema**:
  ```json
  {
    "ok": true,
    "status": "unlistened",
    "listener_id": "rcatwin-ab2def3gh4jk"
  }
  ```

### 4.4 `cito_find`
Searches active listeners in the NATS KV bucket `cito_listeners`.

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
- **Visibility & Filtering Rules (Broker-Clock Authoritative)**:
  - Queries `jsm.streams.info("KV_cito_listeners")` to obtain the NATS server's current authoritative timestamp `broker_now_ms` (`Date.parse(stream_info.ts)`), then reads entries from NATS KV `cito_listeners` and computes `broker_age_ms = Math.max(0, broker_now_ms - Date.parse(kv_entry.created))`.
  - Includes only entries where `entry.state === "ACTIVE"` and `broker_age_ms < 60_000` (60-second active lease).
  - A listener with `scope === "self"` is included **if and only if** `caller.principal_id === entry.principal_id`.
  - A listener with `scope === "workspace"` is included for any authenticated caller in the workspace.
  - Results are ordered deterministically by `kv_entry.revision DESC, listener_id ASC`.
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
        "listener_id": "rcatwin-ab2def3gh4jk",
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
Sends a notification signal to an opted-in listener via NATS JetStream.

- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "to": {
        "type": "string",
        "pattern": "^(?:c_[a-z2-7]{12}|[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12})$",
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
  2. Trim leading/trailing whitespace. If the resulting string is empty or exceeds 500 Unicode code points, reject with `INVALID_INPUT` **before** publishing.
  3. Escape `<` as `&lt;` and `>` as `&gt;` when rendering into wake cards.
- **Pre-Publish Target Validation**: Compute `canonical_to = toCanonicalListenerId(to)` before looking up NATS KV `cito_listeners.<canonical_to>` or comparing the caller's canonical ID for self-send protection.
  1. Fetch `kv_entry = kv.get(canonical_to)` and `stream_info = jsm.streams.info("KV_cito_listeners")`. If `kv_entry` is missing or `target.state === "UNLISTENED"`, fail with `LISTENER_NOT_FOUND`.
  2. Compute `broker_age_ms = Math.max(0, Date.parse(stream_info.ts) - Date.parse(kv_entry.created))`. If `broker_age_ms > 1_800_000` (30 minutes since last heartbeat renewal), fail with `LISTENER_EXPIRED`.
  3. If `SenderEndpointKey === "${target.principal_id}|${target.host_id}|${target.harness}|${target.session_id}"`, fail with `SELF_SEND_FORBIDDEN`.
  4. If `sender.principal_id !== target.principal_id && target.scope !== "workspace"`, fail with `FORBIDDEN_TARGET_SCOPE`.
- **Crash-Safe Sender Idempotency & Milestone Semantics**:
  - If caller omits `idempotency_key`, `cito-mcp` generates a fresh UUIDv4 for that single call.
  - Keyed by `dedup_id = SHA256(JSON.stringify([SenderEndpointKey, canonical_to, effective_idempotency_key]))` for 30 minutes in the sender's `local_sent_idempotency(dedup_id PRIMARY KEY, request_hash, signal_id, seq, status, effective_wake, held_reason, expires_at_ms)` SQLite table.
  - **Transactional Pre-Publish Reservation**:
    1. Before publishing to NATS, `cito-mcp` begins a local SQLite transaction and looks up `dedup_id` (where `expires_at_ms > Date.now()`):
       - If found with a different `request_hash = SHA256(JSON.stringify([message, wake, in_reply_to ?? null]))`, rolls back and rejects with `IDEMPOTENCY_KEY_CONFLICT`.
       - If found with matching `request_hash` and `status != 'PENDING_PUBLISH'`, returns the stored result immediately without re-publishing.
       - If found with `status == 'PENDING_PUBLISH'` (recovering from a prior crash mid-publish), reuses the exact stored `signal_id` and proceeds to publish with `Nats-Msg-Id: dedup_id`.
       - If not found, allocates a new `signal_id` and commits a reservation row with `status = 'PENDING_PUBLISH'` **before** sending over the network.
    2. Subscribes to Core NATS reply subject `cito.ack.<signal_id>` (`timeout = 2000ms`), then publishes the signal (including `"target_generation": target.binding_generation`) to JetStream `cito.signal.<canonical_to>` with header `Nats-Msg-Id: dedup_id` (receiving `PubAck.seq`).
    3. If the recipient `cito-mcp` is online, it runs its atomic Stage-1 admission transaction (Section 5.3) and publishes `{ signal_id, effective_wake, status: effective_wake ? "wake_accepted" : "stored_passive", held_reason }` to `cito.ack.<signal_id>`. Sender takes the first valid reply, updates `local_sent_idempotency` with the final `status`, and unsubscribes.
    4. If no reply arrives on `cito.ack.<signal_id>` within `2000ms`: if `wake === false` or (`sender.principal_id !== target.principal_id && !target.allow_peer_wake`), sender records and returns `effective_wake: false, status: "stored_passive", held_reason: "passive_requested" | "peer_wake_disabled"`; otherwise sender records and returns `effective_wake: true, status: "queued_for_reconnect", held_reason: null`.
- **Output Schema**:
  ```json
  {
    "ok": true,
    "signal_id": "sig_019a4b8c7d2e70008123456789abcdef",
    "seq": 42,
    "to": "rcatwin-ab2def3gh4jk",
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
  1. Resolve caller `(harness, session_id)` (Section 3.2), compute its canonical storage key (Section 3.1.3), and resolve the public `listener_id` from SQLite or caller metadata (Section 3.1.4). If the session has never registered locally, return `{ "ok": true, "listener_id": "<listener_id>", "replay_gap_detected": false, "replay_gap_range": null, "signals": [], "acknowledged_count": 0 }`.
  2. Select all rows in `local_signals` where `listener_id = ? AND acknowledged_at_ms IS NULL AND state IN ('RECEIVED', 'DISPATCHING', 'DISPATCHED', 'STORED_PASSIVE') AND expires_at_ms > Date.now()` ordered by `seq ASC` (limit 50).
  3. Determine the set of IDs to acknowledge: if `ack_all == true`, all selected `signal_id`s plus any additional IDs in `ack_ids` belonging to this `listener_id`; if `ack_all == false`, only IDs in `ack_ids` belonging to this `listener_id` (unknown IDs or IDs belonging to other sessions are ignored).
  4. Update those rows with `acknowledged_at_ms = Date.now()` (an independent column so a concurrent Stage-2 driver completion setting `state = 'DISPATCHED'` can never clear `acknowledged_at_ms` or resurrect an acknowledged message; if any acknowledged row is still in `state = 'RECEIVED'`, also set `state = 'STORED_PASSIVE'` so it will not be batched for wake).
  5. If `replay_gap_detected == 1` on `local_listeners`, include `replay_gap_detected: true` and `replay_gap_range` in the response, and clear `replay_gap_detected = 0, replay_gap_range = NULL` when `ack_all == true`.
- **Output Schema**:
  ```json
  {
    "ok": true,
    "listener_id": "rcatwin-ab2def3gh4jk",
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
          "listener_id": "rcatwin-mn5pqr6st7uv",
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

## 5. NATS JetStream Topology, Lease Lifecycle, and Crash-Safe Dispatch

### 5.1 NATS KV (`cito_listeners`) & JetStream (`CITO_SIGNALS`) Configuration
In storage, lock names and routing subjects below, `listener_id` / `<id>` always denotes the canonical `c_<fp12>` key. Public display IDs appear in tool output and sender display metadata only.
1. **KV Bucket `cito_listeners` (Persistent Metadata, Broker-Timestamp Leases)**:
   - Configured with `history: 1` and **no auto-purge TTL** (`ttl: 0`). Because entries are never auto-purged by TTL, `binding_generation` never resets to `1` after a disconnect, and `LISTENER_EXPIRED` vs. `LISTENER_NOT_FOUND` are always distinguishable.
   - Liveness state is derived from `entry.state` and the NATS broker timestamp age `broker_age_ms = Math.max(0, Date.parse(stream_info.ts) - Date.parse(kv_entry.created))`:

| Logical State | Condition on KV Entry | Visibility in `cito_find` | `cito_send` Behavior |
| :--- | :--- | :--- | :--- |
| **`ACTIVE`** | `entry.state == "ACTIVE" && broker_age_ms < 60_000` | **Visible** (subject to `scope`) | Published to `cito.signal.<id>` + awaits `cito.ack.<sig_id>` (`2s`). |
| **`DISCONNECTED_RETAINED`** | `entry.state == "ACTIVE" && 60_000 <= broker_age_ms <= 1_800_000` | **Hidden** | Published to `cito.signal.<id>` (buffered in JetStream for reconnect); returns `queued_for_reconnect` (or `stored_passive`). |
| **`EXPIRED`** | `entry.state == "ACTIVE" && broker_age_ms > 1_800_000` | **Hidden** | Rejected with `LISTENER_EXPIRED`. |
| **`UNLISTENED`** | Missing key or `entry.state == "UNLISTENED"` | **Hidden** | Rejected with `LISTENER_NOT_FOUND`. |

2. **JetStream Stream `CITO_SIGNALS`**:
   - `subjects: ["cito.signal.*"]`
   - `retention: "limits"`, `max_age: 30 * 60 * 1e9` (`30m` in nanoseconds), `duplicate_window: 30 * 60 * 1e9` (`30m`), `storage: "file"`, `discard: "old"`.
   - **Signal Wire Payload (`cito.signal.<listener_id>`)**:
     ```json
     {
       "signal_id": "sig_019a4b8c...",
       "to": "c_ab2def3gh4jk",
       "target_generation": 3,
       "sender_endpoint_key": "https://auth.internal.rda.ai#u1|9b1d...|pi|01a1...",
       "from": {
         "principal_id": "https://auth.internal.rda.ai#u1",
         "user": "tefx",
         "host_id": "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
         "host_name": "tefx-mbp-work",
         "harness": "pi",
         "listener_id": "dotfiles-mn5pqr6st7uv",
         "about": "dotfiles configuration"
       },
       "message": "配置已更新并推送，请拉取最新分支",
       "requested_wake": true,
       "in_reply_to": null
     }
     ```
   - **Clock-Skew & Latency-Conservative Local Expiration Mapping**: When a recipient receives a JetStream message, it records `wall_at_req_start = Date.now()` and `mono_start = performance.now()` **before** querying a fresh `stream_info = jsm.streams.info("CITO_SIGNALS")`. Upon receiving the response (`rtt_ms = Math.ceil(performance.now() - mono_start)`), it computes `already_elapsed_ms = Math.max(0, Date.parse(stream_info.ts) - msg_timestamp_ms)` and anchors the recipient-local deadline conservatively to the request start time minus RTT:
     `expires_at_ms = wall_at_req_start + Math.max(0, 1_800_000 - already_elapsed_ms - rtt_ms)`.
     All subsequent local SQLite expiry checks (`expires_at_ms > Date.now()`) compare against the recipient's own clock without ever extending lifetime by network response latency.

### 5.2 Lock-Serialized KV Revision Fencing & Multi-Process Ownership (`binding_generation` + `owner_token`)
- Each `cito_listen` call generates a unique `owner_token = uuidv4()`.
- **Lock-Serialized Takeover Sequence (Preventing Local/KV Split-Brain)**:
  1. Acquire an exclusive local file lock `flock("${CITO_STATE_DIR}/sockets/listen-${listener_id}.lock")` (timeout `5000ms`), serializing all local processes attempting to register or update `listener_id`.
  2. While holding `listen-${listener_id}.lock`, first reject any KV/local tuple collision (Section 3.1.5), before stopping the previous receiver or mutating SQLite; then set `local_listeners.status = 'REBINDING'` in `~/.cito/state.sqlite` (which immediately halts any prior local process's Stage-2 `assertDispatchStillAuthorized` gate before the KV write occurs).
  3. Read `kv_entry = kv.get(listener_id)` and `local_row` from `local_listeners`. Compute `next_gen = Math.max(kv_entry?.binding_generation ?? 0, local_row?.binding_generation ?? 0) + 1`.
  4. Write the updated entry (`binding_generation: next_gen, owner_token: my_owner_token, state: "ACTIVE"`) to NATS KV using CAS (`kv.update(listener_id, val, kv_entry.revision)` or `kv.create`). If CAS fails due to a concurrent update, re-read `kv_entry`, recompute `next_gen`, and retry up to 3 times.
  5. **Only after NATS KV CAS succeeds**: commit `status = 'ACTIVE', binding_generation = next_gen, owner_token = my_owner_token` to `local_listeners` in SQLite and release `listen-${listener_id}.lock`. (If KV CAS fails after all retries, restore/set `local_listeners.status = 'FENCED_INACTIVE'`, release the lock, and return `STALE_GENERATION` — never leaving SQLite owned by a failed contender).
- **Scoped Fencing Cleanup (Preventing Winner Clobber)**:
  - Because `listen-${listener_id}.lock` + KV CAS guarantees that every successful takeover has a strictly increasing unique `(binding_generation, owner_token)`, when an older process A (with `A.generation` and `A.owner_token`) observes `kv.owner_token !== A.owner_token` or `kv.state === "UNLISTENED"`:
  - Process A aborts its in-memory `AbortController`, stops its JetStream consumer, and executes SQLite updates **strictly qualified by `A.owner_token` and `A.generation`**:
    - `UPDATE local_listeners SET status = 'FENCED_INACTIVE' WHERE listener_id = ? AND owner_token = A.owner_token`
    - `UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'fenced_before_dispatch' WHERE listener_id = ? AND binding_generation <= A.generation AND state IN ('RECEIVED', 'DISPATCHING')`
  - Because winning process B's `B.generation > A.generation` and `B.owner_token !== A.owner_token`, A's cleanup can never clobber B's `ACTIVE` listener row or B's signals.

### 5.3 Bounded Continuous Liveness Sweep & Two-Stage Crash-Safe Dispatch
1. **Bounded Continuous Liveness Sweep (Every 20s, Shared Healthy Renewal for Both Harnesses)**:
   Every 20 seconds, `cito-mcp` probes all locally `ACTIVE` listeners owned by this process (`owner_token == my_owner_token`) concurrently via `Promise.allSettled` with a **`1500ms` timeout per listener**:
   - **Codex Desktop App (`harness == "codex"`)**:
     - Queries `state_5.sqlite` (`archived`) and probes `${CITO_CODEX_HOME}/ipc/ipc.sock` (`thread-owner-discovery`, Section 7.3).
     - Definitive closure (`archived == 1`, `no-client-found`, or `ChatGPT.app` process absent): marks `'UNLISTENED'` in SQLite (`WHERE owner_token = my_owner_token`) and NATS KV.
   - **Pi (`harness == "pi"`)**:
     - Probes `${CITO_STATE_DIR}/sockets/pi-pid-${pi_pid}.sock` with `{"id":"hb","op":"identify"}\n` (`1500ms` timeout).
     - Definitive closure (`resp.session_id !== registered_session_id`, or `ENOENT`/`ECONNREFUSED` with `kill(pi_pid, 0)` `ESRCH`): marks `'UNLISTENED'` in SQLite and NATS KV.
   - **Shared Transient Failure Rule (Both Codex and Pi)**: On probe timeout or transient socket error, increments `consecutive_probe_failures`; on the **3rd** consecutive failure (`60s`), marks `'UNLISTENED'` in SQLite and NATS KV.
   - **Shared Healthy Renewal Rule (Both Codex and Pi)**: When the probe succeeds, resets `consecutive_probe_failures = 0`, processes any `human_event_seq` advancement (Section 6.2), and refreshes the NATS KV entry via `kv.update(listener_id, entry, last_revision)` (which updates the NATS server's `kv_entry.created` timestamp and resets `broker_age_ms` to `0`).
2. **JetStream Consumer, Listener-Specific Gap Detection & Atomic Stage-1 Admission**:
   - **Consumer Model**: Uses an ephemeral NATS JetStream Pull Consumer on stream `CITO_SIGNALS` filtered to `filter_subject: "cito.signal." + listener_id`, configured with `deliver_policy: "by_start_sequence"`, `opt_start_seq: persisted_seq + 1` (or `deliver_policy: "all"` when `persisted_seq == 0`), and `ack_policy: "explicit"`. On reconnect or process restart, a fresh ephemeral pull consumer is created starting at `opt_start_seq = persisted_seq + 1` so local `persisted_seq` in `state.sqlite` is the single authoritative cursor.
   - **Listener-Specific Replay Gap Detection**: When re-attaching after a disconnect where the previous KV `broker_age_ms > 1_800_000` (the listener was offline longer than the 30-minute `CITO_SIGNALS` retention window), `cito-mcp` sets `replay_gap_detected = 1` and `replay_gap_range = "offline_exceeded_30m"` on `local_listeners`.
   - **Stage 1 (Single Atomic Idempotent SQLite Transaction)**:
     - On receiving a JetStream message (`seq = msg.seq`):
       `cito-mcp` runs a single `BEGIN IMMEDIATE` transaction in `~/.cito/state.sqlite`:
       1. **Deduplication Check First**: Queries `SELECT effective_wake, held_reason, state FROM local_signals WHERE signal_id = ?`.
          - If the row already exists (e.g., redelivered after a crash before `msg.ack()`): advances `persisted_seq = MAX(persisted_seq, msg.seq)`, commits, and **reuses the stored `(effective_wake, held_reason)`** to publish `cito.ack.<signal_id>` and `msg.ack()` **without** re-charging wake quota, re-charging pairwise counters, or re-triggering a banner.
       2. **Scope Check**: If `msg.from.principal_id !== local_listener.principal_id && local_listener.scope !== 'workspace'`, advances `persisted_seq = MAX(persisted_seq, msg.seq)`, commits without storing, and calls `msg.ack()`.
       3. **Epoch, Quota & Pairwise Evaluation**: Consumes any new `human_event_seq` (Section 6.2). Evaluates recipient wake admission using the deterministic precedence order in Section 6.3 (including checking `msg.target_generation < local_listener.binding_generation` as priority #1 `"stale_generation"`). If `effective_wake == true`, increments `unattended_wake_count` and inserts into `local_pairwise_wakes`. Inserts the new row into `local_signals` (`state = 'RECEIVED'` if `effective_wake == true`, else `state = 'STORED_PASSIVE'` — including for `"stale_generation"` signals so they remain readable in `cito_inbox`!), advances `persisted_seq = MAX(persisted_seq, msg.seq)`, and commits.
       4. After commit: publishes `{ signal_id, effective_wake, status: effective_wake ? "wake_accepted" : "stored_passive", held_reason }` to `cito.ack.<signal_id>`, calls `msg.ack()`, refreshes the Pi badge for both waking and passive admissions, and triggers the corresponding waking/passive `cito-banner`.
   - **Stage 2 (`DISPATCHING` $\to$ `DISPATCHED` with Immutable Batch Membership & Whole-Batch Expiry)**:
     1. When the `1500ms` debounce timer fires (or on startup recovery), the dispatcher first expires stale rows (`UPDATE local_signals SET state = 'EXPIRED_UNSENT' WHERE state IN ('RECEIVED', 'DISPATCHING') AND expires_at_ms <= Date.now()`), then acquires the SQLite listener row lease (`dispatcher_token = uuidv4()`, `lease_until_ms = Date.now() + 30_000`) and verifies `local_listeners.status == 'ACTIVE' AND owner_token == my_owner_token`.
     2. **Retry Existing Frozen Batch First (All-or-Nothing Batch Expiry)**: If rows already exist in `state = 'DISPATCHING'` for `(listener_id, binding_generation)`, `cito-mcp` checks the frozen batch's deadline `batch_expires_at_ms = MIN(expires_at_ms)` across all members of that `batch_id`. If `Date.now() >= batch_expires_at_ms`, the **entire** frozen batch is atomically transitioned to `'EXPIRED_UNSENT'`; otherwise `cito-mcp` retries that exact `batch_id` with its original immutable membership and card content.
     3. **Freeze New Batch**: Otherwise, selects all current `state = 'RECEIVED' AND acknowledged_at_ms IS NULL AND expires_at_ms > Date.now()` rows for `(listener_id, binding_generation)`, computes `batch_id = "batch_${listener_id}_${min_seq}_${max_seq}_${binding_generation}"` and `batch_expires_at_ms = MIN(expires_at_ms)`, and atomically updates those rows to `state = 'DISPATCHING', batch_id = ?, batch_expires_at_ms = ?`.
     4. **Lock-Serialized Pre-Effect Gate (`assertDispatchStillAuthorized` + `listen-${listener_id}.lock`)**: Each external host-effect step (Pi socket `deliver`, `codex queue`, AND `open -g "codex://threads/wake/<id>"`) acquires the per-listener file lock `flock("${CITO_STATE_DIR}/sockets/listen-${listener_id}.lock")` (the exact same lock required by `cito_listen` takeover and `cito_unlisten` revocation), checks `!abortSignal.aborted`, confirms in-memory KV watcher state has not been fenced, queries SQLite to confirm `local_listeners.status == 'ACTIVE' AND binding_generation == batch.binding_generation AND owner_token == my_owner_token AND dispatcher_token == my_token` **and** `Date.now() < batch_expires_at_ms` **and** at least one row in `batch_id` has `acknowledged_at_ms IS NULL` (extending `lease_until_ms = Date.now() + 30_000`), and holds `listen-${listener_id}.lock` until that step's command/socket submission completes. Because takeover and revocation must also acquire `listen-${listener_id}.lock`, no takeover can interleave between `assertDispatchStillAuthorized` and host-effect submission.
     5. **Driver Execution & Crash-Window Semantics**:
        - **Pi**: Sends `deliver` with `batch_id` (`3000ms` per-attempt socket timeout). On `{"ok": true}`, commits `UPDATE local_signals SET state = 'DISPATCHED' WHERE batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?`. On `{"ok": false, "error": "SESSION_MISMATCH"}`, marks listener `'UNLISTENED'`, moves the batch to `'STORED_PASSIVE'` (`held_reason = 'session_switched'`), and updates NATS KV. On socket timeout/error, retries with backoff (`1s, 2s`, up to 3 total attempts; if all 3 fail, moves to `'STORED_PASSIVE'` with `held_reason = 'driver_unavailable'`).
        - **Codex Desktop (Documented At-Least-Once / Best-Effort Timeout Boundary)**: Runs `codex queue --thread <id> --message "<card>"` (`10_000ms` timeout, bound to `abortSignal`).
          - If `codex queue` fails to spawn or exits non-zero (definite pre-insertion failure): retries `codex queue` with backoff (`1s, 2s`, up to 3 total attempts, calling `assertDispatchStillAuthorized` before each attempt); if all 3 fail, transitions the batch to `'STORED_PASSIVE'` (`held_reason = 'driver_unavailable'`).
          - If `codex queue` succeeds OR times out at `10_000ms` (unknown insertion outcome): commits `queue_written = 1` in SQLite, calls `assertDispatchStillAuthorized`, and invokes `open -g "codex://threads/wake/<id>"` (`3000ms` timeout). If `open -g` fails, retries **only `open -g`** once after `500ms` (re-checking `assertDispatchStillAuthorized`), then commits `UPDATE local_signals SET state = 'DISPATCHED' WHERE batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?`. (Supported boundary: if `codex queue` timed out before inserting, `open -g` is a harmless no-op and the message remains readable in `cito_inbox`; if `cito-mcp` crashes after `codex queue` or `open -g` before SQLite commit, recovery may insert a duplicate card identifiable by `batch_id` or re-invoke `open -g` idempotently).

---

## 6. Recipient-Side Anti-Loop Protection & Human-Turn Reset Pathway

Because the recipient's `cito-mcp` owns the local wake drivers (`codex queue` and Pi `deliver`) and directly observes local human interactions, wake quota and pairwise circuit breaking are enforced deterministically in the recipient's `~/.cito/state.sqlite` during Stage-1 signal admission:

### 6.1 Persistent Unattended Wake Quota (`max_unattended_wakes = 3`)
- Stored durably in `~/.cito/state.sqlite` (`local_listeners.unattended_wake_count`, mirrored to NATS KV `cito_listeners.<listener_id>.unattended_wakes_remaining`) per `SessionKey`.
- **No Time-Based or Re-Enrollment Bypass**: `unattended_wake_count` does **not** reset when calling `cito_unlisten` $\to$ `cito_listen`, when updating `about`, or when a 30-minute offline window expires. Once a `SessionKey` has been enrolled, its `unattended_wake_count` resets to `0` **only** via a verified monotonic `human_event_seq` advancement (Section 6.2).
- Wakes 1, 2, and 3 are admitted (`effective_wake = true`). Starting with the **4th** consecutive wake without an intervening verified human event, the recipient `cito-mcp` forces `effective_wake = false` (`status: "stored_passive"`, `held_reason: "wake_quota_exhausted"`).

### 6.2 Clock-Independent Restart-Safe Human-Event Watermark (`human_event_seq`)
Each listener tracks `human_event_seq` and `last_consumed_human_seq` (monotonic integers starting at `0` in `local_listeners`, never relying on `Date.now()`):

1. **In Pi (`cito-bridge.ts` — Single-Flight Epoch-Delta Counter with Retired-Epoch Rejection)**:
   - On startup, `cito-bridge.ts` generates a unique in-memory `bridge_epoch = uuidv4()` and initializes a per-session integer `human_event_count = 0`.
   - When `pi.on("input", (event) => ...)` fires with `event.source === "interactive"` (an actual human keyboard submission in the TUI, as opposed to `source === "extension"` from `pi.sendMessage`), `cito-bridge.ts` increments `human_event_count += 1` and returns `{ bridge_epoch, human_event_count }` in `identify` responses.
   - `identify` probes for a given `listener_id` are executed strictly single-flight by the current owner (`WHERE owner_token = my_owner_token`), and `cito-mcp` persists `(last_bridge_epoch, retired_bridge_epochs_json, last_bridge_count, human_event_seq, last_consumed_human_seq)` in `local_listeners` (`~/.cito/state.sqlite`):
     - If `bridge_epoch` is present in `retired_bridge_epochs_json` (a delayed out-of-order response from a previous epoch), the observation is ignored as stale.
     - When `bridge_epoch === last_bridge_epoch` and `human_event_count > last_bridge_count`: `cito-mcp` atomically increments `human_event_seq += (human_event_count - last_bridge_count)` and sets `last_bridge_count = human_event_count`.
     - When `bridge_epoch !== last_bridge_epoch` and `bridge_epoch` is not in `retired_bridge_epochs_json` (Pi restarted and resumed the session): `cito-mcp` appends the old `last_bridge_epoch` to `retired_bridge_epochs_json` (bounded to the last 32 epochs), sets `last_bridge_epoch = bridge_epoch`, `last_bridge_count = human_event_count`, and if `human_event_count > 0`, increments `human_event_seq += human_event_count`. (Merely restarting with `human_event_count == 0` does not increment `human_event_seq`; the first interactive input after restart produces `human_event_count == 1`, which strictly increments the SQLite-persisted `human_event_seq` regardless of wall-clock skew).
2. **In Codex Desktop App (SQLite Atomic Increment on Banner Click)**:
   - Elapsed time after a Cito wake is **never** used to infer human activity.
   - When the user clicks the `cito-banner` for `thread_id`, `cito-banner` atomically executes `UPDATE local_listeners SET human_event_seq = human_event_seq + 1 WHERE harness = 'codex' AND session_id = ?` in `~/.cito/state.sqlite` before opening `codex://threads/<thread_id>`.
3. **Monotonic Local Consumption**:
   - During the 20s liveness sweep, `cito_listen`, and Stage-1 signal admission, `cito-mcp` checks `human_event_seq > last_consumed_human_seq`. When true, it atomically sets `last_consumed_human_seq = human_event_seq`, resets `unattended_wake_count = 0`, and clears `local_pairwise_wakes` for that listener in `state.sqlite` (and updates `unattended_wakes_remaining: 3` in NATS KV).

### 6.3 Recipient-Local Pairwise Inbound Circuit Breaker & Suppression Precedence
- To eliminate cross-host clock skew and distributed reservation races, the pairwise circuit breaker is defined strictly over **recipient-admitted inbound waking signals** per `(listener_id, sender_endpoint_key)` in `local_pairwise_wakes(listener_id, sender_endpoint_key, admitted_at_ms)` using the recipient's monotonic clock.
- **Exact Threshold**: Up to **2 admitted inbound waking signals** from the same `sender_endpoint_key` to `listener_id` within a rolling **120-second** window are permitted (which bounds a 2-round-trip `A <-> B` exchange on each side independently). On the **3rd** requested wake from that same `sender_endpoint_key` within 120 seconds, `cito-mcp` forces `effective_wake = false` (`held_reason = "pairwise_rate_limited"`).
- **Deterministic `held_reason` Precedence**:
  1. `"stale_generation"` (`signal.target_generation < local_listener.binding_generation`)
  2. `"passive_requested"` (`signal.requested_wake === false`)
  3. `"peer_wake_disabled"` (`signal.from.principal_id !== target.principal_id && !target.allow_peer_wake`)
  4. `"wake_quota_exhausted"` (`target.unattended_wake_count >= 3`)
  5. `"pairwise_rate_limited"` (admitted inbound waking count from `sender_endpoint_key` in last 120s $\ge 2$)

---

## 7. Host Wake Drivers & Human Visibility (V1: Codex Desktop & Pi)

### 7.1 Passive vs. Waking Behavior Matrix

| Signal Type | Stored in `cito_inbox`? | Pi TUI Transcript (`sendMessage`) | Codex Queue + Wake URL | `cito-banner` HUD Shown? |
| :--- | :--- | :--- | :--- | :--- |
| **`effective_wake == true`** | Yes (`RECEIVED` $\to$ `DISPATCHED`) | Yes (`display: true, triggerTurn: true, deliverAs: "followUp"`) | Yes (`codex queue` + `open -g codex://threads/wake/<id>`) | **Yes** |
| **`effective_wake == false`** | Yes (`STORED_PASSIVE`) | **No** (status bar updates to `🔔 <slug> (N)`) | **No** | **Yes** (cyan inbox badge / `仅知会` pill) |

### 7.2 Lock-Synchronized Single-Instance `cito-banner` Lifecycle

The HUD is a 388×136pt nonactivating vibrant-dark panel with continuous 16pt corners, a 1pt white border (alpha 0.14, 0.28 on hover), and an initial +20pt-right slide-in over 0.20s ease-out. Waking signals use amber `#FF9F0A`, a 3.5pt left accent, 32×32pt `bell.badge.fill` badge and `唤醒` pill; passive signals use cyan `#64D2FF`, `tray.and.arrow.down.fill` and `仅知会`. While visible, `displayedCount` adds `count ?? 1` only for the exact `(target_harness, target_session_id, effective_wake)` tuple; a tuple change resets the count. Counts above one append ` · ×N` to the pill.

Presentation removes trailing `.local` from hosts, omits empty-title separators, and collapses message newline runs with surrounding whitespace to two spaces. The original `value.subtitle` and `value.message` remain unchanged in headless JSON diagnostics; separate display fields expose cleaned text and tuple counts. Codex shows the blue action `点击跳转至 Codex 对话 ↗`; Pi shows the tertiary action `Pi 终端会话 · 点击关闭横幅`. Labels are nonselectable; the root click view accepts the first mouse and owns hit testing, with a tracking area for the pointing-hand cursor and hover border.

To guarantee that two `cito-banner` windows/processes can never coexist and no notification is dropped during startup or retirement, coordination uses two locks (`banner.lock` for sender serialization and `banner-instance.lock` held by the running `cito-banner` process for its entire lifetime):

1. **Process-Lifetime Instance Lock (`banner-instance.lock`)**:
   - Immediately upon launch, `cito-banner` acquires an exclusive `flock` on `${CITO_STATE_DIR}/sockets/banner-instance.lock` (waiting up to `2000ms` if a retiring instance is finishing its `250ms` fade-out) and holds that file descriptor open until `exit(0)` (when the OS kernel automatically releases the lock).
2. **Sender Protocol (`cito-mcp` — Non-Blocking Instance Probe to Prevent Lock Cycles)**:
   - Acquires exclusive blocking `flock` on `${CITO_STATE_DIR}/sockets/banner.lock` (timeout `2500ms`).
   - While holding `banner.lock`, attempts to connect to `${CITO_STATE_DIR}/sockets/banner.sock` (`300ms` timeout), writes `JSON.stringify(payload) + "\n"`, and awaits `{"ok":true}\n` (`500ms` timeout).
   - If connect or read/write fails, while **still holding `banner.lock`**, `cito-mcp` tests `${CITO_STATE_DIR}/sockets/banner-instance.lock` with a **non-blocking** `flock(LOCK_EX | LOCK_NB)`:
     - **Case A (`banner-instance.lock` is busy — `EWOULDBLOCK`)**: An existing `cito-banner` process is alive (either still finishing startup or mid-retirement). `cito-mcp` does **not** block on `banner-instance.lock` while holding `banner.lock`. Instead, it polls `banner.sock` connect every `50ms` for up to `500ms`; if `banner.sock` connects, it delivers `payload` and releases `banner.lock`. If `banner.sock` remains unreachable, `cito-mcp` releases `banner.lock` for `300ms` to let the retiring instance acquire `banner.lock` and exit, then re-acquires `banner.lock`, retries connecting to `banner.sock` once (`300ms` timeout), and if still unreachable, **re-probes `banner-instance.lock` with non-blocking `flock(LOCK_EX | LOCK_NB)`**: it enters Case B **only if** `banner-instance.lock` is now free; if `banner-instance.lock` is still busy, it logs a best-effort banner skip and releases `banner.lock` without unlinking `banner.sock` or spawning a duplicate process.
     - **Case B (`banner-instance.lock` is free — acquired and immediately released by `cito-mcp`)**: Confirms zero `cito-banner` processes are running. `cito-mcp` unlinks any stale `banner.sock`, spawns `${CITO_STATE_DIR}/bin/cito-banner` detached, polls `banner.sock` readiness (every `25ms` up to `2500ms`, allowing first-launch AppKit initialization during concurrent builds), writes `payload + "\n"`, awaits `{"ok":true}\n`, and always releases `banner.lock` before returning.
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
   - Runs `execFile(CITO_CODEX_BIN, ["queue", "--thread", thread_id, "--message", formattedBatchCard], { timeout: 10000 })`.
   - Runs `execFile("/usr/bin/open", ["-g", "codex://threads/wake/" + thread_id], { timeout: 3000 })`.

### 7.4 Pi Socket Bridge Protocol (`src/pi-extension/cito-bridge.ts`)
- **Transport**: Unix domain socket `${CITO_STATE_DIR}/sockets/pi-pid-${process.pid}.sock` (mode `0600`).
- **Status badge**: `badgeSlug = slug.slice(0, 12).replace(/-+$/g, "") || harness`. Idle: `🔔 <badgeSlug>`; unread: `🔔 <badgeSlug> (N)`; replay gap: `🔔 <badgeSlug> (!)`; both: `🔔 <badgeSlug> (N!)`. Refresh on listen, restore, both waking/passive Stage-1 admissions and inbox. `ack_ids` leaves a gap warning in place; `ack_all: true` clears it. Unlisten, receiver revocation and service shutdown clear the badge only under `listen-c_<fp12>.lock` after checking SQLite `owner_token`; fenced losers never clear the replacement owner's badge. `update(ctx)` clears status immediately when the Pi session ID changes.
- **Custom renderer**: The bridge optionally registers `cito-notification` with Pi using only Node built-ins, preserving standalone installation. It applies `customMessageBg`, a bold accent `🔔 Cito` header and `borderAccent` vertical body bars, wraps wide/combining characters to terminal width, hides acknowledgement details while collapsed and shows them in `dim` when expanded. It renders both the new cards and legacy frozen `<cito_signal>` envelopes.
- **Framing**: One short-lived connection per request; one `\n`-terminated JSON request line $\to$ one `\n`-terminated JSON response line.
- **Operations**:
  1. `{"id":"1","op":"identify"}` $\to$ `{"id":"1","ok":true,"pi_pid":51204,"session_id":"01a1a1ef...","session_name":"...","cwd":"...","is_streaming":false,"bridge_epoch":"e1...","human_event_count":2}`
  2. `{"id":"2","op":"status","session_id":"01a1a1ef...","badge":"🔔 rcatwin"}`:
     - If `req.session_id !== currentSessionId`, returns `{"id":"2","ok":false,"error":"SESSION_MISMATCH"}`. Otherwise updates `ctx.ui.setStatus("cito", req.badge)` and returns `{"id":"2","ok":true}`.
  3. `{"id":"3","op":"deliver","session_id":"01a1a1ef...","batch_id":"batch_c_ab2def3gh4jk_41_42_3","content":"...","wake":true}`:
     - If `req.session_id !== currentSessionId`, returns `{"id":"3","ok":false,"error":"SESSION_MISMATCH"}`.
     - If `processedBatchIds.has(req.batch_id)` (ring buffer capacity 200), returns `{"id":"3","ok":true,"deduplicated":true}`.
     - Calls `pi.sendMessage({ customType: "cito-notification", content: req.content, display: true }, { triggerTurn: req.wake, deliverAs: "followUp" })`.
     - **Only after `pi.sendMessage` returns without throwing**, adds `req.batch_id` to `processedBatchIds` and returns `{"id":"3","ok":true,"deduplicated":false}`.

---

### 7.5 In-Chat Wake Cards

A single signal renders as:

```text
🔔 Cito · alice@mac · Review [↩ rcatwin-teams-pbtjxcxkgw3y]

Review is ready.

↳ Ack: cito_inbox(ack_ids: ["sig_..."])
```

Sender hosts lose a trailing `.local` (case-insensitive, retaining the original if stripping leaves nothing). A trimmed nonempty sender title follows ` · ` unless its normalized slug equals the public sender ID's slug prefix. Bound senders add `[↩ <public_id>]`; unbound senders omit the reply hint. All interpolated text and acknowledgement JSON escape angle brackets at rendering time; stored text remains plain.

Multiple signals use `🔔 Cito · N 条唤醒消息` plus ` · <target title>` when nonempty, followed by blank-line-separated `[1] <sender>\n<message>` entries in sequence order and one shared acknowledgement footer. Persisted frozen legacy cards retain their original bytes during recovery.

## 8. Optional Pure-OIDC Authentication via NATS Auth Callout (GCP Casdoor)

### 8.1 NATS Deployment & Auth Callout Service (`deploy/nats/` & `src/auth-callout/`)
- **Threat-Model Boundary**: Following the Cooperative Workspace Threat Model, authenticated workspace peers connect to the shared `CITO` NATS account. Connection-level identity is authenticated by `cito-auth-callout` (in `"oidc"` mode), while application-level `scope: "self" | "workspace"` filtering and signal attribution are enforced by `cito-mcp`.
- **NATS Account Bootstrap (`deploy/nats/nats-server.conf`)**:
  - In `CITO_AUTH_MODE="none"`: `nats-server` runs without `authorization` blocks; `cito-mcp` connects directly and idempotently creates/verifies the `cito_listeners` KV bucket and `CITO_SIGNALS` stream on startup.
  - In `CITO_AUTH_MODE="oidc"`: `nats-server.conf` defines the `$SYS` account, the `CITO` application account (with JetStream enabled), and an `authorization { auth_callout { issuer: <pubkey_of_CITO_AUTH_CALLOUT_ISSUER_SEED>, auth_users: ["callout-service"], account: "CITO" } }` block.
  - The `cito-auth-callout` service connects using the exempted `callout-service` user credentials, idempotently provisions the `cito_listeners` KV bucket and `CITO_SIGNALS` stream on startup, and subscribes to `$SYS.REQ.USER.AUTH`. (When `CITO_AUTH_CALLOUT_XKEY_SEED` is set, matches the `xkey` public key configured in `auth_callout`).
- **OIDC Verification**:
  - Fetches `${CITO_OIDC_ISSUER}/.well-known/openid-configuration` (`5000ms` timeout) and caches JWKS keys from `jwks_uri` (refreshing on unknown `kid` with a 60s cooldown; if JWKS fetch fails on cache miss, returns an Auth Callout transient error).
  - When a client connects passing `token: "<id_token_jwt>"`:
    1. Verifies JWT signature (`RS256` / `ES256`), `iss === CITO_OIDC_ISSUER`, `exp > now - 30`, and `aud` contains `CITO_OIDC_CLIENT_ID`.
    2. Signs a NATS User Claims JWT (`jwt` v2, `aud: "CITO"`, `exp: claims.exp`) granting publish and subscribe permissions in the `CITO` account (`["$JS.API.>", "$JS.ACK.>", "$KV.cito_listeners.>", "cito.signal.>", "cito.ack.>", "_INBOX.>"]`).
    3. On verification failure, returns an Auth Callout rejection so NATS Server denies the connection with `Authorization Violation`.

### 8.2 Rejection-Aware Double-Checked JIT Login in `cito-mcp`
- **Canonical Token Selection (`id_token`), Local Cryptographic Verification & Cache (`${CITO_STATE_DIR}/oidc-token.json`, mode `0600`)**:
  - `cito-mcp` always uses the OIDC **`id_token`** as the NATS connection token.
  - Before writing or trusting `id_token` claims locally (including for offline `cito_inbox` identity derivation), `cito-mcp` cryptographically verifies the `id_token` signature (`RS256` / `ES256`) against `${CITO_OIDC_ISSUER}/.well-known/openid-configuration` `jwks_uri` and validates `iss === CITO_OIDC_ISSUER` and `aud` contains `CITO_OIDC_CLIENT_ID`, persisting the verified `principal_id` and `owner_user` alongside `{ issuer, client_id, id_token, refresh_token, token_sha256, expires_at, verified_principal_id, verified_user }` in `oidc-token.json` (`0600`). (Offline `cito_inbox` reads `verified_principal_id` from the previously signature-verified `oidc-token.json`).
- **Recovery Flow on Missing/Expired Token or NATS `Authorization Violation`**:
  1. Record `rejected_sha256 = used_token_sha256` (or `null`) and set `deadline = monotonic_now() + 45_000`.
  2. Acquire exclusive `flock` on `${CITO_STATE_DIR}/oidc-login.lock` (polling every `100ms` until `deadline`; on timeout, fail with `OIDC_LOGIN_TIMEOUT`).
  3. **Rejection-Aware Double-Check**: Re-read `oidc-token.json`. Reuse the cached token **only if** `issuer === CITO_OIDC_ISSUER`, `client_id === CITO_OIDC_CLIENT_ID`, `expires_at > now + 60`, **and `token_sha256 !== rejected_sha256`**.
  4. **Silent Refresh (Deadline-Bounded)**: If `refresh_token` is present, POST `grant_type=refresh_token` to `token_endpoint` with timeout `min(10_000, deadline - monotonic_now())`.
     - If `token_endpoint` returns `200 OK` with a new `id_token`, cryptographically verify its JWKS signature, atomically write `oidc-token.json`, release lock, and retry the NATS operation **at most once**.
     - If `token_endpoint` returns `400` / `401` (refresh token expired/revoked), immediately fall through to Step 5 (Interactive Browser PKCE) within the same lock and remaining `deadline`.
     - If `token_endpoint` fails with a network/5xx error, release lock and fail with `AUTH_UNAVAILABLE` (`retryable: true`) without opening a browser.
  5. **Interactive Browser PKCE**: Bind `http://127.0.0.1:0/callback`, generate PKCE `S256` challenge and `state`, open the browser via `/usr/bin/open`, exchange `code` at `token_endpoint` before `deadline`, cryptographically verify the returned `id_token` against `jwks_uri`, atomically write `oidc-token.json`, release lock, and retry the NATS operation **at most once**.

---

## 9. Deferred Scope: Claude Code Adapter (Proposed Design — TODO / Unverified)

> **Status Notice (Out of V1 Scope)**: Claude Code is not installed in the current environment. The design below is recorded strictly as a **Proposed Specification (Unverified in Target Runtime)** for future implementation by a contributor with a Claude Code environment. It is **excluded** from the V1 codebase and Vectl plan.

1. **Proposed Identity Resolution (`SessionStart` Hook)**:
   - Configure a `SessionStart` hook in `~/.claude/settings.json` that reads `{ session_id, transcript_path, cwd }` from stdin JSON and writes `~/.cito/claude-pids/<PPID>.json`.
2. **Proposed Idle Wake Mechanism (`Stop` Hook with `asyncRewake: true`)**:
   - Configure a background `Stop` hook with `"asyncRewake": true` that waits on `~/.cito/sockets/claude-<session_id>.sock` and exits with code `2` (writing the notification card to `stderr`) when an `effective_wake == true` signal arrives.

---

## 10. Verification & Acceptance Criteria (V1)

1. **NATS JetStream, Identity, Lifecycle, and Anti-Loop Contract Tests**:
   - Deterministic canonical IDs (`^c_[a-z2-7]{12}$`) remain storage/routing keys while public IDs use `<slug>-<fp12>`. Verify normalization and derivation order, explicit-tag persistence, SQLite migration, tuple collision rejection before local mutations, canonical/public/renamed-alias idempotency equivalence and public response IDs. Repeating `cito_listen` increments generation, preserves omitted preferences and does **not** reset unattended quota.
   - Persistent wake quota & clock-independent `human_event_seq`: `unattended_wake_count` persists across `unlisten -> listen` and across 30-minute expiry; Pi `bridge_epoch` + `human_event_count` delta increments `human_event_seq` strictly on interactive events across restarts without relying on `Date.now()`.
   - Recipient-local pairwise inbound circuit breaker: 2 admitted inbound waking signals from the same `SenderEndpointKey` to `listener_id` within 120s succeed (`effective_wake: true`); the **3rd** inbound waking signal from that sender within 120s is downgraded to `effective_wake: false` with `held_reason: "pairwise_rate_limited"`.
   - Atomic Stage-1 redelivery idempotency: redelivering an already-persisted `signal_id` reuses the stored `(effective_wake, held_reason)` without double-charging wake quota or pairwise counters.
   - Lock-serialized takeover, owner-qualified fencing, whole-batch expiry, and mid-flight abort: `listen-${listener_id}.lock` + KV CAS prevents local/KV split-brain on failed CAS; losing process (`owner_token != winner`) never clobbers the winning process's `ACTIVE` row; calling `cito_unlisten` or detecting KV generation fencing transitions both `RECEIVED` and `DISPATCHING` rows to `STORED_PASSIVE`, aborts any in-flight driver step before `open -g` wake submission, and preserves `acknowledged_at_ms` independence from `DISPATCHED` completion.
   - Listener-specific offline gap detection: re-attaching after `broker_age_ms > 1_800_000` sets `replay_gap_detected = 1`.
2. **OIDC Auth Callout & Security Tests**:
   - Canonical `owner_user` & `principal_id` extraction, NATS Auth Callout JWT verification (including `$JS.ACK.>` permission) against a mock JWKS server, local `id_token` JWKS verification, and cross-user `FORBIDDEN_TARGET_SCOPE` enforcement.
   - Rejection-aware double-checked `flock`: when an unexpired token is rejected, the lock holder does not reuse the same `token_sha256`, refreshes/re-authenticates once, and concurrent waiters reuse the newly written token.
3. **Pi & Codex Desktop Adapter Tests**:
   - Target-driven outbound routing: unbound callers invoke `cito_find` and `cito_send` without `BINDING_REQUIRED`.
   - Pi process-tree ancestor walk resolves `pi-pid-<ancestor_pid>.sock`, handles `identify`, `status`, continuous 20s liveness sweep (revoking on `session_id` change), and `deliver` with frozen `batch_id` retry idempotency.
   - Codex adapter extracts `_meta["x-codex-turn-metadata"].thread_id`, verifies tab liveness over `ipc.sock` (`thread-owner-discovery`), and invokes `codex queue` + `open -g codex://threads/wake/<thread_id>`.
4. **`cito-banner` Lock-Synchronized Lifecycle Tests**:
   - Concurrent cold-start and retirement-boundary banner invocations coordinate via `banner.lock` + `banner-instance.lock` + `banner.sock` without losing notifications or overlapping processes.
