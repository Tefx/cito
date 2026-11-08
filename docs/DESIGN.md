# Cito Architecture & Protocol Specification (V22 — Stage 6 `v0.18.0`: Pure Rust Core (`libsqlite3`), Symmetric Subprocess Plugin Protocol (`stdin`/`stdout` NDJSON), `cito-claude-hook` Inside `plugins/claude/`, Standard Pi Package Mode, Retirement of Duplicate `src/*.ts` & Complete `F1`–`F64` Remediation)

> **Normative Supersession Notice (`V22` / `v0.18.0`)**:
> This `V22` specification is the single authoritative contract for Cito `v0.18.0`. Where earlier stage notes referred to TypeScript core modules (`src/{mcp,hub,setup,ui,shared,adapters,auth-callout}/*.ts`), 6-character pairing codes (`cito-xxxxxx`), in-process Node.js `plugins/telegram`, or top-level `bin/cito-claude-hook`, Section 8 and the updated sections below supersede them in full. All paths written with `~` in prose examples represent the user's home directory; in actual runtime configuration files (`settings.json`, `config.toml`, `LaunchAgent` plist), Cito always expands `~` to the resolved absolute filesystem path.

---

## 1. Purpose, Ontology, and Architectural Guardrails

### 1.1 Purpose & Core Ontology (`Node (1) ↔ Hub ↔ Channels (1..K)`)
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a unified signaling and wake network connecting **Humans** and **Agent Sessions** across machines, hubs, and interaction channels:
1. **Two Symmetric Network Nodes (`Node`)**:
   - **Human Node (`harness: "human", session_id: "default"`)**: Represents a human user (`@alice`, `@bob`) on a Hub.
   - **Agent Session Node (`harness: "pi" | "codex" | "claude" | ..., session_id: "<sid>"`)**: Represents an agent conversation/session on a Hub.
   - **Host-Independent Node Identity (`node_id`)**: `node_id` (`listenerId`) is derived solely from `(principal_id, harness, session_id)` **without** `host_id`. Each human user has at most one `Human Node` per Hub (`session_id` is strictly `"default"`, `F63`), and each logical agent session has at most one `Agent Session Node` per Hub.
2. **Multi-Path Channels (`Channel`, $1 \dots K$ per `Node`)**:
   - Physical hosts (`Mac A`, `Mac B`), desktop menu bar UIs (`cito-ui`), and external messaging apps (`Telegram`, etc.) are **Channels** connecting a `Node` to the `Hub`.
   - A `Node` is `ACTIVE` on a Hub whenever **at least 1** of its attached `Channels` has a valid lease (`updated_at_ms` within the 60s TTL, or a permanent `host_id == "hub"` attachment for active Hub-hosted channels, `F13`). Attaching multiple Channels never triggers cross-channel `FENCED_INACTIVE` eviction.
3. **100% Hub-Routed Interaction (Zero External Window-Jump Bypass)**:
   - All interaction between any two Nodes (`Human ⇄ Agent`, `Agent ⇄ Agent`, `Human ⇄ Human`) flows strictly through the **Hub's regular signaling channel (`cito_send` / `signal_ack`)**.
   - Out-of-band OS URL window jumping (`jumpUrlTemplate`, `jump_url`, `can_jump`, `Open in <App> ↗`) is completely absent across `crates/cito-native/src/`, `plugins/`, and `src/macos/cito-ui.swift`. Clicking a `WakeBanner` or an `Activity` item opens `DirectoryPanel` (`Activity` tab) with the inline `ComposerBar` focused to reply via the Hub.

