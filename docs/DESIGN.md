# Cito Architecture & Protocol Specification (V2 — Multi-Hub & Extensible Harness Edition)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a notification and signaling bus that wakes or delivers messages to opted-in coding agent conversations across:
1. **Multiple NATS Hubs** (e.g., a personal Tailscale no-auth hub `personal` and a corporate GCP Casdoor OIDC hub `rda` simultaneously),
2. **Users** (e.g., `tefx`, colleague `mingchen`),
3. **Hosts / Machines** (e.g., `mbp-work`, `mbp-personal`),
4. **Extensible Agent Harnesses** (shipped: **Pi Coding Agent** and **ChatGPT / Codex Desktop App**; open plug-in architecture for **Claude Code** and future CLI/IDE harnesses), and
5. **Concurrent Conversations** within the same repository or working directory.

Cito follows a **"Direct Notification & Follow-Up"** model: every message sent via `cito_send` to an opted-in listener (`cito_listen`) is reliably delivered to the target conversation (waking an idle session immediately or queuing into a running session's follow-up queue, with `cito_inbox` as the pull fallback when a target driver is unconfirmed or unavailable).

### 1.2 Implementation Scope vs. Proposed/Deferred Scope
- **In Scope for Implementation & Verification (inside `/Users/tefx/Projects/cito`)**:
  1. **Native Multi-Hub Support (`src/shared/config.ts` & `src/mcp/service.ts`)**:
     - Zero dependency on external gateways (such as Tela). A single `cito-mcp` stdio server manages one or more configured `HubSlot`s from optional `~/.cito/config.json` (or `CITO_*` environment variables for a single `"default"` hub), isolating per-hub NATS connections, OIDC PKCE tokens (`oidc-token-<hub>.json`), and receivers while sharing a single local SQLite database (`~/.cito/state.sqlite`, `PRAGMA user_version = 4`).
     - Symmetric `<hub>:<id>` scoped addressing across all 5 MCP tools (`cito_listen`, `cito_unlisten`, `cito_find`, `cito_send`, `cito_inbox`) and Wake Cards when multiple or named hubs are configured, while keeping NATS wire payloads unprefixed.
  2. **Extensible Harness Adapter Architecture (`src/adapters/` & `src/mcp/`)**:
     - `src/mcp/` (`service.ts`, `receiver.ts`, `state.ts`, `oidc.ts`, `server.ts`, `cli.ts`) is 100% harness-agnostic and contains zero hardcoded checks for `"pi"` or `"codex"`.
     - `harness` is an open identifier (`^[a-z][a-z0-9_-]{0,31}$`) across schemas, SQLite, and `cito-banner.swift`, never a closed hardcoded enum.
     - Ships two built-in adapter classes behind the 3-method `HarnessAdapter` interface (`probe`, `deliver`, `updateBadge?`):
       - `SocketBridgeAdapter(harnessId)`: generic local Unix domain socket driver (`${stateDir}/sockets/<harness>-pid-<pid>.sock`) used by **Pi** (`pi`) and automatically discoverable for any future socket-bridged harness without modifying `src/mcp/`.
       - `CodexDesktopAdapter`: external SQLite/IPC/URL-scheme driver for **ChatGPT / Codex Desktop (`ChatGPT.app`)** (`codex`).
  3. **Zero-Wake-Parameter & Zero-Quota Streamlined Delivery**:
     - Removes `wake?: boolean` from `cito_send` and `allow_peer_wake?: boolean` from `cito_listen` (access control is governed solely by `scope: "self" | "workspace"`).
     - Removes the 6-column `unattended_wake_count` / `bridge_epoch` / `human_event_seq` state machine and `local_pairwise_wakes` table so multi-turn agent collaboration is never artificially throttled, and removes `import SQLite3` from `src/macos/cito-banner.swift`.
     - Auto-acknowledges confirmed deliveries (`Adapter.deliver` returning `"delivered"`) and removes the `↳ Ack: cito_inbox(...)` footer from Wake Cards, while preserving `cito_inbox` (defaulting to drain-on-read when `ack_ids` is omitted) for unconfirmed or driver-unavailable signals.
- **Proposed Design Only (Not Yet Implemented)**:
  - **Claude Code (`claude`)**: Documented in **Section 9** strictly as a **设计建议（仅建议，尚未实现 / Proposed Design Suggestion Only — Not Yet Implemented）**. `README.md` includes per-harness setup and usage guides for Pi, Codex Desktop, and future harnesses, explicitly noting that Claude Code support is not yet implemented.

### 1.3 Non-Goals
- **No External Gateway Dependency**: Cito never requires Tela or multiple MCP server entries to connect to multiple hubs.
- **No Implicit Session Scanning**: Conversations remain invisible until they explicitly call `cito_listen`.
- **No Legacy Schema Migration Shims**: `LocalState.open()` enforces `PRAGMA user_version = 4` and atomically drops and recreates tables if an older local database version is encountered.

---

## 2. System Topology, Multi-Hub Configuration, and Module Boundaries

### 2.1 Acyclic Module Dependency Graph
`src/shared/` $\to$ `src/adapters/` $\to$ `src/mcp/`

- **`src/shared/`**: Configuration (`config.ts`), open `harnessIdSchema` (`^[a-z][a-z0-9_-]{0,31}$`) and tool input schemas (`validation.ts`), deterministic identity & `<hub>:` scoped ID helpers (`identity.ts`), NATS provisioning (`nats.ts`), and error vocabulary (`errors.ts`).
- **`src/adapters/`**: `HarnessAdapter` interface (`types.ts`), `SocketBridgeAdapter` (`pi.ts`), `CodexDesktopAdapter` (`codex.ts`), priority-free `resolveCaller` and extensible `ADAPTER_REGISTRY` (`index.ts`), Git metadata helper (`git.ts`), and clean `formatWakeCard` (`wake-card.ts`).
- **`src/mcp/`**: Harness-agnostic `LocalState` (`state.ts`), `Receiver` (`receiver.ts`), multi-hub `CitoService` (`service.ts`), per-hub `OidcLogin` (`oidc.ts`), MCP stdio server (`server.ts`), and CLI entrypoint (`cli.ts`).

### 2.2 Multi-Hub Configuration (`~/.cito/config.json` & Environment Variables)

```ts
export interface HubConfig {
  name: string;        // Matches ^[a-z0-9][a-z0-9_-]{0,31}$
  natsUrl: string;
  authMode: "none" | "oidc";
  user: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  oidcScopes?: string[];
}

export interface CitoConfig {
  stateDir: string;
  codexHome: string;
  codexBin: string;
  openBin: string;
  hostName: string;
  defaultHub: string;
  hubs: Record<string, HubConfig>;
  // Mirrors hubs[defaultHub] for AuthCalloutConfig compatibility:
  natsUrl: string;
  authMode: "none" | "oidc";
  user: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  oidcScopes?: string[];
}
```

- **Resolution & Validation (`loadConfig(env)`)**:
  1. `stateDir = directory(env.CITO_STATE_DIR ?? "~/.cito")`.
  2. When `env === process.env || env.CITO_STATE_DIR !== undefined || env.CITO_CONFIG_PATH !== undefined`, checks for `env.CITO_CONFIG_PATH ?? join(stateDir, "config.json")`:
     - If present: parses `hubs` (must be a non-empty object whose keys match `^[a-z0-9][a-z0-9_-]{0,31}$`). Each entry defaults `authMode` to `"none"` and `user` to `env.CITO_USER ?? osUser` (requiring `oidcIssuer` and `oidcClientId` when `authMode === "oidc"`).
     - If absent: synthesizes a single hub `"default"` from `CITO_NATS_URL`, `CITO_AUTH_MODE`, `CITO_USER`, `CITO_OIDC_ISSUER`, `CITO_OIDC_CLIENT_ID`, and `CITO_OIDC_SCOPES`.
  3. `defaultHub` precedence: `env.CITO_HUB?.trim() || configJson?.default_hub || (Object.keys(hubs).length === 1 ? Object.keys(hubs)[0] : undefined)`. If `!defaultHub || !(defaultHub in hubs)`, throws `CitoError("INVALID_INPUT", "default_hub must be specified and exist in configured hubs")`.
  4. If `env.CITO_NATS_URL` is explicitly set when `CITO_CONFIG_PATH` is unset, it overrides `hubs[defaultHub].natsUrl`.

---

## 3. Identity, Symmetric `<hub>:` Addressing, and Extensible Caller Resolution

### 3.1 Canonical Storage Identity and Public Display IDs
- Each listener registration on a hub is keyed by `SessionKey = (principal_id, host_id, harness, session_id)` where `harness` matches `harnessIdSchema` (`^[a-z][a-z0-9_-]{0,31}$`).
- Internal canonical ID: `canonical_id = "c_" + fp12` (`^c_[a-z2-7]{12}$`).
- Unscoped public ID: `public_id = "${slug}-${fp12}"` (`^[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12}$`).

### 3.2 Symmetric `<hub>:` Scoped Addressing (`formatScopedId` & `parseScopedId`)
- **NATS Wire Invariant**: All IDs stored in NATS KV (`cito_listeners`), published in JetStream (`CITO_SIGNALS`), or stored in SQLite columns (`listener_id`, `public_id`, `signal_id`, `from_listener_id`, `in_reply_to`) are **unprefixed**.
- **`isSingleDefaultHub(config)`**: `Object.keys(config.hubs).length === 1 && config.defaultHub === "default"`.
- **`formatScopedId(hub, id, config)`**: Returns `id` when `isSingleDefaultHub(config)` is `true`, and `${hub}:${id}` otherwise. Applied to all public MCP outputs (`cito_listen.listener_id`, `cito_unlisten.listener_id`, `cito_find.caller.listener_id`, `cito_find.listeners[].listener_id`, `cito_send.to`, `cito_send.signal_id`, `cito_inbox.listener_id`, `cito_inbox.signals[].signal_id`, `cito_inbox.signals[].from.listener_id`) and to `from_listener_id` when rendering `[↩ ...]` in `formatWakeCard` (which strips optional `^(?:[a-z0-9][a-z0-9_-]{0,31}:)?` before comparing sender `slug` to `from_title`).
- **`parseScopedId(raw, config, explicitHub?)`**: Parses optional `<prefixHub>:<rest>`. Validates that any `<prefixHub>` and any `explicitHub` exist in `config.hubs` and do not conflict with each other (throwing `INVALID_INPUT` prior to any network/DB activity on unknown or conflicting hubs). Returns `{ hub: prefixHub ?? explicitHub ?? config.defaultHub, id: rest }`.

### 3.3 Priority-Free, Extensible Caller Resolution (`resolveCaller`)
- **Step 1 — Explicit Claim Validation**:
  - Extracts explicit Codex thread claims (`x-codex-turn-metadata.thread_id` / `session_id`, `openai/threadId`, `codexThreadId`; multiple distinct values $\to$ `INVALID_INPUT`) and top-level explicit bridge PID claims (`meta.endpoint_pid`, `meta.caller_pid`; non-positive/non-safe-integer $\to$ `PI_BRIDGE_UNAVAILABLE`, two distinct values $\to$ `INVALID_INPUT`).
  - If both an explicit Codex claim and a top-level explicit bridge PID claim are present, throws `CitoError("INVALID_INPUT", "Conflicting caller metadata")` (`outbound()` catches only `BINDING_REQUIRED` and `SESSION_NOT_OPEN`, always rethrowing `INVALID_INPUT`).
- **Step 2 — Resolution Execution**:
  1. **Codex claim present**: When `requireOpen !== false`, calls `readCodexThread` + `assertCodexThreadOpen`; when `requireOpen === false`, tries `readCodexThread(id, config, { allowArchived: true })` and on `SESSION_NOT_OPEN` falls back to `lookupCachedSession?.({ harness: "codex", sessionId: id })`.
  2. **Socket Bridge Discovery (Extensible to Any `<harness>-pid-<pid>.sock`)**:
     - Shared helper `extractCallerPidRoots(meta)` returns `[explicitPid]` if a top-level `endpoint_pid` / `caller_pid` is present; else collects all positive safe integers from nested `endpoint_pid`, `caller_pid`, and `parent_pid` fields in `meta` (walking ONLY those roots `depth = 0..5`, never `process.ppid`); else walks `[process.ppid]` (`depth = 0..5`).
     - At each hop `pid`, scans `${stateDir}/sockets/` for any socket matching `^([a-z][a-z0-9_-]{0,31})-pid-${pid}\.sock$` (checking registered `SocketBridgeAdapter` instances first, and dynamically supporting any `<harness>-pid-${pid>.sock`), then queries `{ op: "identify" }` (`1500ms` timeout) returning `{ ok: true, endpoint_pid: pid, session_id, session_name, cwd }` and enriches via `gitMetadata(cwd, session_name)` (with `git -C` stripping inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_COMMON_DIR`).
     - **Unambiguous Offline PID Fallback (`requireOpen === false`)**: When a socket probe fails and `requireOpen === false`, `lookupCachedSession?.({ endpointPid: pid })` checks `SELECT DISTINCT harness, session_id FROM local_listeners WHERE endpoint_pid = ?` across **all retained rows** (regardless of `status`). If and only if `distinctRows.length === 1`, it returns the cached session metadata; if `0` or `>1` distinct `(harness, session_id)` pairs exist for that PID, it throws `PI_BRIDGE_UNAVAILABLE` (never guessing by `binding_generation` or status).

---

## 4. Extensible `HarnessAdapter` Contract & Built-In Drivers (`src/adapters/`)

```ts
export type ProbeOutcome =
  | { status: "alive" | "switched" | "closed_definitive" }
  | { status: "transient_error"; error: unknown };

export type DeliverOutcome =
  | "delivered"              // Confirmed insertion into host conversation -> auto-sets acknowledged_at_ms
  | "delivered_unconfirmed"  // Queued with indeterminate timeout or unconfirmed channel -> DISPATCHED, keeps acknowledged_at_ms NULL
  | "driver_unavailable"     // Host driver unreachable -> STORED_PASSIVE (held_reason = 'driver_unavailable')
  | "session_switched";      // Host switched session -> revokes all active hub bindings for (harness, session_id)

export interface DeliverContext {
  batchId: string;
  card: string;
  queueWritten: boolean;
  markQueueWritten: () => void;
  runEffect: (
    submit: () => Promise<void>,
    opts: { attempts: number; pause: number[]; onSigtermTimeout?: () => void }
  ) => Promise<boolean>;
  config: CitoConfig;
  signal: AbortSignal;
}

export interface HarnessAdapter {
  readonly id: string;
  probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome>;
  deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome>;
  updateBadge?(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void>;
}
```

- **Extensible Registry (`ADAPTER_REGISTRY`)**:
  - Registers built-in `CodexDesktopAdapter` (`id: "codex"`) and `new SocketBridgeAdapter("pi")` (`id: "pi"`), exports `registerAdapter(adapter: HarnessAdapter)` for plug-in extension, and provides `getAdapter(harness: string): HarnessAdapter | undefined` (which returns the registered adapter or dynamically creates a `SocketBridgeAdapter(harness)` when a corresponding socket bridge is used).
- **`SocketBridgeAdapter(harnessId, { confirmDelivery = true })`**:
  - Communicates over `${stateDir}/sockets/${harnessId}-pid-${row.endpoint_pid}.sock`.
  - `probe`: calls `identify`; returns `switched` if `session_id !== row.session_id`, `closed_definitive` on `SESSION_NOT_OPEN` or `ENOENT`/`ECONNREFUSED` when `process.kill(endpoint_pid, 0)` throws `ESRCH`.
  - `deliver`: calls `{ op: "deliver", session_id: row.session_id, batch_id: ctx.batchId, wake: true, content: ctx.card }` inside `ctx.runEffect(..., { attempts: 3, pause: [1000, 2000] })`. Returns `"delivered"` when `confirmDelivery` is `true`, `"delivered_unconfirmed"` when `confirmDelivery` is `false`, `"session_switched"` on `SESSION_MISMATCH`, or `"driver_unavailable"` on exhausted retries.
- **`CodexDesktopAdapter` (`id: "codex"`)**:
  - `probe`: checks `readCodexThread` (`archived === 0`) and `assertCodexThreadOpen` (`ipc.sock`).
  - `deliver`:
    - Step 1 (`!ctx.queueWritten`): runs `queueCodexWake` (passing `CODEX_HOME: ctx.config.codexHome` in child `env`) via `ctx.runEffect` (`attempts: 3, pause: [1000, 2000]`, tracking whether completion was a clean exit `0` vs. `SIGTERM` timeout via `onSigtermTimeout`). If Step 1 returns `false`, returns `"driver_unavailable"`. Otherwise calls `ctx.markQueueWritten()`.
    - Step 2: runs `triggerCodexWake` (`openBin: ctx.config.openBin`, `attempts: 2, pause: [500]`).
    - Returns `"delivered"` **if and only if** `cleanQueueExit && triggered`; otherwise returns `"delivered_unconfirmed"` so `acknowledged_at_ms` remains `NULL` and the message stays readable in `cito_inbox`.

---

## 5. Single-DB 3-Table Schema & Multi-Hub Lifecycle (`src/mcp/state.ts`, `src/mcp/receiver.ts`)

### 5.1 Three-Table SQLite Schema (`~/.cito/state.sqlite`, `PRAGMA user_version = 4`)
`LocalState.open()` sets `PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;` and runs a single `BEGIN IMMEDIATE ... COMMIT` transaction: if `PRAGMA user_version !== 4`, it executes `DROP TABLE IF EXISTS` on `local_listeners`, `local_signals`, `local_pairwise_wakes`, and `local_sent_idempotency`, creates the 3 tables below, and sets `PRAGMA user_version = 4`:

```sql
CREATE TABLE local_listeners (
  hub TEXT NOT NULL DEFAULT 'default',
  listener_id TEXT NOT NULL,
  public_id TEXT NOT NULL DEFAULT '',
  tag TEXT,
  principal_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  host_id TEXT NOT NULL,
  host_name TEXT NOT NULL,
  harness TEXT NOT NULL CHECK(length(harness) > 0),
  session_id TEXT NOT NULL,
  endpoint_pid INTEGER,
  cwd TEXT NOT NULL,
  repo TEXT,
  branch TEXT,
  title TEXT NOT NULL,
  about TEXT NOT NULL,
  scope TEXT NOT NULL CHECK(scope IN ('self','workspace')),
  status TEXT NOT NULL CHECK(status IN ('ACTIVE','REBINDING','UNLISTENED','FENCED_INACTIVE')),
  binding_generation INTEGER NOT NULL DEFAULT 0,
  owner_token TEXT NOT NULL DEFAULT '',
  persisted_seq INTEGER NOT NULL DEFAULT 0,
  consecutive_probe_failures INTEGER NOT NULL DEFAULT 0,
  replay_gap_detected INTEGER NOT NULL DEFAULT 0,
  replay_gap_range TEXT,
  dispatcher_token TEXT,
  lease_until_ms INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (hub, listener_id)
);
CREATE INDEX listeners_by_session ON local_listeners(harness, session_id);

CREATE TABLE local_signals (
  hub TEXT NOT NULL DEFAULT 'default',
  signal_id TEXT NOT NULL,
  listener_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  binding_generation INTEGER NOT NULL,
  from_user TEXT NOT NULL,
  from_host_name TEXT NOT NULL,
  from_harness TEXT,
  from_listener_id TEXT,
  from_title TEXT NOT NULL,
  from_about TEXT,
  message TEXT NOT NULL,
  held_reason TEXT,
  in_reply_to TEXT,
  created_at TEXT NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE','EXPIRED_UNSENT')),
  batch_id TEXT,
  batch_expires_at_ms INTEGER,
  dispatcher_token TEXT,
  queue_written INTEGER NOT NULL DEFAULT 0,
  acknowledged_at_ms INTEGER,
  batch_card TEXT,
  PRIMARY KEY (hub, signal_id)
);
CREATE INDEX signals_listener ON local_signals(hub, listener_id, state, seq);
CREATE INDEX signals_batch ON local_signals(hub, batch_id);

CREATE TABLE local_sent_idempotency (
  hub TEXT NOT NULL DEFAULT 'default',
  dedup_id TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  signal_id TEXT NOT NULL,
  seq INTEGER NOT NULL DEFAULT 0,
  to_listener_id TEXT NOT NULL,
  status TEXT NOT NULL,
  held_reason TEXT,
  expires_at_iso TEXT NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  PRIMARY KEY (hub, dedup_id)
);
```

### 5.2 Lock Names, Batch Identity & Stage-1 / Stage-2 Rules
- **Uniform Locks & Batch IDs**:
  - Listener lock: `listen-${hub}-${id}.lock`
  - Receiver lock: `receiver-${hub}-${id}.lock`
  - Session badge lock: `session-${harness}-${sha256(sessionId).slice(0, 16)}.lock`
  - Frozen batch ID: `batch_${hub}_${id}_${firstSeq}_${lastSeq}_${generation}` (scoped by `WHERE hub = ? AND batch_id = ?`).
- **Stage-1 Admission (`LocalState.admit`)**:
  - Duplicate `signal_id` on `hub` reuses stored row and advances `persisted_seq`.
  - If `signal.from.principal_id !== target.principal_id && target.scope !== 'workspace'`: advances `persisted_seq` and drops silently.
  - If `signal.target_generation < row.binding_generation`: inserts with `state = 'STORED_PASSIVE', held_reason = 'stale_generation'`, publishes `{ signal_id, status: 'stored_passive', held_reason: 'stale_generation' }` to `cito.ack.<signal_id>`.
  - Otherwise: inserts with `state = 'RECEIVED', held_reason = null`, publishes `{ signal_id, status: 'wake_accepted', held_reason: null }` to `cito.ack.<signal_id>`, shows HUD banner, refreshes badge, and schedules 1500ms Stage-2 dispatch.
- **Unified `refreshSessionBadge`**:
  - Under `session-${harness}-${sha256(sessionId).slice(0, 16)}.lock`, loads all rows for `(harness, session_id)` from `local_listeners`.
  - Resolves `target` (`endpoint_pid`, `public_id`) from the highest-generation row (any status).
  - Active rows are all rows with `status === 'ACTIVE'`, excluding any key in `options?.closingKeys` (`${hub}:${id}`) passed by `CitoService.close()` unless a non-blocking probe `acquireFlock("receiver-${hub}-${id}.lock", 0)` returns `null` (releasing any acquired probe lock immediately).
  - If `activeRows.length === 0`: calls `getAdapter(harness)?.updateBadge(target, undefined, config)` to clear the badge.
  - Otherwise: counts unread signals (`acknowledged_at_ms IS NULL AND expires_at_ms > ? AND state != 'EXPIRED_UNSENT'`) and `gap = activeRows.some(r => r.replay_gap_detected === 1)`, and calls `updateBadge` with `🔔 ${badgeSlug}${countStr}`.

---

## 6. Public MCP Tool Contracts (`src/mcp/service.ts`, `src/mcp/server.ts`)

1. **`cito_listen({ about, tag?, hub?, scope? })`**:
   - Enrolls on `targetHub = input.hub ?? config.defaultHub` (default `scope: "self"`). Quiesces any prior in-process receiver for `(targetHub, id)` before starting the new receiver.
   - Returns `{ ok: true, status: "listening", hub: targetHub, listener_id: formatScopedId(targetHub, publicId, config), binding_generation, user, host_id, host_name, harness, session_id, about, repo, branch, title, scope, lease_ttl_seconds: 60 }`.
2. **`cito_unlisten({ hub? })`**:
   - Targets `[input.hub]` if specified, else all configured hubs where caller's `(hub, listener_id)` is currently `ACTIVE` in SQLite (or `[config.defaultHub]` if none active).
   - If all targeted hubs fail remote KV update, rolls back local `ACTIVE` state and returns the error; if partial failure occurs across multiple hubs, rolls back failed hubs and returns `{ ok: true, status: "unlistened", listener_id, warnings: [{ hub, code, message }] }`.
3. **`cito_find({ query?, hub?, user?, host?, harness?, repo?, limit? })`**:
   - `harness` accepts any string matching `harnessIdSchema` (`^[a-z][a-z0-9_-]{0,31}$`).
   - Targets `[input.hub]` or all configured hubs in parallel (`Promise.allSettled`), wrapping each KV entry decode + `registrationSchema.parse` in `try/catch` to skip malformed records.
   - Sorts merged results by `b.revision - a.revision || a.hub.localeCompare(b.hub) || a.value.listener_id.localeCompare(b.value.listener_id)`, slices to `limit`, formats each `listener_id` via `formatScopedId`, and includes `hub` on each item plus `warnings` for unreachable hubs (throwing the first error if all targeted hubs fail).
4. **`cito_send({ to, hub?, message, in_reply_to?, idempotency_key? })`**:
   - Resolves `{ hub, id: targetId } = parseScopedId(input.to, config, input.hub)`. Validates `in_reply_to` via `parseScopedId(input.in_reply_to, config, hub)` when provided (rejecting cross-hub reply prefixes with `INVALID_INPUT`).
   - Compares sender 4-tuple `(principal_id, host_id, harness, session_id)` directly against `target` for `SELF_SEND_FORBIDDEN`.
   - Publishes unprefixed wire payload `{ signal_id, to: canonicalTo, target_generation, sender_endpoint_key, from, message, in_reply_to }` and awaits `cito.ack.<signal_id>` (`2000ms` timeout, defaulting to `status: "queued_for_reconnect", held_reason: null`).
   - Returns `{ ok: true, signal_id: formatScopedId(hub, row.signal_id, config), seq, to: formatScopedId(hub, displayId(target), config), status, held_reason, expires_at }`.
5. **`cito_inbox({ hub?, ack_ids?, ack_all? })`**:
   - `ack_all` is optional in `inboxInputSchema` with `const ackAll = input.ack_all ?? (input.ack_ids === undefined)` (draining by default when `ack_ids` is omitted).
   - For each slot in `targetedSlots` (`input.hub ? [slot] : allSlots`), resolves offline `principal = await slot.identity(true)` (if a slot throws `UNAUTHORIZED`, rethrows when `targetedSlots.length === 1` or when all slots fail, and skips unauthenticated secondary slots when `targetedSlots.length > 1`), computing `authorizedListeners` from `(slot.name, listenerId({ principal_id: principal.principal_id, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id }))`.
   - In one `BEGIN IMMEDIATE` transaction: validates `ack_ids` for out-of-scope `<hub>:` prefixes and cross-hub bare ID ambiguity across `authorizedListeners`; selects up to 50 unread non-expired rows (`ORDER BY created_at ASC, hub ASC, seq ASC, signal_id ASC LIMIT 50`); updates `acknowledged_at_ms = COALESCE(acknowledged_at_ms, now), state = CASE WHEN state = 'RECEIVED' THEN 'STORED_PASSIVE' ELSE state END` for the union of the `ackAll` page and matched `ack_ids`; and clears `replay_gap_detected = 0, replay_gap_range = NULL` on `authorizedListeners` when `ackAll` is true. Refreshes badge after commit.

---

## 7. Wake Cards & macOS HUD Banner (`src/adapters/wake-card.ts`, `src/macos/cito-banner.swift`)

- **In-Chat Wake Card (`formatWakeCard`)**:
  - Single signal:
    ```text
    🔔 Cito · tefx@Zhaomengs-MacBook-Air · codex [↩ personal:rcatwin-pbtjxcxkgw3y]

    压测数据已更新，请读取最新日志。
    ```
  - Multi-signal batch:
    ```text
    🔔 Cito · 2 条唤醒消息 → 检查在线的 Cito agent

    [1] alice@mac · pi [↩ personal:build-ab2def3gh4jk]
    Ready

    [2] bob@pc · codex
    Done
    ```
  - No `↳ Ack: cito_inbox(...)` footer is emitted.
- **macOS Native HUD Banner (`cito-banner.swift`)**:
  - Zero `SQLite3` dependency (`import SQLite3` and `Banner.state` removed).
  - Accepts any non-empty `target_harness`; renders `"点击跳转至 Codex 对话 ↗"` (system blue, opening `codex://threads/<id>` on click) when `target_harness == "codex"`, and `"\(target_harness) 终端会话 · 点击关闭横幅"` (tertiary label color) for other harnesses.

---

## 8. Proposed Design for Claude Code Adapter (设计建议 — 仅建议，尚未实现)

> **状态声明（仅建议设计 / Proposed Design Suggestion Only — Not Yet Implemented）**：以下为未来接入 Claude Code 时的架构设计建议，不包含具体代码实现细节，当前版本尚未实现 Claude Code 宿主适配。

1. **会话身份与本地桥接建连建议**：
   - 由于 Claude Code 不支持加载进程内 TypeScript 扩展，建议利用 `SocketBridgeAdapter` 的通用本地 Unix Socket 协议（`<harness>-pid-<pid>.sock`）：由运行在 Claude Code 下的 MCP 服务端实例（或 `SessionStart` Hook 伴随进程）在收到首个工具请求时，按调用方进程根 PID 惰性绑定 `~/.cito/sockets/claude-pid-<pid>.sock`，将身份识别、20 秒存活探针与跨进程恢复统一收敛至标准桥接协议。
2. **双模消息投递建议（MCP Channel Push + Turn-Start Inbox Drain）**：
   - **主动唤醒推送（Channel Push）**：MCP 服务端在握手阶段声明实验性通道能力（如 `claude/channel`），当本地桥接 Socket 收到 `deliver` 请求时，通过该 Claude 会话所属的 MCP stdio 通道推送通知以唤醒空闲会话。
   - **回合开始拉取兜底（Inbox Drain）**：考虑到客户端可能未启用实验性推送通道或正处于忙碌回合，桥接投递返回“已投递未确认（`delivered_unconfirmed`）”状态，使消息保留在未读收件箱中；配合 Claude Code 的 `UserPromptSubmit` Hook 或在回合开始时调用无参 `cito_inbox()`（默认读取即清空），确保消息在下一轮对话开始时可靠进入上下文。