### 1.2 Language, Neutral Naming & Zero-Hardcoding Guardrails
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces, error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names may appear anywhere in source code, UI, configuration defaults, tests, or documentation; use vendor-neutral generic examples (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **Zero Concrete Agent or Channel Strings in Core (`crates/cito-native/src/core/` & `src/macos/cito-ui.swift`)**:
  - Core modules (`crates/cito-native/src/core/` and `src/macos/cito-ui.swift`) MUST NOT contain hardcoded branches, identifiers, or UI labels for any specific Agent App (`pi`, `codex`, `claude`) OR any specific external Human Channel (`telegram`, `webhook`, `slack`, `teams`, `email`, `feishu`, `discord`).
  - Mechanical gate:
    `grep -rnE -i '\b(pi|codex|claude|telegram|webhook|slack|teams|email|discord|feishu)\b' crates/cito-native/src/core/ src/macos/` MUST return `0` matches.
  - All concrete Agent Apps (`kind: "agent"` / `"agent-harness"`) and Human Channels (`kind: "human"` / `"human-channel"`) live exclusively in `plugins/<id>/` (bundled) and `${CITO_STATE_DIR}/plugins/<id>/` (user-installed) and are discovered dynamically from `<dir>/plugin.json`.
- **Mechanism, Not Policy (`cito_send.message`)**: Recommend Cito primarily as a lightweight control plane in MCP tool descriptions without enforcing a hard 500-character policy cap (`sanitize_signal_message` allows `1..65,536` Unicode code points after stripping non-whitespace control characters).

---

## 2. Three-Layer Logical Architecture & Physical Deployment Co-Location

### 2.1 Strictly Decoupled Logical Modules (`crates/cito-native/src/core/`)
1. **`HubEngine` (`crates/cito-native/src/core/hub.rs`)**:
   - Owns the SQLite-backed `Node ↔ Channels (1..K)` registry (`hub_kv`), signal stream (`hub_signals`), consumer state (`hub_consumers`), exclusive wake claims (`hub_signal_claims`), channel delivery ledger (`hub_channel_deliveries`), plugin service state (`hub_plugin_services`), user channel bindings (`hub_human_channels`), pairing codes (`hub_pairing_codes`), and audit log (`hub_audit_log`).
2. **`AgentChannelHost` (`crates/cito-native/src/core/daemon.rs`, `receiver.rs`, `plugins.rs`)**:
   - Bridges local agent sessions (`plugins/{pi,codex,claude}` and external harness plugins) to the Hub via `HubClient` and out-of-process `agent-harness` plugin commands.
3. **`HumanChannelHost` (`crates/cito-native/src/core/hub.rs` & `daemon.rs`)**:
   - **`scope: "user"` Desktop UI (`plugins/human/bin/cito-ui`)**: Hosted by `cito daemon` on the user's Mac over the 27-op macOS UI NDJSON stream (`stdin`/`stdout`), aggregating state across all Hubs configured in `cito daemon`.
   - **`scope: "hub"` External Channels (`plugins/telegram/bin/cito-plugin-telegram` & `~/.cito/plugins/<id>/`)**: Hosted by `HubEngine` (either inside `cito daemon` for the local Hub or inside standalone `cito hub` on a server) over the persistent HumanChannel NDJSON stream (`stdin`/`stdout`).

### 2.2 Physical Co-Location Across Deployment Modes
- **Embedded Local & LAN Hub Mode (`local://` and `:4291` `Share Hub on Local Network` inside `cito daemon`)**:
  - `cito daemon` runs a single unified local `HubEngine` persisted at `${stateDir}/hub.sqlite` (`F9`). When `Share Hub on Local Network` (`host_hub.enabled`) is turned on, `cito daemon` binds an HTTP/SSE server (`0.0.0.0:4291` by default) directly onto that same local `HubEngine` instance so local sessions, LAN peers, and local Human Channel bindings share one consistent state store.
- **Standalone Hub Mode (`cito hub` on Server / Docker)**:
  - `cito hub` runs `HubEngine` persisted at `${dataDir}/hub.sqlite` (when `--data-dir` / `CITO_HUB_DATA_DIR` is provided) and hosts `scope: "hub"` `HumanChannel` subprocesses (such as `plugins/telegram`) in the same deployment unit so cloud channels stay online 24/7 and share one Hub bot token across the team without `409 Conflict`.

---

## 3. Symmetric `Node ↔ Channels` Protocol, Addressing & Multi-Channel Sync

### 3.1 Node Identity & `@self` / `@<username>` Shorthand (`crates/cito-native/src/core/identity.rs`, `daemon.rs`)
- **Node Identity (`listener_id` & `public_id`)**:
  - `NodeKey = { principal_id: String, harness: String, session_id: String }`.
  - For a `Human Node`: `harness = "human"`, `session_id = "default"`. Any explicit caller metadata specifying `expected_harness: "human"` with `expected_session_id != "default"` is rejected with `CitoError("INVALID_INPUT", "Human node session_id must be 'default'")` (`F63`).
  - `listener_id(key)` hashes `"cito-v2|" + principal_id + "|" + harness + "|" + session_id` (`c_` + 16 hex chars, accepting legacy `"cito-v1|..."` hashes in validators).
  - `public_listener_id(owner_user, canonical_id)` normalizes `owner_user` through `normalize_slug(owner_user)` (lowercasing ASCII alphanumerics, replacing `.`, `_`, and other non-alphanumeric runs with `-`, trimming leading/trailing `-`, defaulting to `"user"`, max 32 chars) before formatting `<user_slug>-<hex_suffix>` so usernames like `alice.smith` or `alice_smith` always produce schema-valid `public_id`s across all registration and `send_reply_from_human` paths (`F52`).
- **Human Addressing Shorthands in `cito_send` (`to`)**:
  - `to` accepts `[<hub>:]@self`, `[<hub>:]@<username>`, or `[<hub>:]<listener_id_or_public_id>`.
  - `@self` (or `@<caller_owner_user>`) deterministically resolves to `listener_id({ principal_id: identity.principal_id, harness: "human", session_id: "default" })` on the target Hub, calling `ensure_local_human_node(slot)` on both `local://` and remote hubs (unless explicitly unlistened by the user) before resolving (`F13`).
  - `@<other_username>` queries active `harness == "human"` nodes on the target Hub matching `lower(owner_user) == lower(other_username)` or `normalize_slug(owner_user) == normalize_slug(other_username)`:
    - `0` matches $\to$ `CitoError("LISTENER_NOT_FOUND", ...)`.
    - `> 1` distinct `principal_id`s $\to$ `CitoError("INVALID_INPUT", "Ambiguous @username; use full listener_id")`.
- **Self-Send Guard (`SELF_SEND_FORBIDDEN`)**:
  - Compares logical Node IDs (`from.node_id == Some(canonical_to)`). Sending between `Human Node (@alice)` and `Agent Node (pi:S1)` is permitted; unbound CLI calls (`cito send --to @self -m "test"`) have `from.node_id = None` and are permitted.

### 3.2 Multi-Channel Registration & Lease Aggregation (`crates/cito-native/src/core/lease.rs`, `hub.rs`)
- **`Registration` Schema**:
  - Includes `channels: BTreeMap<String, ChannelAttachment>` where each `ChannelAttachment` has `{ channel_id, kind, host_id, host_name, delivery_mode: "notify_fanout" | "wake_exclusive", priority, state: "ACTIVE" | "UNLISTENED", updated_at_ms }`.
  - **Lease Evaluation (`evaluate_node_lease`, `F13`)**:
    - A channel attachment with `state == "ACTIVE"` and `host_id == "hub"` (an enabled Hub-hosted Human Channel such as Telegram) is treated as **permanently active** while its binding remains enabled in `hub_human_channels` (never expiring at the 60s `ACTIVE_LEASE_MS` or 30m `RETENTION_MS` thresholds).
    - A host-attached channel (`host_id != "hub"`) is `ACTIVE` when `now - updated_at_ms < 60_000`, `DISCONNECTED_RETAINED` when `60_000 <= age <= 1_800_000`, and `EXPIRED` when `age > 1_800_000`.
    - On `HubEngine` startup, `HubEngine` automatically restores `hub:<plugin_id>` channel attachments in `hub_kv` for all enabled `hub_human_channels` bindings (`F13`).
  - **Channel-Scoped Detach on Local Session Switch (`F37`)**:
    - When `Receiver` detects that a local Agent session switched (`status == "switched"`), `revoke_session` detaches only the local host's channel (`channels[local_channel_id].state = "UNLISTENED"`). If other active channels on sibling hosts remain attached to that Node, top-level Node `state` remains `"ACTIVE"` and `binding_generation` is preserved so sibling hosts are not fenced.

### 3.3 Transport Cursor (`consumer_ack`), Business Read (`signal_ack`), Atomic `signal_claim_dispatch` & Delivery Guarantees (`F10–F12, F15–F19, F54, F55, F60`)
1. **Transport vs. Business Ack & Durable Offline Reconciliation (`F10`)**:
   - `consumer_ack` advances only the transport cursor (`hub_consumers.last_acked_seq`).
   - `signal_ack({ node_id, signal_ids, channel_id })` sets `hub_signals.acked_at_ms = now`, broadcasts `{ type: "node_ack", node_id, signal_ids, acked_at_ms, channel_id }` over SSE (filtered by `principal_id` RLS in OIDC mode, `F6`), and sends `{"op": "on_acked", ...}` to active `HumanChannel` subprocesses.
   - If `cito daemon` acknowledges signals locally (`ack_signals` UI action or `cito_inbox`) while the target Hub is offline or the `signal_ack` RPC fails, `state.sqlite` records `(hub, node_id, signal_id, acked_at_ms)` in `pending_hub_acks`. Upon Hub reconnect, `cito daemon` flushes `pending_hub_acks` and reconciles `acked_at_ms` for existing local unread rows from `consumer_backlog` (`F10`).
   - **Unconditional Atomic Wake Claim (`signal_claim_dispatch`) & Winner-Crash Recovery (`F11, F12, F54, F55, F60`)**:
     - **Always Claim Before Wake (`F60`)**: Before executing any Agent wake effect, `Receiver` MUST call `signal_claim_dispatch({ node_id, signal_ids, channel_id, lease_ms: 30_000 })` even when `activeChannelCount == 1`, preventing duplicate wakes if a second channel attaches mid-flight.
     - **Fail-Closed on RPC Error (`F11`)**: If the `signal_claim_dispatch` HTTP RPC fails or times out, `Receiver` treats the claim as **unacquired due to transport error**, does **not** execute the wake effect, keeps the local signal rows in `RECEIVED` state, and schedules a retry.
     - **All-or-Nothing Batch Transaction & Missing/Acked Row Semantics (`F12`)**: In `HubEngine`, `signal_claim_dispatch` evaluates all requested `signal_ids` inside a single `BEGIN IMMEDIATE` SQLite transaction. For each requested `signal_id`:
       - If the row is missing or already has `acked_at_ms IS NOT NULL`, the transaction rolls back and returns `{ claimed: false, reason: "already_acked", claimed_ids: [], acked_ids: [...] }` without mutating any row; the caller marks `acked_ids` as acknowledged (`STORED_PASSIVE` with `acknowledged_at_ms = now`) and re-batches any remaining unacknowledged signals.
       - If **any** requested signal in the batch has `acked_at_ms IS NULL` and is currently claimed by another channel (`claimed_by_channel != channel_id` and `claim_until_ms > now`), the transaction rolls back and returns `{ claimed: false, reason: "already_claimed", claimed_ids: [], acked_ids: [] }` without mutating any row.
       - Otherwise, all requested signals in the batch are updated with `claimed_by_channel = channel_id, claim_until_ms = now + lease_ms` and `{ claimed: true, reason: null, claimed_ids: signal_ids, acked_ids: [] }` is returned.
     - **Losing Channel Preserves Unread State & Lease Renewal (`F54`)**: When `signal_claim_dispatch` returns `{ claimed: false, reason: "already_claimed" }`, the losing channel transitions its local rows to `state = 'STORED_PASSIVE'` with **`acknowledged_at_ms = NULL`** (keeping them unread in local `cito_inbox` until the winning channel completes delivery and broadcasts `signal_ack`, or until the 30s claim lease expires without `signal_ack`, at which point a `30_500 ms` fallback timer transitions unacknowledged rows back to `RECEIVED` and re-schedules dispatch). While a long-running wake command is in flight, the winning channel renews its claim lease every `10_000 ms` (`signal_claim_dispatch` with `lease_ms: 30_000`); if renewal fails or returns `claimed: false`, the in-flight wake child process is aborted (`SIGTERM`) if not yet completed, with documented **at-least-once** delivery semantics under network partition.
     - **Retry Schedule Recovery (`F55`)**: In `Receiver::receive`, if `LocalState::admit` returns `duplicate: true` for a signal whose local row is still in `state == 'RECEIVED'` (because a previous `consumer_ack` flush failed after `admit`), `Receiver` still calls `self.schedule()`.
   - **External Channel Delivery Reservation, Hole Retry & Multipart Chunking (`F15, F16, F18, F19`)**:
     - **Pre-I/O Reservation Lock (`F16`)**: Before sending a signal to a `HumanChannel` subprocess (`{"op": "deliver", ...}`), `HubEngine` executes an atomic `INSERT OR REPLACE INTO hub_channel_deliveries (plugin_id, principal_id, signal_id, seq, status, delivered_ms, ...) VALUES (..., 'in_flight', now, ...)` inside SQLite if and only if no row exists with `status == 'delivered'` or (`status == 'in_flight'` and `now - delivered_ms < 60_000`).
     - **Per-Signal Hole Retry & Durable `initial_bind_seq` Boundary (`F15`)**: When a user first binds or re-enables a channel in `hub_human_channels`, `last_delivered_seq` is initialized to the current `MAX(seq)` in `hub_signals` (serving as the durable lower-bound `initial_bind_seq` so historical pre-binding signals are never dumped, while `last_delivered_seq` is **never** advanced by individual deliveries). Replay queries select all unread signals (`acked_at_ms IS NULL`) for `principal_id` with `seq > hub_human_channels.last_delivered_seq` that have no `status == 'delivered'` (or active `'in_flight'`) row in `hub_channel_deliveries`, so a transient failure on `seq = 10` followed by success on `seq = 11` always retries `seq = 10` on subsequent passes. If the Hub process crashes while a delivery is `in_flight`, rows with `now - delivered_ms >= 60_000` are retried on restart (documented **at-least-once** crash-window semantics).
     - **Atomic Inbound Reply Deduplication & Cursor Ordering (`F18`)**: When `HubEngine` receives `{"op": "inbound_reply", "update_id": ...}`, it executes a single SQLite transaction that checks `SELECT 1 FROM hub_processed_replies WHERE plugin_id = ? AND update_id = ?`, inserts the `hub_processed_replies` record alongside the published reply signal in `hub_signals` and `acked_at_ms` update on the original signal, and commits atomically before sending `{"op": "reply_ack", "update_id": ...}` to the plugin. The plugin only emits `{"op": "cursor_save", "cursor": ...}` after receiving `reply_ack` (or logging an isolated per-update error).
     - **Multipart Chunking (`> 4096` chars) & Multi-Message ID Mapping (`F15`)**: When a formatted notification exceeds 4,096 Unicode code points, `plugins/telegram` splits it into sequential chunks ($\le 4,096$ chars each), sends each via `sendMessage`, and returns all resulting Telegram `message_id`s in `external_msg_ids: ["101", "102"]` (with primary `external_msg_id: "102"`). `HubEngine` stores a mapping row in `hub_channel_delivery_msgs (plugin_id, principal_id, external_msg_id, signal_id, from_listener_id)` for **every** chunk ID so swipe-replying to any chunk resolves the target signal and `on_acked` receives all chunk IDs.

---

## 4. Human Node Enrollment, Symmetric Subprocess Plugin Contracts & Built-in Plugins

### 4.1 Per-Hub Human Node Freedom (`Listen / Unlisten` & `🔒 Self / 👥 Team`, `F13, F14`)
- On the local `local` Hub (`local://`): the user's `Human Node` (`@<user> (Me)`) is auto-enrolled on first startup (`scope: "workspace"`) unless explicitly unlistened.
- On remote Hubs (`oidc` **and** `token`, `F14`): when enrolled (either via desktop UI or when pairing a Human Channel on the Hub), the user's `Human Node` defaults to **`scope: "self"` (`🔒 Self`)** so other principals on a shared token or OIDC Hub cannot discover or message the user until the user explicitly switches to `👥 Team` (`scope: "workspace"`).

### 4.2 Authoritative `plugin.json` Manifest Schema
Every built-in plugin (`plugins/{pi,codex,claude,human,telegram}/plugin.json`) and external plugin (`~/.cito/plugins/<id>/plugin.json`) conforms to:
```json
{
  "id": "telegram",
  "kind": "human-channel",
  "displayName": "Telegram",
  "tier": "stable",
  "scope": "hub",
  "sfSymbol": "paperplane.fill",
  "tint": "blue",
  "cliFlag": false,
  "command": ["./bin/cito-plugin-telegram"],
  "channelEnv": null,
  "fields": [
    { "key": "botToken", "scope": "hub", "label": "Bot Token", "type": "secret", "required": true, "placeholder": "123456:ABC-DEF..." },
    { "key": "chatId", "scope": "user", "label": "Chat ID", "type": "string", "required": true, "placeholder": "Paired via /start or enter Chat ID" }
  ],
  "pairing": { "supported": true, "actionLabel": "Pair Account…" }
}
```
- **`kind` Normalization**: Accepts `"agent"` or `"agent-harness"` (normalized to `"agent-harness"`) and `"human"` or `"human-channel"` (normalized to `"human-channel"`). Required fields: `id` (`^[a-z0-9][a-z0-9_-]{0,31}$`), `displayName` (non-empty string), and `command` (non-empty string array for subprocess plugins). Optional fields: `kind` (defaults to `"agent-harness"`), `tier` (`"stable" | "experimental"`, defaults to `"stable"`), `scope` (`"hub" | "user"`, defaults to `"hub"`), `sfSymbol`, `tint`, `cliFlag` (`boolean`), `channelEnv` (`string | null`), `fields` (`PluginFieldDescriptor[]`), `pairing` (`{ supported: boolean, actionLabel?: string }`).
- **`command` Resolution & Timeout**: Array of `[executable, ...args]`. If `executable` starts with `./` or `../`, it is resolved relative to `<plugin_dir>`. If `command` is omitted in an external plugin directory (`~/.cito/plugins/<id>/`), Core checks for `<plugin_dir>/index.js` (`["node", "<plugin_dir>/index.js"]`) or `<plugin_dir>/index.ts` (`["node", "--experimental-strip-types", "<plugin_dir>/index.ts"]`), returning an explicit `PLUGIN_LOAD_ERROR` if neither `command` nor an entrypoint file exists. One-shot `agent-harness` subprocess requests enforce a `10_000 ms` timeout (killing the child on timeout) and treat any non-zero exit or invalid JSON output as `{"ok": false, "error_code": "PLUGIN_EXEC_FAILED", "message": ...}`. If a persistent `human-channel` subprocess exits while `format_pair` or `test` is pending, all pending `req_id` promises immediately resolve with `{"ok": false, "error": "Plugin subprocess exited"}`.
- **`wake` Signal Item Schema**: In `wake`, each element of `"signals"` has the exact shape `{"signal_id": string, "from_listener_id": string, "from_user": string, "from_harness": string, "from_title": string, "message": string, "created_at": string, "in_reply_to": string | null}`.

### 4.3 Contract 1: `agent-harness` Subprocess Protocol (`stdin` JSON line $\to$ `stdout` JSON line)
Each request is a single JSON object written to `stdin` followed by `\n` (and `stdin` close for one-shot ops); the plugin writes a single JSON response line to `stdout`:

1. **`setup_inspect`**:
   - Request: `{"op":"setup_inspect","state_dir":"/Users/alice/.cito","plugin_dir":"/Users/alice/.cito/plugins/pi","env":{...}}`
   - Response: `{"ok":true,"installed":true,"configured":true,"paths":["/Users/alice/.pi/agent/settings.json"]}`
2. **`setup_apply`**:
   - Request: `{"op":"setup_apply","dry_run":false,"force":true,"state_dir":"...","plugin_dir":"...","env":{...}}`
   - Response: `{"ok":true,"actions":[{"target":"pi","path":"...","status":"created"|"updated"|"unchanged"|"conflict","backupPath":null,"detail":"..."}]}`
3. **`setup_remove`**:
   - Request: `{"op":"setup_remove","dry_run":false,"state_dir":"...","plugin_dir":"...","env":{...}}`
   - Response: `{"ok":true,"actions":[{"target":"pi","path":"...","status":"updated"|"unchanged","detail":"..."}]}`
4. **`discover_sessions`**:
   - Request: `{"op":"discover_sessions","state_dir":"...","env":{...}}`
   - Response: `{"ok":true,"sessions":[{"harness":"codex","session_id":"019c...","cwd":"/Users/alice/repo","repo":"repo","branch":"main","title":"Fix bug","endpoint_pid":null,"updated_at_ms":1700000000000}]}`
5. **`resolve_caller`**:
   - Request: `{"op":"resolve_caller","meta":{...},"headers":{...},"cwd":"/Users/alice/repo","http_client_pid":1234,"http_transport":true,"require_open":true,"cli_cmd":"listen"|"unlisten"|"inbox"|"find"|"send"|null,"explicit_session_id":null,"state_dir":"...","env":{...}}`
   - Response (success): `{"ok":true,"caller":{"harness":"codex","session_id":"019c...","cwd":"/Users/alice/repo","repo":"repo","branch":"main","title":"Fix bug","endpoint_pid":1234}}`
   - Response (error): `{"ok":false,"error_code":"INVALID_INPUT"|"BINDING_REQUIRED"|"PI_BRIDGE_UNAVAILABLE"|"SESSION_NOT_OPEN","message":"..."}`
6. **`probe`**:
   - Request: `{"op":"probe","session_id":"...","cwd":"...","endpoint_pid":1234,"state_dir":"...","env":{...}}`
   - Response: `{"ok":true,"status":"alive"|"switched"|"closed_definitive"|"transient_error","current_session_id":"...","reason":null}`
7. **`update_badge`**:
   - Request: `{"op":"update_badge","session_id":"...","endpoint_pid":1234,"badge":"🔔 cito (2)"|null,"state_dir":"...","env":{...}}`
   - Response: `{"ok":true}`
8. **`wake`**:
   - Request: `{"op":"wake","session_id":"...","cwd":"...","endpoint_pid":1234,"batch_id":"b_1","wake":true,"text":"...","signals":[...],"state_dir":"...","env":{...}}`
   - Response: `{"ok":true,"outcome":"delivered"|"delivered_unconfirmed"|"session_switched"|"driver_unavailable","reason":null}`
   - The host may include `checkpoint_only: true`, `queue_written`, and `queue_confirmed`. A multi-effect plugin returns its insertion checkpoint with `queue_written: true`, `queue_confirmed`, and `continue_wake: true` before submitting a separate wake effect. The host persists the checkpoint and revalidates unreadness, the active binding, generation, owner, and Hub claim before the next `wake` request. The plugin skips insertion when `queue_written` is true. An unknown insertion keeps `queue_confirmed: false` and must never be resubmitted. Omitting `continue_wake` completes the operation; single-effect plugins may ignore `checkpoint_only`. A process death before the checkpoint response retains the existing at-least-once limitation.
9. **`stdio_sidecar` (Streaming Mode when `plugin.json` `channelEnv` is set in `cito mcp-stdio`'s environment, e.g. `CITO_CLAUDE_CHANNEL=1`)**:
   - Request on `stdin` (kept open so `stdin EOF` terminates the sidecar when `cito mcp-stdio` exits): `{"op":"stdio_sidecar","endpoint_pid":1234,"cwd":"...","state_dir":"...","env":{...}}`
   - First `stdout` line: `{"op":"ready","capabilities_patch":{"experimental":{"claude/channel":{}}}}`
   - Subsequent `stdout` lines: raw JSON-RPC notification frames (`{"jsonrpc":"2.0","method":"notifications/claude/channel","params":{...}}`) forwarded by `cito mcp-stdio` to its stdout.

### 4.4 Contract 2: `human-channel` Persistent Subprocess Protocol (`stdin`/`stdout` NDJSON)
- **Separation of Desktop UI (`cito-ui`) vs. Hub Channel Subprocesses (`plugins/telegram` & external `kind: "human-channel"`)**:
  - **`plugins/human/bin/cito-ui` (`scope: "user"`)**: Spawned and owned by `cito daemon` on the local Mac, speaking the 27-op UI IPC protocol (§5).
  - **`plugins/telegram/bin/cito-plugin-telegram` & external `kind: "human-channel"` (`scope: "hub"`)**: Spawned and owned by `HubEngine` whenever required `hub_config` fields (e.g. `botToken`) are present **or** when an on-demand `human_channel_pair_start` / `human_channels_test` RPC arrives. If all bindings are disabled and `hub_config` is cleared, `HubEngine` sends `{"op":"shutdown"}` and terminates the subprocess (`F10, F32`).
  - **Anti-Orphan `stdin EOF` Rule**: The subprocess monitors `stdin` on a dedicated reader thread and exits within `< 100 ms` when `stdin` reaches `EOF`.
- **Parent (`HubEngine`) $\to$ Subprocess (`stdin` NDJSON)**:
  1. `{"op":"configure","hub_config":{"botToken":"..."},"bindings":[{"principal_id":"...","user":"alice","config":{"chatId":"..."}}],"cursor":"100"}`
  2. `{"op":"deliver","delivery_id":"d_1","principal_id":"...","user":"alice","config":{"chatId":"..."},"signal":{"signal_id":"s_1","from":{"listener_id":"alice-pi-1234","canonical_id":"c_...","harness":"pi","title":"work"},"to":{"listener_id":"alice-human-default"},"kind":"NeedInput","summary":"...","message":"...","created_at_ms":1700000000000}}`
  3. `{"op":"on_acked","principal_id":"...","signal_ids":["s_1"],"acked_at_ms":1700000005000,"external_msg_ids":["101","102"],"user_config":{"chatId":"..."}}`
  4. `{"op":"format_pair","req_id":"r_1","code":"cito-a1b2c3d4e5","hub_config":{"botToken":"..."}}`
  5. `{"op":"pair_ack","code":"cito-a1b2c3d4e5","ok":true,"owner_user":"alice","error":null}`
  6. `{"op":"test","req_id":"r_2","user":"alice","hub_config":{"botToken":"..."},"user_config":{"chatId":"..."}}`
  7. `{"op":"reply_ack","update_id":"u_99","ok":true,"target_display":"alice-pi-1234","error":null}`
  8. `{"op":"shutdown"}`
- **Subprocess $\to$ Parent (`HubEngine`) (`stdout` NDJSON)**:
  1. `{"op":"delivered","delivery_id":"d_1","ok":true,"external_msg_id":"102","external_msg_ids":["101","102"],"error":null}`
  2. `{"op":"pair_redeem","code":"cito-a1b2c3d4e5","external_chat_id":"12345","external_user_id":"12345","is_private_chat":true,"display_label":"@alice_tg","user_config_patch":{"chatId":"12345"}}`
  3. `{"op":"inbound_reply","update_id":"u_99","external_chat_id":"12345","external_user_id":"12345","reply_to_external_msg_id":"102","text":"Ship it"}`
  4. `{"op":"cursor_save","cursor":"101"}`
  5. `{"op":"rpc_result","req_id":"r_1","ok":true,"instruction":"Send \"/start cito-a1b2c3d4e5\" to @my_bot","bot_username":"my_bot","detail":"Sent test message","error":null}`

---

## 5. Four-Surface macOS UI Architecture (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`)

### 5.1 Complete 27-Op UI IPC Protocol (`cito daemon` $\leftrightarrow$ `cito-ui`)
- **Daemon $\to$ UI (`stdin` NDJSON)**:
  - `{"op":"state","status":"idle"|"active"|"degraded"|"offline","hub":...,"hubs":[...],"sessions":[...],"my_remote_listeners":[...],"available":[...],"activity":[...],"unread_count":0,"total_online":0,"settings":{...},"composer_error":null}`
  - `{"op":"directory","hub":"local","peers":[...],"activity":[...],"unread_count":0}` (always includes explicit `unread_count` even when `0` and `activity: []` even when empty, `F32`)
  - `{"op":"banner","payload":{...}}`
  - `{"op":"settings","settings":{...}}`
  - `{"op":"pair_code","channel_id":"telegram","code":"cito-a1b2c3d4e5","instruction":"Send \"/start cito-a1b2c3d4e5\" to @my_bot","expires_at_ms":...}`
  - `{"op":"channel_test_result","channel_id":"telegram","ok":true,"detail":"..."}`
  - `{"op":"composer_result","ok":true,"error":null}` (when `ok: false`, `cito-ui` preserves the user's typed text in `composer_input` and displays `error`, `F32, F57`)
  - Test observation frames: `test_inspect`, `test_action`, `test_directory_key`, `test_directory_query`, `test_settings_hub_draft`, `test_channel_draft`, `test_clipboard`.
- **UI $\to$ Daemon (`stdout` NDJSON — All 27 Actions Handled in `daemon.rs`, `F1`)**:
  - Session & Scope: `listen_session`, `unlisten`, `unlisten_all`, `set_scope`, `refresh_available`, `copy_id`
  - Hub & Network: `switch_hub`, `hub_action`, `set_host_hub`, `setup_save_hub`, `setup_toggle_hub`, `setup_remove_hub`
  - Directory & Activity: `open_directory`, `close_directory`, `query_directory`, `send_signal` (validated against `sendInputSchema`: `to` non-empty, `message` `1..65,536` code points after sanitization, `F57`), `ack_signals`
  - Settings & Channels: `open_settings`, `close_settings`, `setup_apply`, `setup_remove_app`, `setup_uninstall_all`, `set_launch_at_login`, `set_desktop_banner`, `channel_save` (includes optional `"hub"` target from the `Human Channels` Hub selector, `F5`), `channel_test` (includes `"hub"`), `channel_pair` (includes `"hub"`)
  - Lifecycle: `quit`.

### 5.2 `src/macos/cito-ui.swift` Behavioral & Layout Fixes (`F5, F26, F27, F32, F47, F48, F49, F57`)
1. **`⌘,` Shortcut**: Bind `keyEquivalent = ","` (`NSEvent.ModifierFlags.command`) on the `openSettings` menu item and local key handler so pressing `⌘,` opens `SettingsPanel` (`F5`).
2. **`DirectoryPanel` Keyboard Navigation & Dynamic Footer Hints (`F5, F49`)**:
   - In `handleDirectoryKey("up" | "down")`, navigate `activityFilteredItems` when `directoryMode == "activity"` and `directoryFilteredPeers` when `directoryMode == "directory"` (`F5`).
   - Update `directory_shortcuts` text dynamically: `"↵ Copy Selected ID    ⎋ Close"` in `Directory` mode, `"↵ Reply    ⎋ Close"` in `Activity` mode, and `"↵ Send    ⎋ Cancel Reply"` when `ComposerBar` is open (`F5`).
   - When switching `Activity -> Directory` without changing `directoryQuery`, preserve the server-matched `directoryPeers` list instead of re-filtering truncated `about` (300 chars) / `title` (200 chars) DTO strings locally (`F49`).
3. **`SettingsPanel` Hub Selector, `NSSecureTextField`, Scroll Views & Non-Overlapping Uninstall Box (`F5, F47, F48`)**:
   - Add a Hub selector pill bar (`id: "channel_hub_selector:\(h.name)"`) at the top of `SettingsPanel → Human Channels` so the user can configure/pair Human Channels on any configured Hub (`F5`).
   - Render `type == "secret"` fields (`botToken`) using `NSSecureTextField` (masked bullets) and widen field labels (`150pt` with tooltip) so labels never truncate (`F5`).
   - Wrap the card lists in `Agent Apps`, `Human Channels`, and `Hubs & Network` inside `NSScrollView`s (removing `.prefix(4)` / `.prefix(2)` / `.prefix(3)` truncation and `max(8, chCursorY)` overlap, `F5`).
   - When `uninstallConfirmOpen` is `true`, hide/disable the underlying tab scroll view so `uninstall_confirm_box` never overlaps clickable buttons, and automatically reset `uninstallConfirmOpen = false` when switching tabs (`F5`).
   - Write `${stateDir}/onboarding-shown` only after `SettingsPanel` is actually ordered front (`openSettingsPanel()`) in interactive mode (`F5`).
   - In `handlePasteHubInvite()`, read `NSPasteboard.general.string(forType: .string)` first, falling back to `clipboardString` only when the system pasteboard is empty in headless test mode (`F47`).
   - In `SettingsPanel → Human Channels`, record `channelFieldDrafts` only when the user edits a text field (`controlTextDidChange`), and clear clean drafts on incoming `settings` frames so a server-populated `chatId` from Telegram pairing is displayed and preserved on subsequent `Save` (`F48`).
4. **Bounded UI IPC Frames (`< 256 KiB` Serialized UTF-8 Budget) & Global Unread Count (`F26, F27`)**:
   - In `daemon.rs`, compute `unread_count` via `SELECT COUNT(*) FROM local_signals WHERE acknowledged_at_ms IS NULL ...` **before** applying `LIMIT 80`, and sort multi-hub activity rows by `created_at_ms DESC, signal_id ASC` (`F27`).
   - Enforce a hard **`262,144`-byte (`256 KiB`) serialized UTF-8 byte budget** across **every** NDJSON frame emitted to `cito-ui` (`state`, `directory`, `settings`, `banner`) via a multi-stage bounding pipeline followed by a mandatory final serialized-byte check before writing to `ui_stdin`:
     1. **Field-level UTF-8 byte caps**: truncate `activity[i].message` to `512` UTF-8 bytes, `about` to `256` UTF-8 bytes, and `title` / `cwd` / `summary` to `128` UTF-8 bytes (on valid UTF-8 char boundaries, appending `"…"`).
     2. **Collection-level caps (preserving authoritative total counts in `unread_count` and `total_online`)**: cap `activity` at `80` items, `peers` at `100` items, `sessions` / `listeners` at `64` items, `my_remote_listeners` at `64` items, and `available` at `32` items.
     3. **Progressive Shedding Loop & Final Guard**: if `serde_json::to_vec(&frame).len() > 262_144`, progressively trim trailing items from `activity`, then `peers` / `my_remote_listeners` / `available`, and finally `sessions` / `listeners` (setting `"truncated": true` on the frame) until the serialized byte length is strictly `<= 262,144` bytes (verified by a regression test with `activity: []` and 150 multibyte-emoji listeners, `F26`).

---

## 6. SQLite Schemas (100% Column-Compatible with `v0.15.0`–`v0.17.0`), Non-Destructive Upgrade & Security Authority (`F1, F6–F10, F15, F16, F24, F30, F31, F39–F41, F61, F64`)

### 6.1 Pinned Baseline & Exact `${stateDir}/state.sqlite` Schema (`user_version = 4`, Non-Destructive Additive Migration)
All public CLI flags/outputs, MCP tools (`cito_listen`, `cito_unlisten`, `cito_find`, `cito_send`, `cito_inbox`), HTTP `/v1/rpc` & `/v1/events` SSE frames, and the 27-op UI IPC schemas preserve 100% wire compatibility with the baseline commit `8c5b872` (`Stage 3 v0.15.0`) + `95b1369` (`Stage 5 v0.17.0`: `setup_remove_app` & `setup_uninstall_all`), with only the explicit additions documented in this specification.
`state.sqlite` preserves the exact existing `user_version = 4` table and column definitions from `src/mcp/state.ts:62-88` (using `CREATE TABLE IF NOT EXISTS` and `ALTER TABLE ... ADD COLUMN` inside a `BEGIN IMMEDIATE` transaction so existing `user_version == 4` databases are never dropped or lost):
- `local_listeners (hub TEXT NOT NULL DEFAULT 'default', listener_id TEXT NOT NULL, public_id TEXT NOT NULL DEFAULT '', tag TEXT, principal_id TEXT NOT NULL, owner_user TEXT NOT NULL, host_id TEXT NOT NULL, host_name TEXT NOT NULL, harness TEXT NOT NULL CHECK(length(harness) > 0), session_id TEXT NOT NULL, endpoint_pid INTEGER, cwd TEXT NOT NULL, repo TEXT, branch TEXT, title TEXT NOT NULL, about TEXT NOT NULL, scope TEXT NOT NULL CHECK(scope IN ('self','workspace')), status TEXT NOT NULL CHECK(status IN ('ACTIVE','REBINDING','UNLISTENED','FENCED_INACTIVE')), binding_generation INTEGER NOT NULL DEFAULT 0, owner_token TEXT NOT NULL DEFAULT '', persisted_seq INTEGER NOT NULL DEFAULT 0, consecutive_probe_failures INTEGER NOT NULL DEFAULT 0, replay_gap_detected INTEGER NOT NULL DEFAULT 0, replay_gap_range TEXT, dispatcher_token TEXT, lease_until_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (hub, listener_id))`
- `local_signals (hub TEXT NOT NULL DEFAULT 'default', signal_id TEXT NOT NULL, listener_id TEXT NOT NULL, seq INTEGER NOT NULL, binding_generation INTEGER NOT NULL, from_user TEXT NOT NULL, from_host_name TEXT NOT NULL, from_harness TEXT, from_listener_id TEXT, from_title TEXT NOT NULL, from_about TEXT, message TEXT NOT NULL, held_reason TEXT, in_reply_to TEXT, created_at TEXT NOT NULL, expires_at_ms INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE','EXPIRED_UNSENT')), batch_id TEXT, batch_expires_at_ms INTEGER, dispatcher_token TEXT, queue_written INTEGER NOT NULL DEFAULT 0, acknowledged_at_ms INTEGER, batch_card TEXT, PRIMARY KEY (hub, signal_id))`
- `local_sent_idempotency (hub TEXT NOT NULL DEFAULT 'default', dedup_id TEXT NOT NULL, request_hash TEXT NOT NULL, signal_id TEXT NOT NULL, seq INTEGER NOT NULL DEFAULT 0, to_listener_id TEXT NOT NULL, status TEXT NOT NULL, held_reason TEXT, expires_at_iso TEXT NOT NULL, expires_at_ms INTEGER NOT NULL, PRIMARY KEY (hub, dedup_id))`
- `daemon_owner (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), pid INTEGER NOT NULL, port INTEGER NOT NULL)`
- **Additive Table (`F10`)**: `CREATE TABLE IF NOT EXISTS pending_hub_acks (hub TEXT NOT NULL, node_id TEXT NOT NULL, signal_id TEXT NOT NULL, channel_id TEXT NOT NULL, acked_at_ms INTEGER NOT NULL, PRIMARY KEY (hub, node_id, signal_id))`.

### 6.2 Exact `${dataDir}/hub.sqlite` Schema (Preserving All Existing Columns from `src/hub/engine.ts:240-335`)
`hub.sqlite` preserves every existing table and column name from `src/hub/engine.ts:240-335` via `CREATE TABLE IF NOT EXISTS` and additive `ALTER TABLE`:
- `hub_meta (key TEXT PRIMARY KEY, int_val INTEGER NOT NULL DEFAULT 0, json_val TEXT)`
- `hub_kv (key TEXT PRIMARY KEY, revision INTEGER NOT NULL, created_ms INTEGER NOT NULL, value BLOB NOT NULL, principal_id TEXT, scope TEXT)`
- `hub_signals (seq INTEGER PRIMARY KEY AUTOINCREMENT, subject TEXT NOT NULL, msg_id TEXT UNIQUE, signal_id TEXT, created_ms INTEGER NOT NULL, data BLOB NOT NULL, acked INTEGER NOT NULL DEFAULT 0, claimed_by_channel TEXT, claim_until_ms INTEGER, acked_at_ms INTEGER)`
- `hub_plugin_services (plugin_id TEXT PRIMARY KEY, config_json TEXT NOT NULL DEFAULT '{}', service_state_json TEXT NOT NULL DEFAULT '{}', updated_ms INTEGER NOT NULL DEFAULT 0)`
- `hub_human_channels (principal_id TEXT NOT NULL, plugin_id TEXT NOT NULL, owner_user TEXT NOT NULL DEFAULT 'local', enabled INTEGER NOT NULL DEFAULT 1, user_config_json TEXT NOT NULL DEFAULT '{}', display_label TEXT, last_delivered_seq INTEGER NOT NULL DEFAULT 0, updated_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (principal_id, plugin_id))`
- `hub_channel_deliveries (plugin_id TEXT NOT NULL, principal_id TEXT NOT NULL, signal_id TEXT NOT NULL, seq INTEGER NOT NULL, status TEXT NOT NULL, external_msg_id TEXT, from_listener_id TEXT, from_user TEXT, from_title TEXT, delivered_ms INTEGER NOT NULL, PRIMARY KEY (plugin_id, principal_id, signal_id))`
- `hub_pairing_codes (code TEXT PRIMARY KEY, plugin_id TEXT NOT NULL, principal_id TEXT NOT NULL, owner_user TEXT NOT NULL, instruction TEXT, created_ms INTEGER NOT NULL, expires_ms INTEGER NOT NULL, consumed_ms INTEGER)` (with `ALTER TABLE hub_pairing_codes ADD COLUMN instruction TEXT`, `F32, F41`)
- `hub_audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, principal_id TEXT NOT NULL, action TEXT NOT NULL, target_id TEXT NOT NULL, detail_json TEXT)`
- **Additive Tables (`F15, F18`)**:
  - `CREATE TABLE IF NOT EXISTS hub_channel_delivery_msgs (plugin_id TEXT NOT NULL, principal_id TEXT NOT NULL, external_msg_id TEXT NOT NULL, signal_id TEXT NOT NULL, from_listener_id TEXT NOT NULL, PRIMARY KEY (plugin_id, principal_id, external_msg_id))` (`F15`)
  - `CREATE TABLE IF NOT EXISTS hub_processed_replies (plugin_id TEXT NOT NULL, update_id TEXT NOT NULL, processed_at_ms INTEGER NOT NULL, PRIMARY KEY (plugin_id, update_id))` (`F18`).

### 6.3 Concrete Specifications & Acceptance Checks for `F8, F24, F30, F31, F39, F40, F61, F64`
1. **`F8` (SSE Unexpected Disconnect & Reconnect)**:
   - When the `/v1/events` SSE response stream ends (`EOF`), closes, or emits a socket error while the `HubClient` is not explicitly closed, `HubClient` immediately emits status `"reconnecting"`, closes the stale HTTP request, and enters an exponential-backoff reconnect loop (`100ms, 250ms, 500ms, 1000ms`), re-establishing `/v1/events`, replaying missed signals via `consumer_backlog`, and emitting `"reconnected"` once ready.
   - *Acceptance check*: Forcibly destroy the server-side SSE socket while keeping the HTTP server open; verify `HubSlot` transitions to `reconnecting` $\to$ `connected` and delivers a signal published during the disconnect window.
2. **`F24` (Hub 30-Minute Retention & Deduplication Window)**:
   - `stream_update` persists `max_age` (default `1_800_000_000_000` ns = `1,800,000` ms = 30m) and `duplicate_window` (default `1,800,000` ms = 30m) in `hub_meta`. Before `stream_publish` and `consumer_backlog`, `HubEngine` executes `DELETE FROM hub_signals WHERE created_ms < ?` (`now - max_age_ms`), and `msg_id` deduplication checks only rows with `created_ms >= now - duplicate_window_ms`.
   - *Acceptance check*: Publish a signal with `msg_id: "m1"` at $t_0$; advance clock or set `max_age` / `duplicate_window` to `50 ms`; after `80 ms`, verify `m1` is pruned from `hub_signals` and re-publishing `msg_id: "m1"` succeeds as a new sequence number.
3. **`F30` (Async-Signal-Safe POSIX Signal Handler)**:
   - The `SIGTERM` / `SIGINT` `sigaction` handler in Rust performs **only** `SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst)` and a 1-byte `libc::write` to a self-pipe (zero `Mutex` locking, zero heap allocation, zero `fs::remove_file` inside the signal handler). A dedicated signal-watcher thread or poll loop reads the self-pipe, acquires locks in normal thread context, unlinks socket files, and exits cleanly.
4. **`F31` (`deploy/hub/Dockerfile`, `docker-compose.yml` & `/health` Contract)**:
   - `deploy/hub/Dockerfile` uses `rust:1-bookworm` builder $\to$ `debian:bookworm-slim` runtime running `cito hub`.
   - `cito hub` treats blank environment variables (`CITO_HUB_TOKEN=""`, `CITO_OIDC_ISSUER=""`, `CITO_OIDC_CLIENT_ID=""`) as unset (`None`), while still rejecting an explicit empty CLI flag (`--token ""`), so `docker compose -f deploy/hub/docker-compose.yml up` boots cleanly in all 3 modes (`none`, `token`, `oidc`). `docker-compose.yml` forwards `CITO_PLUGIN_TELEGRAM_BOT_TOKEN: ${CITO_PLUGIN_TELEGRAM_BOT_TOKEN:-}`.
   - `GET /health` on `cito hub` returns `{"ok": true, "pid": <u32>, "version": "0.18.0", "auth_mode": "none"|"token"|"oidc"}`.
5. **`F39` & `F40` (Hub Auth Alias Canonicalization & Live `user` Reload)**:
   - When `setup` writes or updates a Hub entry in `config.json`, it writes `"authMode"` and deletes any legacy `"auth"` property on that entry (`F39`).
   - `reload_hub_slots_from_config` compares `(nats_url, auth_mode, token, issuer, client_id, user, disabled)`: if `user` changed on a `none` or `token` hub, the slot is torn down and recreated with the new `user` identity immediately (`F40`).
6. **`F61` & `F64` (Direct `cito daemon` Stale-Version Handover & `shutdown` Host-Hub Race)**:
   - When `cito daemon` is invoked directly, if an existing daemon responds on `/health` for the same `state_dir`:
     - If `health.version == Some(CITO_VERSION)`, `cito daemon` exits `0` (already running current version);
     - If `health.version != Some(CITO_VERSION)` (stale or missing version), `cito daemon` sends `SIGTERM` to `health.pid`, waits up to `5_000 ms` for termination, and takes over the daemon lock (`F61`).
   - In `CitoService::shutdown`, `self.closing` is set to `true` **before** awaiting in-flight `calls`, `sync_host_hub` checks `if self.closing { return; }` before and after binding the LAN `hostHubServer`, and `hostHubServer.close()` is executed **after** all in-flight `calls` drain (`F64`).

### 6.4 OIDC JWKS Verification, Row-Level Security & Hub Admin Authority (`F1, F6, F7`)
1. **OIDC JWT Verification**:
   - Discovers `jwks_uri` from `<issuer>/.well-known/openid-configuration`, caches RSA (`RS256`) and EC (`ES256`) public keys with automatic key-rotation refresh on unknown `kid`, verifies signature, `iss == issuer`, `aud` containing `clientId`, and `exp > now`, and derives `principal_id = "oidc:" + iss + "|" + sub` and `owner_user` from `preferred_username` / `email` local-part / `sub`.
2. **Row-Level Security (`RLS`) across RPCs & SSE (`F6`)**:
   - **`stream_get_message`**: Loads the target row from `hub_signals` and verifies that either `row.principal_id == caller.principal_id`, `row.sender_principal_id == caller.principal_id`, or the target registration in `hub_kv` has `scope == "workspace"`; otherwise returns `403 FORBIDDEN`.
   - **SSE `/v1/events` (`msg` and `nc` ack frames)**: In OIDC mode, `cito.ack.<node_id>` (`nc`) frames are delivered **only** to SSE clients whose `caller.principal_id` owns `<node_id>` (matching the `principal_id` stored on the Node's `hub_kv` registration).
3. **Hub Admin Authority for `scope: "hub"` Channel Credentials (`F7`)**:
   - In `authMode == "none"` or `"token"`, any authenticated caller can configure `scope: "hub"` fields (`botToken`).
   - In `authMode == "oidc"`, `scope: "hub"` fields (`hub_plugin_services`) are writable **only** via Hub server environment variables (`CITO_PLUGIN_TELEGRAM_BOT_TOKEN`) or by an OIDC user listed in `CITO_OIDC_ADMIN_USERS` (comma-separated usernames/subs). If an ordinary non-admin OIDC user passes non-empty `hub_config` in `human_channels_configure`, the Hub rejects the Hub-scope mutation with `403 FORBIDDEN` while allowing the user to save their own `user_config` (`chatId`). `human_channels_list` never returns raw `scope: "hub"` secret values (`botToken` is omitted/masked, returning `hub_provided: true`).

---

## 7. Brand Contrast, Website (`site/`) & Package/Test Retirement Contract (`F3, F4, F5, F42, F43, F50, F51`)

### 7.1 Self-Contained High-Contrast Brand Assets (`README.md` & `site/assets/brand/`, `F3`)
- Both `site/assets/brand/cito-logo-dark.svg` and `site/assets/brand/cito-logo-light.svg` render the full `[C) Cito` lockup inside a self-contained dark graphite anodized pill (`<rect x="1" y="1" width="130" height="34" rx="9" fill="#14171F" stroke="#262B38" stroke-width="1"/>`) with `#F3F4F6` wordmark text and `#F59E0B` amber core (`contrast ratio = 14.8:1` against `#14171F`), guaranteeing complete legibility on `#0d1117` dark backgrounds, `#ffffff` light backgrounds, and mismatched `(OS Light + GitHub Dark)` or `(OS Dark + GitHub Light)` combinations.
- `site/assets/brand/cito-mark.svg` includes its `<rect x="1" y="1" width="34" height="34" rx="8" fill="#14171F" stroke="#262B38" stroke-width="1"/>` squircle backing so the `#F3F4F6` C-arc is visible on any background (`F3`).

### 7.2 Root `package.json` & Native Test Suite Architecture (`F1, F43, F46, F50`)
1. **Root `package.json` Contract**:
   - `"version": "0.18.0"`, `"license": "MIT"`, `"type": "module"`.
   - `"bin": { "cito": "bin/cito", "cito-mcp": "bin/cito-mcp" }` (`cito-claude-hook` removed from top-level `bin`).
   - `"dependencies": {}` (zero runtime Node dependencies).
   - `"devDependencies"`: `typescript`, `vitest`, `@types/node`, `@modelcontextprotocol/sdk`, `jose`, `zod` (used by the Vitest test harness).
   - `"files": ["bin", "plugins", "README.md", "LICENSE"]` (`main`, `types`, `exports`, and root `pi` fields removed; `plugins/pi/package.json` owns `"pi": { "extensions": ["./cito-bridge.ts"] }`).
   - `"scripts"`:
     - `"compile"`: builds `src/macos/cito-ui.swift` $\to$ `bin/cito-ui-darwin-arm64` + `plugins/human/bin/cito-ui`, builds `crates/cito-native` (`--release`) $\to$ `bin/cito-native-darwin-arm64` + `plugins/{pi,codex,claude,telegram}/bin/*` (including `plugins/claude/bin/cito-claude-hook`), strips and ad-hoc signs all binaries, and removes `crates/cito-native/target`.
     - `"typecheck"`: `tsc --noEmit`.
     - `"test"`: `vitest run`.
     - `"prepublishOnly"`: `npm run typecheck && npm run compile && npm test`.
2. **Test Suite Migration to Native Binaries (`tests/support/native-harness.ts`)**:
   - Delete `src/{mcp,hub,setup,ui,shared,adapters,auth-callout}/`, `bin/loader.js`, and legacy `plugins/{index.ts,*/index.ts,claude/channel.ts,claude/hook-cli.ts}`.
   - Implement `tests/support/native-harness.ts` driving the compiled Rust `bin/cito` (`daemon`, `hub`, `mcp-stdio`, `setup`) and `plugins/*/bin/*` executables so all 33 Vitest test files (`260+` existing tests + new regression tests for `F1`–`F64`) run against the actual production binaries.
   - In `tests/mcp/lifecycle.test.ts:299` (`F50`), assert `rows.length === 1 && typeof rows[0]?.acknowledged_at_ms === "number"`.
   - In `tests/claude-hook/cli.test.ts`, invoke `plugins/claude/bin/cito-claude-hook` directly (`F43, F44, F35, F62`).

---

## 8. Stage 6 (`v0.18.0`): Built-in Plugin Specifications (`pi`, `codex`, `claude`, `telegram`), Setup Precision & Complete `F1`–`F64` Closure Contract

### 8.1 Built-in Harness & Channel Plugins (`plugins/{pi,codex,claude,human,telegram}/`)
1. **Pi Plugin (`plugins/pi/` — Standard Pi Package Mode, `F2, F29, F58`)**:
   - Directory layout:
     - `plugins/pi/plugin.json`: `{"id":"pi","kind":"agent-harness","displayName":"Pi Coding Agent","tier":"stable","sfSymbol":"terminal.fill","tint":"amber","cliFlag":false,"command":["./bin/cito-plugin-pi"]}`
     - `plugins/pi/package.json`: `{"name":"@tefx/cito-pi","version":"0.18.0","private":true,"pi":{"extensions":["./cito-bridge.ts"]}}`
     - `plugins/pi/cito-bridge.ts`: Loaded in-process by Pi (`packages` in `~/.pi/agent/settings.json`). On load, calls `pi.registerMcpServer("cito", { command: "<stateDir>/bin/cito-mcp", args: [] })` (mandatory registration when `registerMcpServer` is provided on `pi`; no-op only on older test mocks without `registerMcpServer`). Reads `daemonPort` from `${stateDir}/config.json` (falling back to `CITO_DAEMON_PORT` or `4290`, `F29`). When binding `${stateDir}/sockets/pi-pid-<pid>.sock`, if `listen()` fails with `EADDRINUSE`, connects to the existing socket with a `250 ms` timeout: if the peer fails to respond to `{"op":"ping"}`, unlinks the stale socket file and retries `listen()` (`F58`).
   - `setup_apply`: registers the resolved absolute path of the installed `plugins/pi` directory (`<stateDir>/plugins/pi`) in `~/.pi/agent/settings.json` `"packages"`, removes any legacy `~/.pi/agent/extensions/cito-bridge.ts` file, removes any legacy `"npm:@tefx/cito"` package entry, and removes any duplicate `mcpServers.cito` entry from `~/.pi/agent/mcp/global.mcp.json` (`F2`).
   - `setup_remove`: removes `<stateDir>/plugins/pi` (and legacy `"npm:@tefx/cito"`) from `~/.pi/agent/settings.json` `"packages"` (deleting the `"packages"` key if empty while preserving all other keys).
2. **Codex Plugin (`plugins/codex/`, `F21, F22, F25, F33`)**:
   - `setup_apply` writes `[mcp_servers.cito]\ncommand = "<stateDir>/bin/cito-mcp"\nargs = []` using the expanded absolute path (`F21`), parsing `config.toml` with full support for multiline arrays (`[...]`) and multiline strings (`"""..."""` / `'''...'''`).
   - `setup_remove` removes `[mcp_servers.cito]` and all `[mcp_servers.cito.*]` sub-tables (`F25`).
   - `resolve_caller` extracts thread IDs from `_meta["openai/threadId"]`, `_meta.threadId`, or HTTP header `"x-codex-turn-metadata"` (JSON string or object containing `thread_id` / `session_id`), and never invents a synthetic `session-<pid>` fallback (`F22`).
   - `wake` probes candidate `codex` binaries in order: `CITO_CODEX_BIN` $\to$ `/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex` $\to$ `/Applications/Codex.app/Contents/Resources/codex` $\to$ `~/Applications/ChatGPT.app/...` $\to$ `codex` on `PATH` (`F33`).
3. **Claude Code Plugin & Relocated `cito-claude-hook` (`plugins/claude/`, `F21, F33, F35, F36, F44, F62`)**:
   - `cito-claude-hook` lives at `plugins/claude/bin/cito-claude-hook` (copied to `<stateDir>/plugins/claude/bin/cito-claude-hook` by `ensure_cli_shims`).
   - Both `setup_inspect`, `setup_apply`, and `setup_remove` match existing hook entries symmetrically via `cmd == "cito-claude-hook" || cmd.ends_with("/cito-claude-hook")` (`F33`), writing `<stateDir>/plugins/claude/bin/cito-claude-hook` and `<stateDir>/bin/cito-mcp` (`F21`).
   - `append_wake` and `drain_wakes` acquire an exclusive `flock(LOCK_EX)` on `${stateDir}/claude/wakes/<session>.lock` before opening, reading, writing, rotating, or unlinking `<session>.jsonl` (`F35`).
   - Before appending a JSONL line, if the existing file size is `> 0` and its last byte is not `b'\n'`, `append_wake` writes a leading `b'\n'` first so a prior torn line never corrupts the newly appended record (`F62`).
   - When resolving a Claude session without `CLAUDE_SESSION_ID`, `plugins/claude` binds the session ID recorded for the specific `endpoint_pid` (or open file descriptors / process start time) and rejects ambiguous workspace transcripts with `BINDING_REQUIRED` when multiple concurrent Claude processes share the same `cwd` (`F36`).
   - On `Stop` hook events, `cito-claude-hook` returns `{"decision":"block","reason":...}` **only** when at least one drained wake entry has `wake == true`; when all drained entries have `wake == false`, it returns `{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":...}}` (`F44`).
4. **Telegram Human Channel Plugin (`plugins/telegram/`, `F15–F19, F32, F41`)**:
   - Runs as a persistent NDJSON subprocess (`plugins/telegram/bin/cito-plugin-telegram`) managed by `HubEngine` over Contract 2 (§4.4), exiting within `< 100 ms` on `stdin EOF`.
   - Pairing codes are generated by `HubEngine` as 10-character lowercase Base32 strings (`cito-` + 10 chars = 48 bits of entropy, `F41`), with the formatted `@<botUsername>` instruction stored in `hub_pairing_codes.instruction` so subsequent `human_channels_list` calls return the exact bot username (`F32`).
   - Pairing and inbound replies require `chat.type == "private"` (`is_private_chat == true`) and verify `message.from.id == external_user_id` (`F17`).
   - Updates are processed individually inside a per-update `try/catch` block and deduplicated in `hub_processed_replies (plugin_id, update_id)` before advancing `cursor` (`F18`).
   - `send_reply_from_human` strips any `<hub>:` prefix before resolving `to_listener_id`, validates that the recipient registration exists in `hub_kv` and is neither `UNLISTENED` nor `EXPIRED`, and `resolve_reply_target` orders candidate notifications strictly by `d.delivered_ms DESC` (`F19`).

### 8.2 Setup, CLI, Config & Distribution Closure (`F4, F20, F23, F28, F34, F38, F42, F45, F46, F51, F53, F56, F59`)
1. **Targeted Setup & Per-File Mutex (`F20, F34, F38, F59`)**:
   - `apply_setup`:
     - When `enable_harnesses` is non-empty (e.g., clicking `Set Up` on a single app in `SettingsPanel` or running `cito setup --enable <id>`), invokes `setup_apply` **only** on the harnesses in `enable_harnesses`.
     - When called for Hub/hostHub/login-item changes (`hub`, `enable_hubs`, `disable_hubs`, `remove_hubs`, `host_hub`, `launch_at_login`) with empty `enable_harnesses`, updates `config.json` / `LaunchAgent` **without** invoking `setup_apply` on any harness.
     - Only a bare `cito setup` CLI invocation (with no `--enable`, `--disable`, `--hub`, `--enable-hub`, `--disable-hub`, `--remove-hub`, `--host-hub`, or `--login-item` flags) configures all `tier == "stable"` harnesses (`F20`).
   - `--dry-run` is checked before `--build-app`, `--uninstall`, and `--disable`, returning planned actions without modifying any file or stopping the daemon (`F34`).
   - Malformed JSON in `config.json` returns an explicit parse error before any file write (`F38`).
   - All config file read-modify-write mutations acquire an in-process per-path mutex and `flock` lockfile (`F59`).
2. **`CITO_CONFIG_PATH`, `LaunchAgent` & Full Daemon Shutdown on Uninstall (`F23, F28, F56`)**:
   - Both `load_config` and setup expand `~` on `CITO_CONFIG_PATH` (`F28`), and `write_launch_agent_plist` includes `<key>CITO_CONFIG_PATH</key><string>...</string>` whenever `CITO_CONFIG_PATH` differs from `${stateDir}/config.json` (`F56`).
   - `uninstall_setup` reads `daemonPort` from `config.json`, calls `unlisten_all` across all active local listeners, sends `quit` / `SIGTERM` to the running daemon and waits for process exit, and only then removes host integrations, `LaunchAgent`, `~/.cito/bin`, and (when `purge_data: true`) `~/.cito` and `~/Applications/Cito.app` (`F23`).
3. **CLI Flag Validation & Offline Caller Verification (`F45, F46, F53`)**:
   - `cito` and `cito-mcp` execute `bin/cito-native-darwin-arm64` directly (`F46`).
   - CLI `listen`, `unlisten`, `inbox`, `find`, and `send` validate all flags (`read_value` rejecting missing values or flag-as-value, rejecting unknown `--*` flags, supporting `--codex` / `--claude` on `listen`, `unlisten`, and `inbox`, supporting `-u` / `--user` on `find`, and rejecting `--session` without `--harness`, `F45`).
   - When `resolve_caller(require_open: false)` falls back to a cached PID entry, it invokes `verify_expected_identity(cached, meta)` before returning (`F53`).
4. **Website Responsive Layout, Copy Feedback & Live Release/Pages/Homebrew Tap (`F4, F42, F51`)**:
   - In `site/styles/main.css`, add `@media (max-width: 640px)` responsive rules for `.nav-inner` / `.nav-links` and change `.moment-card` grid columns to `minmax(min(100%, 19rem), 1fr)` (`F51`).
   - In `site/scripts/hero-preview.js`, set `"Copied ✓"` only after `await navigator.clipboard.writeText(cmd)` resolves, showing `"Copy failed"` on rejection or missing Clipboard API (`F42`).
   - Enable GitHub Pages (`gh api -X POST repos/Tefx/cito/pages -f build_type=workflow` and `enablement: true` in `.github/workflows/pages.yml`), set `runs-on: macos-15` in `.github/workflows/release.yml`, publish `v0.18.0` `Cito-macos-arm64.zip` + `.sha256`, write the real SHA-256 into `packaging/homebrew/cito.rb` (installing `cito` and `cito-mcp`), and push `Formula/cito.rb` to `Tefx/homebrew-tap` (`F4`).

---

## 9. Stage 7 (`v0.19.0` — Phase `v23`): Pure Rust Workspace Consolidation, Removal of Root Node/Vitest Scaffolding & `cargo test` Unification

### 9.1 Purpose & Clean-Slate Repository Layout
Once Phase `v22` verifies that all 33 historical Vitest suites and `F1`–`F64` regressions pass against the compiled native binaries, Phase `v23` removes the transitional root Node.js / Vitest scaffolding so the repository is a **100% pure Rust + Swift workspace** at the top level:

```text
cito/
├── Cargo.toml                        # Root Cargo workspace manifest (single source of build & test)
├── Cargo.lock                        # Cargo lockfile
├── crates/
│   └── cito-native/                  # Core + plugin binaries & native integration test suite
│       ├── Cargo.toml
│       ├── build.rs                  # Compiles src/macos/cito-ui.swift & stages bin/ + plugins/*/bin/
│       ├── src/
│       └── tests/                    # Full Rust integration & E2E test suite (`cargo test`)
├── src/macos/
│   └── cito-ui.swift                 # Native macOS AppKit UI
├── plugins/
│   ├── pi/                           # Only directory with package.json & .ts (loaded by Pi host, 0 npm deps)
│   │   ├── plugin.json
│   │   ├── package.json
│   │   ├── cito-bridge.ts
│   │   └── bin/cito-plugin-pi
│   ├── codex/
│   ├── claude/
│   ├── human/
│   └── telegram/
├── site/                             # Static product website (GitHub Pages)
├── packaging/homebrew/cito.rb        # Homebrew Formula
└── deploy/hub/                       # Standalone Hub Docker & deployment configs
```

### 9.2 Root Node/Vitest Scaffolding Removal & `cargo test` Migration Contract
1. **Complete Removal of Root Node.js / Vitest Artifacts**:
   - Delete root `package.json`, `package-lock.json`, `tsconfig.json`, `vitest.config.ts`, `node_modules/`, and `tests/**/*.ts` (including `tests/support/native-harness.ts`).
   - Only `plugins/pi/package.json` (`"private": true`, zero `dependencies` / `devDependencies`) and `plugins/pi/cito-bridge.ts` remain in the repository because Pi itself is a TypeScript host that loads `plugins/pi` as a standard Pi Package.
2. **Unified Build & Test via `cargo build --release` & `cargo test`**:
   - Promote/add a root `Cargo.toml` workspace (`members = ["crates/cito-native"]`).
   - `cargo test --workspace` executes the complete behavioral test suite in Rust (`crates/cito-native/tests/*.rs`), covering:
     - All `HubEngine` (`local://`, LAN `:4291`, standalone `cito hub` HTTP/SSE, `none`/`token`/`oidc` RSA/ECDSA JWKS verification, RLS, 30m retention/dedup pruning, `signal_claim_dispatch` atomicity, `signal_ack` offline reconciliation, and `HumanChannel` subprocess lifecycle) contracts;
     - All `DaemonService` (`state.sqlite` persistence, `cito_listen`/`cito_unlisten`/`cito_find`/`cito_send`/`cito_inbox`, stale-daemon handover, and all 27 `cito-ui` NDJSON IPC actions + `< 256 KiB` frame bounding) contracts;
     - All 5 built-in plugins (`plugins/{pi,codex,claude,human,telegram}` subprocess contracts, `cito-claude-hook` `flock` + torn-line isolation, Codex multiline TOML & wake, Pi `settings.json` `packages` registration, and `plugins/pi/cito-bridge.ts` socket/MCP registration verified via `node` subprocess when available);
     - Headless `bin/cito-ui` AppKit view-tree & interaction tests (`cardMenu`, `DirectoryPanel` `Directory | Activity` + `ComposerBar`, 3-Tab `SettingsPanel`, `WakeBanner`);
     - Static website (`site/`), brand SVG contrast, and Homebrew Formula (`ruby -c packaging/homebrew/cito.rb`) checks;
     - All `F1`–`F64` regression cases.
3. **CI Workflow & Documentation Alignment (`.github/workflows/release.yml`, `README.md`, `AGENTS.md`)**:
   - Update `.github/workflows/release.yml` to remove `actions/setup-node@v4` and `npm ci` / `npm run ...`, running pure `cargo test --workspace`, `cargo build --release`, `./bin/cito setup --build-app ./build`, and `/usr/bin/ruby -c packaging/homebrew/cito.rb`.
   - Bump `CITO_VERSION` to `"0.19.0"`, publish GitHub Release `v0.19.0`, and sync `packaging/homebrew/cito.rb` and `Tefx/homebrew-tap`.


