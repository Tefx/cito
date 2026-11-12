# Cito Architecture & Protocol Specification (V31 — Stage 13 `v0.21.2`: Declarative Layout, Human Lifecycle & Bounded Delivery Retry — M1–M5)

> **Normative Supersession Notice (`V31` / `v0.21.2`)**:
> This specification is the authoritative contract for Cito `v0.21.2` (Stage 13 — Phase `v31`). Section 15 supersedes conflicting layout, lifecycle, and delivery rules in Sections 1–14. Section 14 otherwise retains its presentation and interaction requirements. The Stage 10 (`v0.20.2`) two-stratum channel setup, independent scope saving, geometry checks, error handling, bridge readiness, and real daemon/UI integration contracts remain in force. All paths written with `~` in prose examples represent the user's home directory; runtime configuration files use expanded absolute paths.

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
  - Includes `channels: BTreeMap<String, ChannelAttachment>` where each `ChannelAttachment` has `{ channel_id, kind, host_id, host_name, delivery_mode: "notify_fanout" | "wake_exclusive", priority, state: "ACTIVE" | "UNLISTENED", owner_token?, updated_at_ms }`.
  - **Channel Ownership**: Local registrations store an `owner_token` on their channel attachment. Refreshing the same local binding retains its token; replacing that channel's owner rotates only its token. An active sibling channel preserves the Node's `binding_generation` and top-level `owner_token`, keeping pending signal generations and legacy sibling owners valid. Heartbeats, wake preflight/renewal, and captured-owner detach compare the local row against its channel token and Node generation, falling back to the top-level token only when the channel token is absent. With no active sibling, a replacement also advances the Node generation and replaces its top-level token.
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
  - Headless test hooks: `test_inspect`, `test_control`, `test_settings_input`, `test_composer_input`, `test_settings_key`, `test_directory_key`, `test_clipboard`.
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
   - Inbound `cito-ui -> daemon` action lines use a separate **`1,048,576`-byte (`1 MiB`) limit**, excluding the newline delimiter, matching HTTP `/ui` request bodies. This admits a 65,536-code-point message plus its JSON envelope. Drain oversized lines through the next newline or EOF without growing the buffer, emit an uncorrelated `error` with `INVALID_INPUT: UI action exceeds 1 MiB`, and continue processing subsequent actions on the same stream. An oversized frame has no trusted request identity, so it cannot settle a pending composer submission.

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
   - `GET /health` on `cito hub` returns `{"ok": true, "pid": <u32>, "version": "0.19.0", "auth_mode": "none"|"token"|"oidc"}`.
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
   - **`stream_get_message`**: Loads the target row from `hub_signals` and verifies that either the target registration in `hub_kv` is owned by `caller.principal_id` or `data.from.principal_id` matches `caller.principal_id`; otherwise returns `403 FORBIDDEN`.
   - **SSE `/v1/events` (`msg` and `nc` ack frames)**: In OIDC mode, `cito.ack.<node_id>` (`nc`) frames are delivered **only** to SSE clients whose `caller.principal_id` owns `<node_id>` (matching the `principal_id` stored on the Node's `hub_kv` registration).
3. **Hub Admin Authority for `scope: "hub"` Channel Credentials (`F7`)**:
   - In `authMode == "none"` or `"token"`, any authenticated caller can configure `scope: "hub"` fields (`botToken`).
   - In `authMode == "oidc"`, `scope: "hub"` fields (`hub_plugin_services`) are writable **only** via Hub server environment variables (`CITO_PLUGIN_TELEGRAM_BOT_TOKEN`) or by an OIDC user listed in `CITO_OIDC_ADMIN_USERS` (comma-separated usernames/subs). If an ordinary non-admin OIDC user passes non-empty `hub_config` in `human_channels_configure`, the Hub rejects the Hub-scope mutation with `403 FORBIDDEN` while allowing the user to save their own `user_config` (`chatId`). `human_channels_list` never returns raw `scope: "hub"` secret values (`botToken` is omitted/masked, returning `hub_provided: true`).

---

## 7. Brand Contrast, Website (`site/`) & Package/Test Retirement Contract (`F3, F4, F5, F42, F43, F50, F51`)

### 7.1 Self-Contained High-Contrast Brand Assets (`README.md` & `site/assets/brand/`, `F3`)
- Both `site/assets/brand/cito-logo-dark.svg` and `site/assets/brand/cito-logo-light.svg` render the full `[C) Cito` lockup inside a self-contained dark graphite anodized pill (`<rect x="1" y="1" width="130" height="34" rx="9" fill="#14171F" stroke="#262B38" stroke-width="1"/>`) with `#F3F4F6` wordmark text and `#F59E0B` amber core (`contrast ratio = 14.8:1` against `#14171F`), guaranteeing complete legibility on `#0d1117` dark backgrounds, `#ffffff` light backgrounds, and mismatched `(OS Light + GitHub Dark)` or `(OS Dark + GitHub Light)` combinations.
- `site/assets/brand/cito-mark.svg` includes its `<rect x="1" y="1" width="34" height="34" rx="8" fill="#14171F" stroke="#262B38" stroke-width="1"/>` squircle backing so the `#F3F4F6` C-arc is visible on any background (`F3`).

### 7.2 Historical Stage 6 Transition (`F1, F43, F46, F50`)

The following transition was used for v0.18.0 and is superseded by Section 9. It does not describe the current build, test, or distribution entrypoints. The current product version is `0.21.2` in `crates/cito-native/Cargo.toml`; only `plugins/pi/package.json` remains as a host extension manifest.
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
     - `plugins/pi/package.json`: `{"name":"@tefx/cito-pi","version":"0.19.0","private":true,"pi":{"extensions":["./cito-bridge.ts"]}}`
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
   - `send_reply_from_human` strips any `<hub>:` prefix before resolving `to_listener_id` (or `to`), validates that the recipient registration exists in `hub_kv` and is neither `UNLISTENED` nor `EXPIRED`, and `resolve_reply_target` orders candidate notifications strictly by `d.delivered_ms DESC` (`F19`). The daemon `/ui` action and channel `inbound_reply` share HubEngine reply validation and publication: private targets require the same principal, self-send is forbidden, and the reply must contain 1–65,536 Unicode code points after control-character filtering. The UI action accepts `message` (or `text`) and optionally `in_reply_to` (or `signal_id`); publishing the reply and acknowledging that source signal on the caller's Human Node commit atomically, with acknowledgement events and channel `on_acked` notifications sent after commit.

### 8.2 Setup, CLI, Config & Distribution Closure (`F4, F20, F23, F28, F34, F38, F42, F45, F46, F51, F53, F56, F59`)
1. **Targeted Setup & Per-File Mutex (`F20, F34, F38, F59`)**:
   - `apply_setup`:
     - When `enable_harnesses` is non-empty (e.g., clicking `Set Up` on a single app in `SettingsPanel` or running `cito setup --enable <id>`), invokes `setup_apply` **only** on the harnesses in `enable_harnesses`.
     - When called for Hub/hostHub/login-item changes (`hub`, `enable_hubs`, `disable_hubs`, `remove_hubs`, `host_hub`, `launch_at_login`) with empty `enable_harnesses`, updates `config.json` / `LaunchAgent` **without** invoking `setup_apply` on any harness.
     - Only a bare `cito setup` CLI invocation (with no `--enable`, `--disable`, `--hub`, `--enable-hub`, `--disable-hub`, `--remove-hub`, `--share-hub`, `--no-share-hub`, `--share-port`, or `--launch-at-login` flags) configures all `tier == "stable"` harnesses (`F20`).
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
│       ├── build.rs                  # Compiles and signs src/macos/cito-ui.swift
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
   - The root `Cargo.toml` workspace uses `members = ["crates/cito-native"]` and `resolver = "2"`, with one root `Cargo.lock`.
   - `build.rs` compiles and ad-hoc signs the Swift UI for both test and release profiles. `.cargo/config.toml` installs `bin/cito-linker` as the macOS linker hook: it calls the system linker and atomically stages completed release binaries into `bin/` and `plugins/*/bin/`. A build script runs before the final Rust link, so Rust staging happens in this post-link hook rather than a recursive Cargo invocation.
   - `crates/cito-native/tests/native.rs` owns the integration target; its support modules exercise Cargo-provided `CARGO_BIN_EXE_*` executables. Existing private Hub/Auth/Login cases and TLS fixtures live in `crates/cito-native/tests/hub/` and remain wired to their unit-test modules.
   - `bin/cito-test-runner` isolates unit-test processes. Integration fixtures clear inherited environment, allocate temporary homes and private state/config paths, reserve ephemeral ports, mock host launch/browser commands, and run the AppKit companion headlessly. A shared mutex serializes subprocess scenarios, including multi-hub scenarios. Keep incremental `target/` until verification finishes.
   - Local Node.js with type stripping is needed only for the dependency-free Pi bridge contract and website JavaScript behavior tests; there is no npm install or root Node toolchain.
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

---

## 10. Stage 8 (`v0.20.0` — Phase `v24`): Full-Lifecycle UX, State-Machine & Reconfiguration Contract (`L1`–`L17`)

### 10.1 Overview & Design Guardrails (`[SHARP]` vs. `[FLEXIBLE]`)
Stage 8 (`v0.20.0` — Phase `v24`) closes all 17 lifecycle, state-machine, and reconfiguration gaps (`L1`–`L17`) across the Human Channel setup/pairing/reconfiguration flow (`L1`–`L8`), the Agent App onboarding/verify/repair/disconnect/reset flow (`L9`–`L13`), and the Multi-Hub / LAN Share / Directory empty-state lifecycle (`L14`–`L17`):
- **`[SHARP]` Zero Concrete Agent or Channel Strings in Core & UI**:
  - `crates/cito-native/src/core/` and `src/macos/cito-ui.swift` MUST NOT contain any hardcoded branches, identifiers, or UI strings for `pi`, `codex`, `claude`, `telegram`, `webhook`, `slack`, `teams`, `email`, `discord`, or `feishu`.
  - Mechanical gate: `grep -rnE -i '\b(pi|codex|claude|telegram|webhook|slack|teams|email|discord|feishu)\b' crates/cito-native/src/core/ src/macos/` MUST exit with `0` matches.
  - All harness- or channel-specific setup instructions, placeholders, field help texts, and post-setup/post-disconnect hints MUST be declared in `plugins/<id>/plugin.json` and projected generically through `daemon.rs` and `hub.rs` DTOs.
- **`[SHARP]` Backward-Compatible Headless Inspection Contract**:
  - All existing `NSUserInterfaceItemIdentifier` values and `test_inspect` top-level keys (`items`, `directory_items`, `settings_items`, `settings_open`, `settings_tab`, `directory_open`, `directory_mode`, `composer_open`) in `src/macos/cito-ui.swift` MUST remain intact so all existing `ui::*`, `daemon::*`, `hub::*`, and `plugins::*` tests continue to pass alongside new `L1`–`L17` regression tests.
- **`[FLEXIBLE]` Scrollable Card Containers**:
  - Card heights in `SettingsPanel` (`Agent Apps`, `Human Channels`, `Hubs & Network`) may expand dynamically to accommodate setup guides, pairing instructions, and inline edit forms because `mountSettingsCards(_:in:)` wraps each tab's content in an `NSScrollView` and shifts coordinates so `minY >= 8`.

---

### 10.2 Declarative `plugin.json` Lifecycle Metadata & Separation of Secret Redaction vs. UI Visibility (`L1`, `L10`)

1. **`[SHARP]` Extended `plugin.json` Manifest Schema (`plugins/{pi,codex,claude,telegram}/plugin.json`)**:
   - **`agent-harness` plugins (`plugins/{pi,codex,claude}/plugin.json`)** add declarative lifecycle hints:
     - `postSetupHint` (`string`, optional):
       - `plugins/pi/plugin.json`: `"Connected · Run /reload in open Pi sessions, then click + Listen"`
       - `plugins/codex/plugin.json`: `"Connected · Start a new Codex session, then click + Listen"`
       - `plugins/claude/plugin.json`: `"Connected · Restart open Claude Code sessions, then click + Listen"`
     - `postDisconnectHint` (`string`, optional):
       - `plugins/pi/plugin.json`: `"Disconnected · Active Pi listeners stopped"`
       - `plugins/codex/plugin.json`: `"Disconnected · Active Codex listeners stopped"`
       - `plugins/claude/plugin.json`: `"Disconnected · Active Claude Code listeners stopped"`
   - **`human-channel` plugins (`plugins/telegram/plugin.json`)** add declarative setup guidance and field metadata:
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
       "setupGuide": [
         "1. Message @BotFather on Telegram (/newbot) to create a bot and copy its Bot Token.",
         "2. Under 1. HUB GATEWAY · ONCE PER HUB, paste the Bot Token and click Save Gateway Token.",
         "3. Under 2. YOUR PERSONAL NODE, click Pair Account… and send the generated /start command to your bot — Chat ID fills in automatically. Skip for Now leaves pairing optional."
       ],
       "fields": [
         {
           "key": "botToken",
           "scope": "hub",
           "label": "Bot Token",
           "type": "secret",
           "required": true,
           "placeholder": "123456:ABC-DEF...",
           "configuredPlaceholder": "•••••••• (Saved — enter new token to replace)",
           "helpText": "From @BotFather on Telegram"
         },
         {
           "key": "chatId",
           "scope": "user",
           "label": "Chat ID",
           "type": "string",
           "required": true,
           "placeholder": "Paired via /start or enter Chat ID",
           "helpText": "Auto-filled when you pair via /start, or enter your numeric Chat ID"
         }
       ],
       "pairing": {
         "supported": true,
         "actionLabel": "Pair Account…",
         "helpText": "Generates a 10-minute /start command to link your private Telegram chat."
       }
     }
     ```
2. **`[SHARP]` Separation of Wire Secret Redaction (`redacted`) vs. UI Field Visibility (`hidden`) (`hub.rs` `channel_list()`, `L1`)**:
   - In `crates/cito-native/src/core/hub.rs` `channel_list(&self, caller: &Value)`:
     - Compute `let is_admin = admin(Some(caller)).is_ok();` once per `channel_list` call.
     - For each field `f` in `fields(&plugin)`:
       - `let hub = s(f, "scope") == "hub";`
       - `let val = if hub { &cfg[s(f, "key")] } else { &user[s(f, "key")] };`
       - `let is_secret = s(f, "type") == "secret";`
       - **`hidden: bool`**: `hub && !is_admin` — ONLY non-admin OIDC callers hide `scope: "hub"` fields. Local/token callers and OIDC Hub administrators receive `hidden: false` so `cito-ui.swift` renders the `NSSecureTextField` (`channel_field:<channel_id>:<key>`) in `SettingsPanel → Human Channels`.
       - **`redacted: bool`**: `hidden || is_secret` — Secret values (`type == "secret"`) and non-admin hub-scoped values are NEVER sent in cleartext over the wire.
       - **`value: String`**: `""` whenever `redacted` is `true`; otherwise the stored string representation (`val.as_str()`, `""` if `null`, or `val.to_string()`).
       - **`configured: bool`**: `configured(val)` (`true` when the stored value in `hub_plugin_services` or `hub_human_channels` is non-empty, even when `value` on the wire is `""` due to `redacted == true`).
       - **`configured_placeholder: String`**: `f["configuredPlaceholder"].as_str().unwrap_or(if is_secret { "•••••••• (Saved — enter new value to replace)" } else { f["placeholder"].as_str().unwrap_or("") })`.
       - **`help_text: Option<String>`**: `f["helpText"].as_str()`.
     - On each channel item in `channel_list()`, include:
       - `"setup_guide"`: `plugin.manifest.get("setupGuide").cloned().unwrap_or(Value::Null)` (array of strings or null).
       - `"pairing_help_text"`: `plugin.manifest["pairing"]["helpText"].as_str()`.

---

### 10.3 Secret Patch Semantics (`Retain` vs. `Replace` vs. `Clear`) & Non-Mutating `channel_test` (`L2`)

1. **`[SHARP]` UI `collectValues()` Dirty-Only Secret Submission (`src/macos/cito-ui.swift`, `L2`)**:
   - In `SettingsPanel → Human Channels`, iterate only over visible fields (`for f in ch.fields where f.hidden != true`) so non-admin callers never submit hidden `scope: "hub"` fields:
     - Let `fid = "channel_field:\(chId):\(f.key)"` and `draft = self.channelFieldDrafts[self.draftKey(fid)]`.
     - When `f.type == "secret"` and `f.configured == true`:
       - If `let d = draft, !d.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty`: include `vals[f.key] = d` (user explicitly typed a replacement secret).
       - Else if `draft == nil`, `let fv = f.value, !fv.isEmpty`: include `vals[f.key] = fv` (preserves compatibility with synthetic headless UI test fixtures that pass a non-redacted `value` in mock `settings` frames).
       - Otherwise (`draft == nil` or `draft == ""`, and `f.value` is `nil` or `""`): **omit `f.key` from `vals`** so `Save`, `Pair Account…`, and `Send Test` retain the Hub's stored secret credential without overwriting it with `""`.
     - When `f.type != "secret"` or `f.configured == false`:
       - Include `vals[f.key] = draft ?? self.settingsFieldControls[fid]?.stringValue ?? f.value ?? ""`.
2. **`[SHARP]` Hub-Side Secret Merge Guard (`crates/cito-native/src/core/hub.rs`, `L2`)**:
   - Replace blind `merge(&mut cfg, patch)` on channel configs with `merge_channel_config(&plugin, scope, &mut cfg, patch, allow_clear_secret)`:
     - `allow_clear_secret` is `p["clear_hub_config"] == true || p["reset_hub"] == true || p["reset"] == true`.
     - For each `(k, v)` in `patch.as_object()`:
       - Look up the field descriptor `f` in `fields(&plugin)` with `s(f, "key") == k && s(f, "scope") == scope`.
       - If `s(f, "type") == "secret"` and `(v.is_null() || v.as_str() == Some(""))` and `!allow_clear_secret`: skip inserting empty/null so the existing stored secret in `base[k]` is preserved.
       - Otherwise set `base[k] = v.clone()`.
3. **`[SHARP]` Non-Mutating `human_channels_test` (`crates/cito-native/src/core/hub.rs`, `L2`)**:
   - In `HubEngine::human_rpc`:
     - Enforce `hub_patch_guard(p, Some(caller))?` before both `human_channels_test` and `human_channel_pair_start` (non-admin OIDC callers supplying a non-empty `hub_config` still receive `FORBIDDEN: Hub administrator required`).
     - When `op == "human_channels_test"`:
       - Validate `p["hub_config"]` and `p["user_config"]` via `validate_config`, load `(mut cfg, _)` from `service(c, id)` and `mut user` from `hub_human_channels`, and apply `merge_channel_config` **in memory only** without calling `save_service(c, id, &cfg)` or mutating SQLite.
       - Ensure the plugin subprocess is running (`self.ensure_process(&plugin)?`) and invoke `process.request(json!({"op":"test","user":caller["owner_user"],"hub_config":cfg,"user_config":user}))`.
     - When `op == "human_channel_pair_start"`:
       - If `p.get("hub_config")` is present, validate and merge via `merge_channel_config(&plugin, "hub", &mut cfg, patch, false)` and persist via `save_service(c, id, &cfg)` before calling `ensure_process(&plugin)` and `configure_process(id, &process)`, enabling 1-click `Pair Account…` immediately after pasting a new `botToken`.

---

### 10.4 Complete Human Channel State Model & Lifecycle Actions (`L3`, `L4`, `L5`, `L6`, `L7`, `L8`)

1. **`[SHARP]` Human Channel Status Model (`hub.rs` `channel_list()`, `L3`)**:
   - In `channel_list(&self, caller: &Value)`:
     - `let has_binding = !binding.is_null();`
     - `let binding_enabled = has_binding && n(&binding, "enabled") == 1;`
     - `let enabled = binding_enabled && user_ready;` (never report `enabled: true` when `binding.is_null()` or `!user_ready`).
     - `let configured = hub_ready && user_ready;`
     - `let has_active_pair = !pair.is_null();`
     - `let status = if hub_ready && has_active_pair && !user_ready { "pairing_pending" } else if hub_ready && user_ready && enabled { "ready" } else if hub_ready && user_ready && !enabled { "paused" } else if hub_ready { "hub_ready" } else { "not_configured" };`
     - `let label_opt = binding["display_label"].as_str().filter(|s| !s.is_empty());`
     - `let detail = match status {`
       - `"ready"` => `if let Some(lbl) = label_opt { format!("Connected as {lbl} · Ready for notifications") } else { "Connected and ready for notifications".into() }`,
       - `"paused"` => `if let Some(lbl) = label_opt { format!("Paused ({lbl}) · Enable to resume notifications") } else { "Paused · Enable to resume notifications".into() }`,
       - `"pairing_pending"` => `"Pairing in progress · Send the /start command below".into()`,
       - `"hub_ready"` => `"Service ready · Pair your account or enter Chat ID".into()`,
       - `_` => `"Configure service credentials and pair your account".into()`,
       `};`
2. **`[SHARP]` Atomic Channel Lifecycle Transitions (`Pause/Resume`, `Unpair`, `Clear Service`, Manual `chatId` Override & `attach()` Gate, `L4`)**:
   - **Pause / Resume (`op: "channel_toggle"` or `op: "channel_save"` with `enabled: bool`)**:
     - Updates `hub_human_channels.enabled` (`1` or `0`), sets `last_delivered_seq = max_seq(c)?` when transitioning `0 -> 1`, preserves `user_config_json` (`chatId`, `_external_chat_id`, `_external_user_id`) and `display_label`, and updates the Human Node channel attachment (`attach(c, &binding, active, remote)`).
   - **Unpair Account (`op: "channel_unpair"` or `op: "disconnect_human_channel"` with `reset_user: true` / default)**:
     - In `hub.rs` `human_channels_configure`, when `p["reset_user"] == true`:
       - Resets the caller's row in `hub_human_channels` for `(principal_id, plugin_id)`: `enabled = 0`, `user_config_json = "{}"`, `display_label = NULL`, `updated_ms = now_ms()`.
       - Marks all unconsumed `hub_pairing_codes` for `(principal_id, plugin_id)` as consumed (`consumed_ms = now_ms()`).
       - Calls `attach(c, &binding, false, remote)` so `reg["channels"]["hub:<plugin_id>"]["state"]` becomes `"UNLISTENED"`.
   - **Clear Service / Change Bot (`op: "disconnect_human_channel"` or `op: "channel_save"` with `reset_hub: true`, Admin-Only)**:
     - Requires `admin(Some(caller))?`.
     - Resets `hub_plugin_services` for `plugin_id` (`config_json = "{}"`, `service_state_json = "{}"`, `updated_ms = now_ms()`).
     - Marks all unconsumed `hub_pairing_codes` for `plugin_id` as consumed (`consumed_ms = now_ms()`).
     - If `p["reset_user"] != false`, also resets the caller's `hub_human_channels` binding (`enabled = 0`, `user_config_json = "{}"`, `display_label = NULL`) and detaches `hub:<plugin_id>`.
     - Stops and removes the running `ChannelProcess` for `plugin_id`.
   - **Manual `chatId` Override Identity Reconciliation**:
     - In `human_channels_configure`, when `p.get("user_config")` changes a user-scoped field (such as `chatId`) to a value different from `old_config[key]`, automatically strip `_external_chat_id` and `_external_user_id` from `config` (unless explicitly provided in `patch`) and reset `display_label = Value::Null` (unless `p.get("display_label")` is explicitly provided), preventing stale paired identity metadata from conflicting with the new manual `chatId`.
   - **Strict `attach()` Readiness Invariant**:
     - `attach(c: &Connection, b: &Value, enabled: bool, remote: bool)` continues to write/update the `hub_kv` Human Node record (`listener_id(pid, "human", "default")`), setting `reg["channels"]["hub:<plugin_id>"]["state"] = if enabled { "ACTIVE" } else { "UNLISTENED" }`.
     - Callers of `attach()` in `human_channels_configure` and `restore_attachments` compute `let active = enabled && hub_ready && user_ready;` and pass `active` as the `enabled` parameter to `attach(c, &binding, active, remote)` so an unconfigured or half-configured channel never marks the Human Node `ACTIVE`.
3. **`[SHARP]` Pairing Code Superseding & Cancellation (`L5`)**:
   - In `hub.rs` `human_channel_pair_start`:
     - Before inserting a new row into `hub_pairing_codes`, execute:
       `UPDATE hub_pairing_codes SET consumed_ms=? WHERE principal_id=? AND plugin_id=? AND consumed_ms IS NULL`
       with `now_ms()`, immediately invalidating any previous unredeemed pairing code for `(principal_id, plugin_id)`.
   - Support `op: "channel_cancel_pair"` in `daemon.rs` $\to$ `human_channels_configure` with `cancel_pair: true` in `hub.rs`, which marks all unconsumed `hub_pairing_codes` for `(principal_id, plugin_id)` as consumed (`consumed_ms = now_ms()`) and returns the refreshed `channel_list(caller)`.
4. **`[SHARP]` Active Settings Hub Live Refresh & Pairing Instruction Persistence (`L6`)**:
   - In `DaemonService` (`crates/cito-native/src/core/daemon.rs`):
     - Add `active_settings_hub: Option<String>` to `DaemonService`.
     - Whenever `open_settings`, `channel_save`, `configure_human_channel`, `channel_toggle`, `channel_unpair`, `channel_cancel_pair`, `disconnect_human_channel`, `channel_test`, `test_human_channel`, `channel_pair`, or `pair_human_channel` executes with target `hub`, set `self.active_settings_hub = Some(hub.clone())`.
     - In `DaemonService::settings(&self)`, resolve the target hub as:
       `let hub = self.active_settings_hub.as_deref().filter(|h| self.slots.contains_key(*h)).unwrap_or(&self.config.default_hub);`
       and call `self.settings_for_hub(hub)`.
     - In `settings_for_hub(&self, channel_hub: &str)`, include both `"channel_hub": channel_hub` and `"settings_hub": channel_hub` in the returned JSON object.
     - After `channel_save`, `configure_human_channel`, `channel_toggle`, `channel_unpair`, `channel_cancel_pair`, or `disconnect_human_channel` succeeds in `daemon.rs`, emit `json!({"op":"settings","hub":hub,"settings":self.settings_for_hub(&hub)})` so `cito-ui` updates immediately.
   - In `src/macos/cito-ui.swift` `acceptSettings(_ value: SettingsPayload, hub: String? = nil)`:
     - Resolve `let target = value.settings_hub ?? value.channel_hub ?? hub ?? effectiveChannelHub`.
     - For each `channel` in `value.channels ?? []`:
       - Let `instrKey = target + "|" + channel.id`.
       - If `let serverInstr = channel.pairing_instruction, !serverInstr.isEmpty`:
         `channelInstructions[instrKey] = serverInstr`
       - Else:
         `channelInstructions.removeValue(forKey: instrKey)`
       - When `channel.configured == true`, also clear any stale transient error in `channelResults[instrKey]`.
5. **`[SHARP]` Bot Token Rotation Resets Cursor & Subprocess Polling State (`L7`)**:
   - In `hub.rs` `save_service(c: &Connection, id: &str, cfg: &Value) -> Result<bool, String>`:
     - Compare the incoming `cfg` with the previously stored `config_json` in `hub_plugin_services`.
     - If `old_cfg != *cfg`, reset `service_state_json = "{}"` (clearing the stored `cursor`) when updating `hub_plugin_services`, and return `Ok(true)`.
   - In `plugins/telegram/native.rs` `Runtime::request` (`"configure"` branch):
     - Let `old_token = str_at(&self.config, "botToken").unwrap_or("");`
     - Let `token_changed = !old_token.is_empty() && Some(old_token) != str_at(&r["hub_config"], "botToken");`
     - Update `self.config = r["hub_config"].clone();`
     - If `token_changed`: reset `self.cursor = 0; self.pending.clear(); self.reply_chats.clear(); self.pair_chats.clear();`
     - Else if `self.pending.is_empty()`: set `self.cursor = id(&r["cursor"]).and_then(|s| s.parse().ok()).unwrap_or(0);`
6. **`[SHARP]` Remote Hub Channel Schema Forwarding (`daemon.rs`, `L8`)**:
   - In `daemon.rs` (`channel_save` / `channel_test` / `channel_pair` / `channel_toggle` / `channel_unpair` / `channel_cancel_pair`):
     - When partitioning `action["values"]` into `hub_config` and `user_config`:
       - First check `self.plugins.find_by_id(id)` for local field definitions;
       - If `self.plugins.find_by_id(id)` is `None` (or has empty `fields`), query `self.slot(&hub)?.rpc("human_channels_list", &json!({}))` and extract `fields` from the matching channel descriptor (`ch["id"] == id`) on the target Hub.
       - If neither local plugins nor the target Hub's `human_channels_list` contains `id`, return `"INVALID_INPUT: Unknown human channel plugin: {id}"`.
7. **`[SHARP]` `SettingsPanel → Human Channels` Card Layout & Control Identifiers (`src/macos/cito-ui.swift`, `L1`–`L6`)**:
   - On each channel card (`id: "settings_channel:\(ch.id)"`, `width: 480`):
     - **Status Badge (`id: "settings_channel_status:\(ch.id)"`, `NSRect(x: 240, y: cardHeight - 25, width: 226, height: 16)`)**:
       - `ch.status == "ready"`: `"● Connected"` (or `"● Connected (\(label))"` when `display_label` is non-empty), color `.systemGreen`.
       - `ch.status == "paused"`: `"⏸ Paused"`, color `.systemOrange`.
       - `ch.status == "pairing_pending"`: `"◌ Pairing…"`, color `.systemBlue`.
       - `ch.hub_provided || ch.status == "hub_ready"`: `"Hub Service: ● Ready"`, color `.secondaryLabelColor`.
       - `ch.status == "error"`: `"⚠ Error"`, color `.systemRed`.
       - Otherwise: `"○ Not Configured"`, color `.secondaryLabelColor`.
     - **Setup Guide Box (`id: "channel_setup_guide:\(ch.id)"`)**:
       - Rendered when `!(ch.setup_guide ?? []).isEmpty && !ch.configured` at `NSRect(x: 14, y: guideY, width: 452, height: guideHeight)` using `(ch.setup_guide ?? []).joined(separator: "\n")`.
     - **Fields (`id: "channel_field:\(ch.id):\(f.key)"`)**:
       - Rendered for all `f` in `ch.fields.filter { $0.hidden != true }` at `NSRect(x: 170, y: fieldY, width: 296, height: 24)` (`NSSecureTextField` when `f.type == "secret"`), with `placeholderString = (f.type == "secret" && f.configured) ? (f.configured_placeholder ?? "•••••••• (Saved — enter new value to replace)") : f.placeholder`.
     - **Pairing Instruction & Copy/Cancel Bar (`!instruction.isEmpty`)**:
       - `pairing_instruction:\(ch.id)` (`NSRect(x: 14, y: 40, width: 300, height: instructionHeight)`).
       - `copy_pair_command:\(ch.id)` (`MenuControl("Copy Command")`, `leadingSymbol: "doc.on.doc"`, `frame: NSRect(x: 320, y: 42, width: 92, height: 22)`): extracts the `/start cito-...` substring (or copies `instruction`) to `appPasteboard` and `clipboardString`.
       - `cancel_pair_channel:\(ch.id)` (`MenuControl("Cancel")`, `frame: NSRect(x: 416, y: 42, width: 50, height: 22)`): clears `channelInstructions[instrKey]` and emits `["op": "channel_cancel_pair", "hub": effectiveChannelHub, "channel_id": chId]`.
     - **Bottom Action Row (`y: 10, height: 24`)**:
       - `pair_channel:\(ch.id)` (`NSRect(x: 14, y: 10, width: 112, height: 24)`): `"Pair Account…"` (or `ch.pairing_action_label`) when `!ch.configured`, `"Re-pair…"` when `ch.configured`.
       - When `ch.configured` (`status == "ready"` or `"paused"`):
         - `toggle_channel:\(ch.id)` (`MenuControl(ch.enabled ? "Pause" : "Resume")`, `frame: NSRect(x: 132, y: 10, width: 64, height: 24)`): emits `["op": "channel_toggle", "hub": effectiveChannelHub, "channel_id": chId, "enabled": !ch.enabled]`.
         - `unpair_channel:\(ch.id)` (`MenuControl("Unpair")`, `style: .destructiveHover`, `frame: NSRect(x: 202, y: 10, width: 62, height: 24)`): emits `["op": "channel_unpair", "hub": effectiveChannelHub, "channel_id": chId, "reset_user": true]`.
       - When `!ch.configured` and `ch.fields.contains(where: { $0.scope == "hub" && $0.configured && $0.hidden != true })`:
         - `clear_channel_service:\(ch.id)` (`MenuControl("Clear Token")`, `style: .destructiveHover`, `frame: NSRect(x: 132, y: 10, width: 84, height: 24)`): emits `["op": "disconnect_human_channel", "hub": effectiveChannelHub, "channel_id": chId, "reset_hub": true, "reset_user": true]`.
       - `test_channel:\(ch.id)` (`MenuControl("Send Test")`, `frame: NSRect(x: 312, y: 10, width: 80, height: 24)`).
       - `save_channel:\(ch.id)` (`MenuControl("Save")`, `style: .prominent`, `frame: NSRect(x: 398, y: 10, width: 68, height: 24)`).

---

### 10.5 Guided Onboarding Banner, Agent App Verify/Repair/Disconnect Lifecycle & Non-Destructive `Disconnect All` (`L9`, `L10`, `L11`, `L12`, `L13`)

1. **`[SHARP]` 3-Step Guided Onboarding Strip & Continuation Callout (`src/macos/cito-ui.swift`, `L9`)**:
   - In `settings_header` (`520 × 48 pt, y: 412`), add a `?` guide toggle button (`id: "toggle_onboarding_guide"`, `frame: NSRect(x: 460, y: 12, width: 22, height: 24)`, `symbol: "questionmark.circle"`, `toolTip: "Toggle Setup Guide"`) to the left of `close_settings` (`x: 486, y: 12, width: 24, height: 24`), and narrow `hubsTab` to `NSRect(x: 334, y: 12, width: 122, height: 24)` so all header controls have non-overlapping frames.
   - In `SettingsPanel → Agent Apps` (`settings_apps_tab`), render a 3-step onboarding banner (`id: "onboarding_banner"`, `frame: NSRect(x: 20, y: 316, width: 480, height: 44)`) whenever `onboardingGuideOpen == true` OR no Agent App is configured yet (`!(resolvedApps.contains(where: { $0.configured }))`):
     - Step 1 button (`id: "onboarding_step:apps"`, `"1. Agent Apps"`): switches to `apps` tab.
     - Step 2 button (`id: "onboarding_step:channels"`, `"2. Mobile Alerts →"`): switches to `channels` tab.
     - Step 3 button (`id: "onboarding_step:hubs"`, `"3. Hubs & Network →"`): switches to `hubs` tab.
   - When at least one Agent App is `configured == true`, render a continuation callout (`id: "apps_next_step_callout"`, `frame: NSRect(x: 20, y: nextCalloutY, width: 480, height: 38)`) below the Agent App cards with label `"Agent app connected · Next: get wake alerts on your phone"` and CTA button `Set Up Mobile Alerts →` (`id: "goto_channels_tab"`, `frame: NSRect(x: 318, y: 7, width: 150, height: 24)`) that switches `settingsActiveTab = "channels"`.
2. **`[SHARP]` Restoring Staged Shims/Plugins on `setup_apply` & Detecting `repair_needed` (`daemon.rs`, `setup.rs`, `L10`, `L12`)**:
   - In `daemon.rs` (`"setup_apply" | "enable_app"`):
     - Always call `ensure_cli_shims_native(&self.config.state_dir, &self.plugins)?` **before** calling `apply_plugin_setup(p, true, false)?` so that if `~/.cito/bin/cito`, `~/.cito/bin/cito-mcp`, or `~/.cito/plugins/<id>` was removed (e.g. by a prior disconnect/uninstall or manual deletion), clicking `Set Up`, `Re-check`, or `Repair` in `SettingsPanel` restores the shims and staged plugin directory before writing host config files.
     - Track `last_app_action: HashMap<String, String>` on `DaemonService` (`"setup"` on `setup_apply`, `"disconnect"` on `setup_remove_app`).
   - In `setup.rs` `uninstall_setup_native`:
     - Replace any literal plugin directory name in `state_dir` cleanup by iterating `&plugins.plugins` (`state_dir.join(&p.id)`) so `crates/cito-native/src/core/setup.rs` contains zero hardcoded plugin names.
   - In `daemon.rs` `settings_for_hub`:
     - When `inspect_plugin_setup(p)` reports `configured == true`, check if `self.config.state_dir.join("bin").exists()` or `self.config.state_dir.join("plugins").exists()`: if staging has occurred in `state_dir`, verify that `self.config.state_dir.join("bin/cito-mcp").exists()` and `self.config.state_dir.join("plugins").join(&p.id).exists()`.
     - If host config is present (`configured_in_host == true`) but staged files under `state_dir` were deleted after setup:
       - Report `configured: false`, `status: "repair_needed"`, `detail: "Staged Cito files missing — click Repair to restore"`.
     - When `configured == true` and healthy:
       - Report `configured: true`, `status: "ready"`, and `detail`: `p.post_setup_hint.as_deref().unwrap_or("Connected and ready")`.
     - When `!configured` and `self.last_app_action.get(&p.id).map(String::as_str) == Some("disconnect")`:
       - Report `detail`: `p.post_disconnect_hint.as_deref().unwrap_or("Disconnected · Click Set Up to reconnect")`.
   - In `src/macos/cito-ui.swift` (`SettingsPanel → Agent Apps`):
     - `settings_app_status:\(app.id)` text:
       - `"● Connected"` (`.systemGreen`) when `app.configured == true`
       - `"⚠ Repair Needed"` (`.systemOrange`) when `!app.configured && app.status == "repair_needed"`
       - `"○ Not Set Up"` (`.secondaryLabelColor`) otherwise.
     - `setup_app:\(app.id)` button title:
       - `"Re-check"` when `app.configured == true`
       - `"Repair"` when `!app.configured && app.status == "repair_needed"`
       - `"Set Up"` otherwise.
3. **`[SHARP]` `setup_remove_app` Automatically Unlistens Active Sessions of That Harness (`daemon.rs`, `L11`)**:
   - In `daemon.rs` (`"setup_remove_app" | "disable_app"`):
     - Before/after calling `remove_plugin_setup(p)?`, query `self.state.active()?` for all active local listener rows where `string(&row, "harness") == id`, and invoke `let _ = self.unlisten_id(string(&row, "hub"), string(&row, "listener_id"));` for each matching row.
     - Clear any cached caller PID entries for `harness == id` in `self.caller_cache`.
4. **`[SHARP]` Decoupling `Disconnect All Integrations` from `Uninstall & Quit` (`daemon.rs`, `setup.rs`, `cito-ui.swift`, `L13`)**:
   - **New UI IPC Action `op: "setup_disconnect_all"` in `daemon.rs`**:
     - Unlistens all active local listeners across all hubs (`for row in self.state.active()? { let _ = self.unlisten_id(string(&row, "hub"), string(&row, "listener_id")); }`).
     - Calls `remove_plugin_setup(plugin)` for every `plugin` in `self.plugins.agent_plugins()`.
     - Disables `LaunchAgent` (`let _ = set_launch_at_login(false, &self.config.state_dir, &self.config.home_dir, &self.plugins);`).
     - **Keeps `cito-daemon` and `cito-ui` running**, preserves `~/.cito/bin` and `~/.cito/plugins` (so subsequent `Set Up` is instant), and emits refreshed `settings` and `state` frames.
   - **Existing `op: "setup_uninstall_all"` in `daemon.rs`**:
     - Performs full uninstallation and daemon shutdown (`request_shutdown()`), removing shims/staged plugins and (when `purge_data: true`) `~/.cito` and `Cito.app`.
   - **Updated `uninstall_confirm_box` in `src/macos/cito-ui.swift` (`NSRect(x: 20, y: 48, width: 480, height: 96)`)**:
     - Header label: `"Reset or Uninstall Cito"` (`NSRect(x: 12, y: 68, width: 456, height: 18)`, `size: 12.5, weight: .semibold`).
     - `uninstall_purge_data` checkbox (`"Also delete ~/.cito local data & hub settings on full uninstall"`, `frame: NSRect(x: 12, y: 40, width: 420, height: 22)`).
     - `cancel_uninstall_all` button (`"Cancel"`, `frame: NSRect(x: 112, y: 8, width: 72, height: 24)`).
     - `confirm_disconnect_all` button (`"Disconnect All"`, `frame: NSRect(x: 192, y: 8, width: 116, height: 24)`): emits `["op": "setup_disconnect_all"]` and closes `uninstallConfirmOpen` while keeping `SettingsPanel` open.
     - `confirm_uninstall_all` button (`"Confirm Uninstall"`, `style: .destructiveHover`, `frame: NSRect(x: 316, y: 8, width: 152, height: 24)`): emits `["op": "setup_uninstall_all", "purge_data": purge]`.

---

### 10.6 Hub Lifecycle: OIDC `Switch Account`, Inline `Edit Hub`, Disabled Hub `Sign In`, `cardMenu` Offline Visibility, LAN Share Controls & Actionable Empty States (`L14`, `L15`, `L16`, `L17`)

1. **`[SHARP]` OIDC `Switch Account` Forces Interactive Account Selection (`login.rs`, `daemon.rs`, `cito-ui.swift`, `L14`)**:
   - In `src/macos/cito-ui.swift` (`cardMenu` hub drawer):
     - When `hub.status == "connected"` and `hub.auth_mode == "oidc"`, the `"Switch Account"` button (`id: "login:\(hub.name)"`) emits `["op": "hub_action", "hub": hub.name, "action": "switch_account"]` (or `"action": "login", "force_interactive": true`).
   - In `crates/cito-native/src/core/daemon.rs`:
     - Update `interactive_login(action: &Value) -> bool` to match `action["op"] == "login_hub" || (action["op"] == "hub_action" && matches!(action["action"].as_str(), Some("login" | "switch_account")))`.
     - If `action["op"] == "hub_action" && matches!(action["action"].as_str(), Some("login" | "switch_account"))`, automatically enable the hub in `config.json` (`v["hubs"][&hub]["enabled"] = json!(true)`) before `s.reload_slots()?` so clicking `Sign In…` on a disabled OIDC hub in `SettingsPanel` (`login_hub:\(hub.name)`) enables the hub and launches the OIDC flow in 1 click (`L15`).
     - Pass `force_account_select = action["action"] == "switch_account" || action["force_interactive"] == true` into `login::recover_cancellable_with_options`.
   - In `crates/cito-native/src/core/login.rs`:
     - When `force_account_select == true`:
       - Bypass cached token freshness reuse (`if !force_account_select { if let Some(cache) = old.as_ref().filter(...) { return verify_cache(...); } }`).
       - Bypass silent `refresh_token` exchange (`let old_refresh = if force_account_select { None } else { ... };`).
       - Append `("prompt", "select_account")` to the authorization URL query parameters so the OIDC provider prompts the user to choose an account.
2. **`[SHARP]` Inline `Edit Hub`, `cardMenu` Disconnected Hub Retention & LAN Share Port/Auth Controls (`daemon.rs`, `cito-ui.swift`, `L15`, `L16`)**:
   - **`HubConfigItem` & `settings_for_hub` (`daemon.rs`, `cito-ui.swift`)**:
     - Include `"issuer": cfg.get("issuer").or_else(|| cfg.get("oidcIssuer"))` and `"client_id": cfg.get("clientId").or_else(|| cfg.get("client_id")).or_else(|| cfg.get("oidcClientId"))` and `"auth_mode"` on each hub item in `settings["hubs"]`.
     - In `daemon.rs` `snapshot()`:
       - Include all configured hubs in `state["hubs"]` (with `status: "disconnected"` when `enabled == false` in `config.json`) OR when `hub_action` with `action == "disconnect"` is invoked from `cardMenu`, keep the hub visible in `state["hubs"]` with `status: "disconnected"` so the user can click `"Connect"` (`reconnect:<hub>`) in `cardMenu` without opening `SettingsPanel`!
       - Specifically: in `daemon.rs` `snapshot()`, build `state["hubs"]` from `settings["hubs"]` (where enabled hubs report their live slot status and disabled hubs report `"status": "disconnected"`, `"enabled": false`), while `self.slots` continues to contain only `enabled != false` hubs (so `cito_find` / `reload_slots` / default hub fallback behavior on disabled hubs remains 100% unchanged!).
       - In `src/macos/cito-ui.swift`, `available_header` target hub strip and session `switch_hub` ring filter `hubs.filter { $0.status != "disconnected" }` when selecting active target hubs, while the top `cardMenu` hub pill strip (`hub:\(hub.name)`) and drawer (`drawer:\(hub.name)`) show all hubs so a disconnected hub remains visible with its `"Connect"` (`reconnect:\(hub.name)`) button!
   - **Inline `Edit` Button on Hub Rows in `SettingsPanel → Hubs & Network` (`cito-ui.swift`, `daemon.rs`)**:
     - On each hub row (`settings_hub:\(hub.name)`), add an `Edit` button (`id: "edit_hub:\(hub.name)"`, `frame: NSRect(x: 192, y: 13, width: 46, height: 22)`, `radius: 6`) that populates `addHubName = hub.name`, `addHubUrl = hub.url`, `addHubAuth = hub.auth_mode`, `addHubUser = hub.user ?? ""`, `addHubIssuer = hub.issuer ?? ""`, `addHubClientId = hub.client_id ?? ""`, sets `editingExistingHub = true`, and opens `add_hub_form` (with submit button label `"Save Hub"` while keeping `id: "submit_add_hub"`).
     - When `editingExistingHub == true`, submitting `submit_add_hub` sends `"edit": true` (without forcing `set_default: true`); in `daemon.rs` `setup_save_hub`, when `action["edit"] == true`, merge updated fields into the existing `v["hubs"][&name]` object (retaining the existing stored `token` if the submitted `token` is empty, and preserving `enabled` and `default_hub`).
   - **LAN Share Port & Token Auth Controls in `host_hub_card` (`cito-ui.swift`, `daemon.rs`, `L16`)**:
     - Include `"auth_mode": self.host_hub_auth_mode(&raw)` and `"error": self.last_host_hub_error` on `host_hub` in `settings_for_hub` (recording `last_host_hub_error` on `DaemonService` when `set_host_hub` fails and clearing it when `set_host_hub` succeeds).
     - Inside `host_hub_card` (`NSRect(x: 20, y: 232, width: 480, height: 124)`):
       - Keep `toggle_host_hub` (`NSRect(x: 396, y: 88, width: 70, height: 24)`), `host_hub_endpoint` (`NSRect(x: 14, y: 46, width: 452, height: 16)`), and `copy_hub_invite` (`NSRect(x: 14, y: 12, width: 168, height: 24)`).
       - Add port input field (`id: "host_hub_port"`, `frame: NSRect(x: 192, y: 12, width: 64, height: 24)`, `value: "\(hostHub.port)"`) and `Apply Port` / `Token Auth` controls:
         - `toggle_host_hub_auth` (`MenuControl((hostHub.auth_mode ?? (hostHub.token.isEmpty ? "none" : "token")) == "none" ? "Auth: None" : "Auth: Token")`, `frame: NSRect(x: 262, y: 12, width: 96, height: 24)`, `radius: 6`): toggles `auth_mode` between `"token"` and `"none"` via `emit(["op": "set_host_hub", "enabled": hostHub.enabled, "port": portVal, "auth_mode": nextAuthMode])`.
         - `apply_host_hub_port` (`MenuControl("Apply")`, `frame: NSRect(x: 364, y: 12, width: 56, height: 24)`, `radius: 6`): parses `settingsFieldControls["host_hub_port"]?.stringValue` and emits `["op": "set_host_hub", "enabled": hostHub.enabled, "port": portVal]`.
       - When `let err = hostHub.error, !err.isEmpty`, display `err` in `host_hub_error` (`NSRect(x: 14, y: 62, width: 452, height: 14)`, `.systemRed`).
3. **`[SHARP]` Actionable Context-Aware Empty States in `cardMenu` & `DirectoryPanel` (`cito-ui.swift`, `L17`)**:
   - **`cardMenu` Empty State (`id: "empty"`)**:
     - When `listeners.isEmpty && available.isEmpty`:
       - Preserve container `id: "empty"` (`NSRect(x: 0, y: 0, width: 380, height: 92)`).
       - Inside the card (`NSRect(x: 10, y: 3, width: 360, height: 86)`), display `"No active listeners"` and subtitle:
         - If `!(settings?.apps.contains(where: { $0.configured }) ?? true)`: `"Connect your coding apps in Settings to start"`
         - Else: `"Call cito_listen in a session or configure Mobile Alerts"`
       - Add two quick-action buttons at `y: 8`:
         - `empty_open_setup` (`MenuControl("Set Up Agent Apps…")`, `frame: NSRect(x: 44, y: 8, width: 148, height: 22)`): opens `SettingsPanel` on `apps` tab.
         - `empty_open_channels` (`MenuControl("Mobile Alerts…")`, `frame: NSRect(x: 198, y: 8, width: 118, height: 22)`): opens `SettingsPanel` on `channels` tab.
   - **`DirectoryPanel` Empty State Overlay (`id: "directory_empty_state"`, `NSRect(x: 20, y: 110, width: 520, height: 140)`)**:
     - Added as a child of `directory_panel` above `directory_scroll`, shown (`isHidden = false`) ONLY when the active mode's filtered list is empty (`directoryMode == "directory" ? directoryFilteredPeers.isEmpty : activityFilteredItems.isEmpty`), and hidden (`isHidden = true`) whenever rows exist.
     - In `Directory` mode (`directoryFilteredPeers.isEmpty`):
       - When filters/query are active (`!directoryQuery.isEmpty || directoryScopeFilter != "all" || directoryHarnessFilter != "all"`):
         - Title: `"No matching nodes"`, subtitle: `"No active listeners match your current search or filter."`
         - CTA button `directory_reset_filters` (`"Reset Filters"`, `frame: NSRect(x: 196, y: 18, width: 128, height: 26)`): resets `directoryQuery = ""`, `directoryScopeFilter = "all"`, `directoryHarnessFilter = "all"`, refreshes the filter bar, and emits `query_directory`.
       - When no filters are active:
         - Title: `"No active nodes on this Hub"`, subtitle: `"Enroll a coding session with + Listen or connect an Agent App."`
         - CTA button `dir_empty_open_apps` (`"Open Agent Apps Settings…"`, `frame: NSRect(x: 166, y: 18, width: 188, height: 26)`): opens `SettingsPanel` (`apps` tab).
     - In `Activity` mode (`activityFilteredItems.isEmpty`):
       - When `activityFilter == "unread"` and `!activityAllItems.isEmpty`:
         - Title: `"All caught up"`, subtitle: `"You have no unread signals."`
         - CTA button `act_empty_show_all` (`"Show All Activity"`, `frame: NSRect(x: 186, y: 18, width: 148, height: 26)`): switches `activityFilter = "all"` and calls `applyActivityFilter()`.
       - Otherwise:
         - Title: `"No recent signal activity"`, subtitle: `"Signals sent between your sessions and channels will appear here."`
         - CTA button `act_empty_open_channels` (`"Configure Human Channels…"`, `frame: NSRect(x: 166, y: 18, width: 188, height: 26)`): opens `SettingsPanel` (`channels` tab).

---

### 10.7 Complete `L1`–`L17` Verification & Acceptance Matrix

| ID | Lifecycle Gap | Authoritative Modules | Required Verification / Regression Test |
| :--- | :--- | :--- | :--- |
| **`L1`** | Secret wire redaction (`redacted`) conflated with UI visibility (`hidden`); missing inline setup guide | `hub.rs`, `plugins/telegram/plugin.json`, `cito-ui.swift` | Local/token & OIDC admin callers receive `hidden: false, redacted: true, value: "", configured: true` for `botToken`; non-admin OIDC callers receive `hidden: true, redacted: true`; `channel_field:telegram:botToken` is visible in `SettingsPanel` with `configured_placeholder` and `channel_setup_guide:telegram`. |
| **`L2`** | Empty secret field overwrites saved `botToken` on `Save` / `Pair`; `channel_test` mutates DB | `cito-ui.swift`, `hub.rs` | `collectValues()` omits clean configured secret keys; `hub.rs` ignores `""` for secret fields unless `clear_hub_config`/`reset_hub` is set; `human_channels_test` merges transient config in memory without mutating `hub_plugin_services`. |
| **`L3`** | Incomplete Human Channel state machine (`enabled: true` when `binding.is_null()`) | `hub.rs`, `cito-ui.swift` | `channel_list()` reports `enabled: false` when unbound, and transitions deterministically across `not_configured`, `hub_ready`, `pairing_pending`, `ready`, `paused`, and `error` with `display_label` in `detail`. |
| **`L4`** | Missing `Pause/Resume`, `Unpair`, `Clear Token`, manual `chatId` identity reset, & `attach()` readiness gate | `hub.rs`, `daemon.rs`, `cito-ui.swift` | `channel_toggle` pauses/resumes without losing `chatId`; `channel_unpair` clears `user_config_json`, `display_label`, and pairing codes; `reset_hub: true` clears `hub_plugin_services` and stops subprocess; changing `chatId` clears stale `_external_chat_id`/`_external_user_id`; `attach()` sets `ACTIVE` iff `enabled && hub_ready && user_ready`. |
| **`L5`** | Old unconsumed pairing codes remain redeemable after re-pairing or unpairing | `hub.rs`, `daemon.rs`, `cito-ui.swift` | Starting a new pair, cancelling pairing (`channel_cancel_pair`), or unpairing marks prior unconsumed `hub_pairing_codes` as consumed (`consumed_ms = now_ms()`); redeeming a superseded code fails with `INVALID_INPUT`. |
| **`L6`** | `DaemonService::settings()` only polls `default_hub`; `acceptSettings()` wipes `channelInstructions` | `daemon.rs`, `cito-ui.swift` | `active_settings_hub` tracks the selected Settings hub and includes `settings_hub` in `settings` payload; `acceptSettings` preserves active pairing instructions until paired or cancelled; `copy_pair_command:<id>` copies the `/start cito-...` command. |
| **`L7`** | Changing `botToken` retains stale Telegram `cursor` and in-memory maps | `hub.rs`, `plugins/telegram/native.rs` | Changing `botToken` in `save_service` resets `service_state_json = "{}"` and resets `cursor = 0`, `pending`, `reply_chats`, and `pair_chats` in `cito-plugin-telegram`. |
| **`L8`** | `daemon.rs` fails `channel_save`/`channel_pair`/`channel_test` when channel plugin is only on remote Hub | `daemon.rs` | `daemon.rs` resolves field `scope` (`"hub"` vs `"user"`) from the target Hub's `human_channels_list` schema when `self.plugins.find_by_id(id)` is `None`. |
| **`L9`** | First-time onboarding lacks step progression and post-setup guidance | `cito-ui.swift` | `SettingsPanel` renders `onboarding_banner` (`onboarding_step:apps`, `onboarding_step:channels`, `onboarding_step:hubs`), `?` toggle (`toggle_onboarding_guide`), and `apps_next_step_callout` (`goto_channels_tab`). |
| **`L10`** | `Set Up` / `Re-check` lacks harness-specific reload hints and `Repair` state | `plugins/*/plugin.json`, `daemon.rs`, `cito-ui.swift` | `plugin.json` supplies `postSetupHint` / `postDisconnectHint`; missing staged files report `status: "repair_needed"` with `"Repair"` button in `SettingsPanel`. |
| **`L11`** | Clicking `Disconnect` on an Agent App leaves its active sessions listening in `cardMenu` | `daemon.rs` | `setup_remove_app` unlistens all active `local_listeners` rows with `row["harness"] == id` and clears `caller_cache` for that harness. |
| **`L12`** | Clicking `Set Up` after deleting `~/.cito/bin` or `~/.cito/plugins` fails to restore shims | `daemon.rs`, `setup.rs` | `setup_apply` in `daemon.rs` invokes `ensure_cli_shims_native(&self.config.state_dir, &self.plugins)` before `apply_plugin_setup`. |
| **`L13`** | `Remove All Integrations…` unconditionally quits `Cito.app` and `cito-daemon` | `daemon.rs`, `cito-ui.swift` | `uninstall_confirm_box` offers both `confirm_disconnect_all` (`op: "setup_disconnect_all"`, keeping daemon & UI alive) and `confirm_uninstall_all` (`op: "setup_uninstall_all"`, full uninstall & quit). |
| **`L14`** | OIDC `Switch Account` silently reuses cached JWT or refresh token without account picker | `login.rs`, `daemon.rs`, `cito-ui.swift` | `switch_account` passes `force_account_select = true`, skips cache/refresh token reuse, and appends `prompt=select_account` to the authorization URL. |
| **`L15`** | Hub rows lack inline `Edit`; clicking `Sign In…` on a disabled OIDC hub fails; `Disconnect` in `cardMenu` hides the hub | `daemon.rs`, `cito-ui.swift` | `edit_hub:<name>` opens pre-filled `add_hub_form`; `login_hub:<name>` auto-enables disabled OIDC hubs before login; disconnected hubs remain visible in `cardMenu` with `reconnect:<name>`. |
| **`L16`** | `Share Hub on Local Network` card lacks port input, auth mode toggle, and error display | `cito-ui.swift` | `host_hub_card` exposes `host_hub_port`, `apply_host_hub_port`, `toggle_host_hub_auth`, and `host_hub_error`. |
| **`L17`** | Empty states in `cardMenu` and `DirectoryPanel` have no actionable CTAs | `cito-ui.swift` | `cardMenu` empty card provides `empty_open_setup` and `empty_open_channels`; `DirectoryPanel` provides `directory_empty_state` with `directory_reset_filters`, `dir_empty_open_apps`, `act_empty_show_all`, and `act_empty_open_channels`. |

---

## 11. Stage 9 (`v0.20.1` — Phase `v25`): Theory-Grounded UI/UX Redesign, Two-Stratum Human Channel Architecture (`Hub Gateway` vs. `Personal Human Node`), Holistic Layout Grid & Live Visual E2E Verification (`U1`–`U11`)

### 11.1 Theoretical & Normative UX Foundation (4 Governing Laws)
Every layout, grouping, label, and interaction rule in Stage 9 (`v0.20.1` — Phase `v25`) is governed by four explicit HCI and Apple macOS Human Interface Guidelines (HIG) laws:
1. **Law 1 — State-Strata Isomorphism (Don Norman's *Conceptual Model & Mapping Principle* + Domain-Driven State Ownership)**:
   - Controls with different state owners, cardinalities, and blast radii MUST NOT be rendered as flat peer inputs in a single undifferentiated form.
   - In Cito's ontology (`Node (1) ↔ Hub ↔ Channels (1..K)`), Human Channel configuration spans three distinct state strata:
     - **Device Stratum (`This Mac`)**: `Mac Desktop Banner` (`desktopBannerEnabled` in local `config.json`), affecting only local AppKit HUD banners on this Mac.
     - **Hub Gateway Stratum (`scope: "hub"`, stored in `hub_plugin_services`)**: Shared channel infrastructure (e.g., Telegram `@BotFather` `Bot Token`), configured **once per Hub** by the Hub Owner/Admin and shared by every Human Node connected to that Hub.
     - **Personal Human Node Stratum (`scope: "user"`, stored in `hub_human_channels`)**: Per-user enrollment (e.g., `/start` account pairing & personal `Chat ID`), configured **once per Human Principal (`@user`)** on that Hub.
2. **Law 2 — Progressive Disclosure & Prerequisite Staging (Apple macOS HIG — *Settings* & *Grouping Controls*)**:
   - Separate `scope: "hub"` gateway infrastructure (`stratum_hub:<id>`) from `scope: "user"` personal node enrollment (`stratum_user:<id>`) into two visually distinct sub-containers inside each Hub-routed channel card (`settings_channel:<id>`), so users immediately understand what belongs to the Hub (set up once) versus what every joining Human Node configures for themselves.
3. **Law 3 — Single Navigation Locus & User-Invoked Contextual Help (Apple macOS HIG — *Toolbars & Tab Views*)**:
   - The window header (`settings_header`: `Agent Apps` | `Human Channels` | `Hubs & Network`) is the sole primary navigation bar; never render a duplicate 3-button tab bar underneath it by default. Contextual help (`?` `toggle_onboarding_guide`) is strictly user-toggled and works identically across all 3 tabs.
4. **Law 4 — Persistent Field Labels, Right-Edge Action Alignment & Zero Truncation Invariant (Apple macOS HIG — *Labels*, *Forms*, & *Layout*)**:
   - Because `placeholderString` disappears once an `NSTextField` has a value, every editable input in `add_hub_form`, `host_hub_card`, and `settings_channel:*` MUST have a persistent visible label. Every button and badge frame width must accommodate its full text (`truncated_labels == []`), and list-row action buttons (`CONNECTED HUBS`, `Agent Apps`) must right-align flush to `x = 468`.

### 11.2 Binding Requirements (`U1`–`U11`)
Interactive macOS testing of `v0.20.0` exposed 11 concrete UX, session-discovery, terminology, state-stratum, and visual layout defects (`U1`–`U11`). Stage 9 (`v0.20.1` — Phase `v25`) supersedes conflicting layout/copy rules in `§10` with the following binding contracts:

1. **`[SHARP]` `U1` — Gate Session Discovery on `configured == true` (`crates/cito-native/src/core/daemon.rs`, `plugins/{codex,pi,claude}/native.rs`)**:
   - **Problem**: `AVAILABLE SESSIONS` in `cardMenu` displayed `"Codex · 8 recent"` from `~/.codex/state_5.sqlite` even before `Codex` was `Set Up` (or after `Codex` was `Disconnected`).
   - **Contract**:
     - In `daemon.rs` `snapshot()`: compute `let settings = self.settings();` first, extract the set of configured agent harnesses from `settings["apps"]` (`let configured_harnesses: HashSet<&str> = settings["apps"].as_array().into_iter().flatten().filter(|a| a["configured"] == true).filter_map(|a| a["id"].as_str()).collect();`), and filter `available` so any session whose `harness` matches a registered `self.plugins.agent_plugins()` entry with `!configured_harnesses.contains(harness)` is excluded from `available`! (Note: synthetic test harnesses in unit/integration fixtures whose `harness` ID is not in `settings["apps"]` or where `p.is_builtin == false` remain unaffected unless `settings["apps"]` explicitly lists that harness with `configured: false`.)
     - In `daemon.rs` `setup_apply`, `setup_remove_app`, and `setup_disconnect_all`: immediately call `if let Some(a) = self.session_activity.as_mut() { a.refresh_discovery(); }` before `self.push_ui()`.

2. **`[SHARP]` `U2` — Live Running `Pi` Process & Session Discovery + Self-Healing Bridge Socket (`plugins/pi/native.rs`, `plugins/pi/cito-bridge.ts`, `crates/cito-native/tests/support/{mod.rs,pi-bridge.mjs}`)**:
   - **Problem**: After clicking `Set Up` on `Pi Coding Agent`, already-open `pi` terminal sessions did not appear in `AVAILABLE SESSIONS` because `plugins/pi/native.rs` only scanned `~/.cito/sockets/pi-pid-*.sock` (which are absent for `pi` processes started prior to `Set Up` or after `Disconnect` cleaned `~/.cito/sockets`).
   - **Contract**:
     - **Self-healing socket in `plugins/pi/cito-bridge.ts` & test harness `crates/cito-native/tests/support/pi-bridge.mjs`**:
       - Call `ensureServer()` during extension initialization (in addition to `session_start`), and register a `.unref()` `setInterval(..., 2000)` that checks whether `socketPath` still exists on disk when `~/.pi/agent/settings.json` contains the Cito package; if `socketPath` was unlinked (e.g. by a prior disconnect/repair), close the stale `server` handle and re-run `ensureServer()` so `pi-pid-<pid>.sock` is automatically restored within 2s. Update `crates/cito-native/tests/support/pi-bridge.mjs` to match `ensureServer()` running at extension load time while preserving `F58` stale-socket recovery.
     - **Live process + session file discovery in `plugins/pi/native.rs` & hermetic test isolation (`CITO_PI_PROCESS_DISCOVERY`)**:
       - In `candidates(req)`: when `op == "discover_sessions"` (or `resolve_caller` / `verify` / `probe` when no socket matched), after collecting socket-backed candidates from `state(req).join("sockets")`, if `setup_inspect` reports `configured == true` (in `~/.pi/agent/settings.json`) and `env_value(req, "CITO_PI_PROCESS_DISCOVERY").as_deref() != Some("0")`, also scan running `pi` processes owned by `geteuid()` via `/bin/ps -axo pid=,uid=,command=` where `matches_process(command)` is true and `pid` is not already in `out`:
         - Resolve the process's working directory via `/usr/sbin/lsof -a -d cwd -Fn -p <pid>` (parsing the `n<path>` line).
         - Look up the latest session `.jsonl` file in `config_home(req, "CITO_PI_HOME", "~/.pi/agent").join("sessions")` under the encoded cwd directory (`--<cwd_without_leading_slash_and_slashes_replaced_by_dashes>--`), reading `"id"` (or filename stem) as `session_id`, `"session_name"` / `"name"` (or directory basename) as `session_name`, and file `modified()` as `updated_at_ms`. If no session file is found, or if multiple live `pi` processes share the same `cwd` and an earlier candidate in `out` already claimed that `session_id`, fall back to `session_id: format!("pid-{p}")` and `session_name: basename(cwd)` so two live processes in the same `cwd` never collide on the same `session_id`.
         - Allow `resolve_caller` and `probe` in `plugins/pi/native.rs` to accept a live process candidate `(p, v)` when no socket responded yet (verifying `process_identity(p)` matches `matches_process`), so clicking `+ Listen` in `cardMenu` immediately enrolls the open Pi session!
         - In `crates/cito-native/tests/support/mod.rs`, `Fixture::new()` sets `CITO_PI_PROCESS_DISCOVERY=0` by default so ambient host `pi` processes never leak into unrelated sandbox tests, while dedicated `U2` live-process discovery tests set `CITO_PI_PROCESS_DISCOVERY=1` and spawn a fixture-owned process whose `cwd` is inside the test sandbox.

3. **`[SHARP]` `U3` — Re-Enrolling the Human Node in `cardMenu` When Stopped (`src/macos/cito-ui.swift`)**:
   - **Problem**: Clicking `[ × ]` (`stop:<scoped_id>`) on the Human listener (`harness == "human"`) in `cardMenu` unlistened the Human node, and because `AVAILABLE SESSIONS` only lists agent sessions, there was no UI control to add the Human node back.
   - **Contract**:
     - In `src/macos/cito-ui.swift` `rebuild()`:
       - Evaluate `let targetHub = effectiveTargetHub`, `let hasHumanListener = listeners.contains { $0.harness == "human" && $0.hub == targetHub }`, and `let targetUser = hubs.first(where: { $0.name == targetHub })?.user ?? hubs.first?.user ?? "local"`.
       - Whenever `!hasHumanListener` and `hubs.contains(where: { $0.name == targetHub && $0.status != "disconnected" })` (so stopping the Human Node on any connected Hub shows the re-enrollment card for that Hub):
         - Render a compact **"Human Node Paused"** card at the top of `cardMenu` (`id: "human_paused_card"`, `NSRect(x: 0, y: 0, width: 380, height: 52)`):
           - Inner card: `roundedView(NSRect(x: 10, y: 3, width: 360, height: 46), radius: 10, color: .labelColor.withAppearanceAlpha(0.05), border: .labelColor.withAppearanceAlpha(0.10))`
           - Badge: `makeHarnessBadge(NSRect(x: 10, y: 9, width: 28, height: 28), radius: 7, iconInset: 6, harness: "human", harnesses: map)`
           - Title (`NSRect(x: 46, y: 24, width: 220, height: 16)`, `size: 12.5, weight: .semibold`): `"Human Node (@\(targetUser))"`
           - Subtitle (`NSRect(x: 46, y: 8, width: 220, height: 14)`, `size: 10.5, color: .secondaryLabelColor`): `"Paused · Click Enable to receive signals"` (`196pt <= 220pt`)
           - Button: `MenuControl("Enable", frame: NSRect(x: 272, y: 12, width: 78, height: 22), leadingSymbol: "plus", radius: 6)` with `identifier = NSUserInterfaceItemIdentifier("listen_human")` and `style = .prominent`.
           - Clicking `listen_human` emits `["op": "listen_session", "hub": targetHub, "harness": "human", "session_id": "default", "about": "Human receiver"]`.

4. **`[SHARP]` `U4` — Unified "Human Node / Human Channels" Terminology (`src/macos/cito-ui.swift`, `plugins/telegram/plugin.json`)**:
   - **Problem**: Using `"Mobile Alerts"` on buttons/banners conflicted with the `"Human Channels"` tab name and implied passive one-way alerts rather than a bidirectional Human Node on the signaling network.
   - **Contract**:
     - Replace all occurrences of `"Mobile Alerts"` with vendor-neutral `"Human Channels"` terminology (keeping `src/macos/cito-ui.swift` free of any concrete channel names such as `"Telegram"` per `§1.2`):
       - `onboarding_step:channels` label: `"2. Human Channels →"`
       - `goto_channels_tab` label: `"Human Channels →"` (with callout label `NSRect(x: 12, y: 10, width: 264, height: 18)`, `size: 10`: `"Agent app connected · Optional: add a Human Channel"`)
       - `empty_open_channels` label in `cardMenu`: `"Human Channels…"` (with subtitle: `"Call cito_listen in a session or open Human Channels"`)

5. **`[SHARP]` `U5` — Non-Redundant, User-Controlled `?` Setup Guide Across All 3 Tabs & Dismissible Callout (`src/macos/cito-ui.swift`)**:
   - **Problem**:
     1. `showGuide = onboardingGuideOpen || !resolvedApps.contains(where: { $0.configured })` forced `onboarding_banner` open whenever 0 apps were configured (even after clicking `?`), duplicating the 3 tab buttons right below the header (`Screenshot 1` & `Screenshot 7`).
     2. `onboarding_banner` was only added inside `appsView` (`Tab 1`), so clicking `?` on `Human Channels` or `Hubs & Network` did nothing (`Screenshot 5`).
   - **Contract**:
     - `onboardingGuideOpen` defaults to `false` and is strictly controlled by `toggle_onboarding_guide` (`?` button in `settings_header`, which highlights with `style = onboardingGuideOpen ? .prominent : .neutral`) and `dismiss_onboarding_guide` (`×` inside the banner); it is **never** forced true by `!resolvedApps.contains(...)`.
     - When `onboardingGuideOpen == true`, render `onboarding_banner` (`NSRect(x: 20, y: 348, width: 480, height: 58)`) directly inside `root` below `settings_header` (`y: 412..460`) and shrink the `mountSettingsCards` scroll viewport height from `368pt` (`y: 44..412`) to `300pt` (`NSRect(x: 0, y: 44, width: 520, height: 300)`) so `onboarding_banner` (`y: 348..406`) and the active tab viewport (`y: 44..344`) have strictly disjoint vertical coordinate ranges and never overlap any tab content on **all 3 tabs (`apps`, `channels`, `hubs`)**!
     - Inside `onboarding_banner`:
       - Top line (`y: 34`): `"Quick Setup Guide — Click any step to jump:"` (`x: 12, y: 34, width: 420, height: 16`, `size: 11, weight: .semibold`) + `MenuControl("Close Guide", frame: NSRect(x: 450, y: 32, width: 22, height: 20), symbol: "xmark")` (`id: "dismiss_onboarding_guide"`).
       - Bottom line (`y: 8`): 3 step buttons (`onboarding_step:apps`: `"1. Agent Apps"`, `onboarding_step:channels`: `"2. Human Channels →"`, `onboarding_step:hubs`: `"3. Hubs & Network →"`), each with `frame: NSRect(x: 8 + CGFloat(idx) * 156, y: 8, width: 152, height: 22)`, `radius: 6`, styled `.prominent` when `settingsActiveTab == step.0` and `.neutral` otherwise.
     - Always show the section header `"CONNECTED AGENT APPS"` and subtitle `"Enable 1-click notification & wake support in your coding apps."` at the top of `appsView`.
     - For `apps_next_step_callout` (at the bottom of `appsView` below the last app card at `NSRect(x: 20, y: 226 - CGFloat(max(0, resolvedApps.count - 1)) * 90 - 46, width: 480, height: 38)` when at least 1 app is configured and `!nextStepCalloutDismissed` and no Human Channel is configured yet):
       - Label (`NSRect(x: 12, y: 10, width: 264, height: 18)`, `size: 10`): `"Agent app connected · Optional: add a Human Channel"`.
       - Include both **`Human Channels →`** (`id: "goto_channels_tab"`, `frame: NSRect(x: 282, y: 7, width: 136, height: 24)`) and **`Skip`** (`id: "skip_next_step"`, `frame: NSRect(x: 424, y: 7, width: 48, height: 24)`, which sets `nextStepCalloutDismissed = true` and rebuilds).

6. **`[SHARP]` `U6` — Allow Skipping Optional Human Channel Setup (`src/macos/cito-ui.swift`)**:
   - **Problem**: Users landing on `Human Channels` had no `Skip` option if they only wanted `Mac Desktop Banner`.
   - **Contract**:
     - Preserve `"○ Not Configured"` on `settings_channel_status:\(ch.id)` when `ch.status == "not_configured"` and `!ch.hub_provided`, and render an explicit **`Skip for Now`** button (`id: "skip_channel:\(ch.id)"`, `frame: NSRect(x: 132, y: 10, width: 96, height: 24)`) inside `stratum_user:\(ch.id)`'s bottom action bar whenever `!ch.configured`. Clicking `skip_channel:\(ch.id)` sets `nextStepCalloutDismissed = true` and switches `settingsActiveTab = "hubs"` without mutating Hub state.

7. **`[SHARP]` `U7` — Eliminate All Button/Badge Truncation & Right-Align `CONNECTED HUBS` Row Controls (`src/macos/cito-ui.swift`)**:
   - **Problem**:
     1. `"Experimental"` pill on Claude Code truncated to `"Experimen..."` (`Screenshot 1`).
     2. `"✨ Paste from Invite"` and `"+ Add Hub…"` in `Hubs & Network` truncated to `"✨ Paste from Inv..."` and `"+ + Add Hu..."` (with duplicate `+ +`, `Screenshot 4`).
     3. On the `local` hub row, `Edit` (`x: 192`) and `Enabled` (`x: 318`) left an `84pt` empty gap on the right side (`Screenshot 4`).
   - **Contract**:
     - **`settings_app_tier:<id>` (`"Experimental"` badge)**: cap `titleW` on `settings_app:<id>` to `min(app.tier == "experimental" ? 80 : 210, ceil(measureTextWidth(app.display_name, font: titleFont)) + 4)` so `settings_app_tier:<id>` (`frame: NSRect(x: 54 + titleW, y: 56, width: 88, height: 16)`, `maxX <= 222`, with inner label `NSRect(x: 6, y: 1, width: 76, height: 14)` and `alignment = .center`) never overlaps `settings_app_status:<id>` (`x: 230`).
     - **`paste_hub_invite`**: title `"Paste Invite"`, `leadingSymbol: "doc.on.clipboard"`, `frame: NSRect(x: 272, y: 190, width: 116, height: 22)`.
     - **`open_add_hub`**: title `"Add Hub…"` (no `+` in title string since `leadingSymbol: "plus"` draws the icon!), `leadingSymbol: "plus"`, `frame: NSRect(x: 396, y: 190, width: 104, height: 22)`.
     - **Right-aligned Hub Row Controls in `CONNECTED HUBS` (`settings_hub:<name>`, width `480pt`)**:
       - Anchor buttons flush to the right edge (`rightX = 468`) with `6pt` spacing:
         - When `hub.name == "local"`:
           - Subtitle: `"Built-in on this Mac (local://) · Handle: @\(hub.user ?? "local")"`
           - `toggle_hub:local` (`"Enabled"` / `"Disabled"`): `NSRect(x: 396, y: 13, width: 72, height: 22)`
           - `edit_hub:local` (`"Edit"`): `NSRect(x: 338, y: 13, width: 52, height: 22)`
           - Title & subtitle width: `300pt` (`x: 28..328`).
         - When `hub.name != "local"`:
           - `remove_hub:<name>` (`"Remove"`): `NSRect(x: 396, y: 13, width: 72, height: 22)`
           - `toggle_hub:<name>` (`"Enabled"` / `"Disabled"`): `NSRect(x: 318, y: 13, width: 72, height: 22)`
           - `edit_hub:<name>` (`"Edit"`): `NSRect(x: 260, y: 13, width: 52, height: 22)`
           - If `hub.auth_mode == "oidc"`: `login_hub:<name>` (`"Sign In…"`): `NSRect(x: 184, y: 13, width: 70, height: 22)`.

8. **`[SHARP]` `U8` — `Share Hub on Local Network`: Visible/Copyable Token & Immediate `✓ Saved` Feedback (`src/macos/cito-ui.swift`, `daemon.rs`)**:
   - **Problem**: When `Auth: Token` was selected on `Share Hub on Local Network`, the token was invisible in the UI, and clicking `Apply` gave zero visual feedback (`Screenshot 8`).
   - **Contract**:
     - In `daemon.rs` `set_host_hub`: when `auth_mode == "token"` and `token` in `host_hub` is empty, generate and persist a random token even when `enabled == false` so `settings_for_hub` immediately returns the non-empty `host_hub.token`.
     - In `src/macos/cito-ui.swift`:
       - Generalize `MenuControl.copied` to `func copied(_ message: String = "✓ Copied")`.
       - In `host_hub_card`: when `authMode == "token"` and `!hostHub.token.isEmpty`, narrow `host_hub_endpoint` to `NSRect(x: 14, y: 46, width: 254, height: 16)` (`width: 452` otherwise) and display a **`Copy Token`** pill (`id: "copy_host_hub_token"`, `frame: NSRect(x: 274, y: 44, width: 192, height: 20)`, `leadingSymbol: "key.fill"`) next to `host_hub_endpoint` showing `"Token: \(String(hostHub.token.prefix(8)))… Copy"`, which copies `hostHub.token` to the clipboard and calls `copied("✓ Token Copied")`!
       - Add a `"Port:"` label (`NSRect(x: 188, y: 16, width: 34, height: 15)`) before `host_hub_port` (`NSRect(x: 222, y: 12, width: 58, height: 24)`).
       - Widen `toggle_host_hub_auth` (`"Auth: Token"` / `"Auth: None"`, `NSRect(x: 286, y: 12, width: 102, height: 24)`) and `apply_host_hub_port` (`"Apply"`, `NSRect(x: 394, y: 12, width: 72, height: 24)`).
       - On `copy_hub_invite`: call `copyInvite.copied("✓ Copied")` when clicked.
       - On `apply_host_hub_port`: validate that the trimmed port string parses as an integer in `1..=65535`; when valid, emit `set_host_hub` and show `"✓ Saved"` (`apply.copied("✓ Saved")`) while `host_hub.error` is `nil` on the resulting `settings` frame (if the port string is invalid or `settings` returns `host_hub.error != nil`, display the error in `host_hub_error` in `.systemRed` and keep/reset the button title at `"Apply"`).

9. **`[SHARP]` `U9` — Explicit Field Labels & Protocol Explanations in `Add / Edit Hub` Form (`src/macos/cito-ui.swift`)**:
   - **Problem**: `add_hub_form` had zero labels above its text fields and zero explanation of `local://` vs `http://` / `https://` (`Screenshot 9`).
   - **Contract**:
     - Expand `add_hub_form` height inside `hubsView` (`NSRect(x: 20, y: 4, width: 480, height: 184)`):
     - **When editing `local` (`editingExistingHub && editingOriginalHub == "local"`)**:
       - Header (`NSRect(x: 14, y: 156, width: 452, height: 18)`, `size: 12.5, weight: .semibold`): `"Edit Built-in Local Hub (local://)"`
       - Protocol description (`NSRect(x: 14, y: 134, width: 452, height: 16)`, `size: 11, color: .secondaryLabelColor`): `"Runs in-process on this Mac (SQLite-backed). No external server or auth needed."`
       - Field Label (`NSRect(x: 14, y: 104, width: 452, height: 14)`, `size: 10, weight: .semibold, color: .secondaryLabelColor`): `"YOUR DISPLAY HANDLE (@USERNAME)"`
       - Input (`NSRect(x: 14, y: 76, width: 452, height: 24)`): `makeEditableField("settings_hub_user", ..., placeholder: "e.g. alice", value: addHubUser)`
       - Helper text (`NSRect(x: 14, y: 54, width: 452, height: 14)`, `size: 10.5, color: .secondaryLabelColor`): `"Shown to local agent sessions when you send or receive signals on this Mac."`
       - Keep `settings_hub_name` (`"local"`) and `settings_hub_url` (`"local://"`) registered in `settingsFieldControls` (hidden) so form submission works seamlessly.
     - **When adding or editing a Remote / Shared Hub (`!editingExistingHub || editingOriginalHub != "local"`)**:
       - Row 1 Labels (`y: 164`, `size: 10, weight: .semibold, color: .secondaryLabelColor`):
         - `"HUB NAME"` (`NSRect(x: 14, y: 164, width: 146, height: 13)`)
         - `"HUB URL (http://ip:4291 or https://host)"` (`NSRect(x: 168, y: 164, width: 298, height: 13)`)
       - Row 1 Inputs (`y: 138`):
         - `settings_hub_name` (`NSRect(x: 14, y: 138, width: 146, height: 24)`, placeholder `"e.g. team or office-mac"`)
         - `settings_hub_url` (`NSRect(x: 168, y: 138, width: 298, height: 24)`, placeholder `"http://192.168.1.10:4291"`)
       - Row 2 Label & Auth Mode Pills (`y: 106`):
         - Label `"AUTH:"` (`NSRect(x: 14, y: 110, width: 42, height: 14)`, `size: 10, weight: .semibold, color: .secondaryLabelColor`)
         - Pills: `settings_hub_auth:none` (`"Trusted / Tailscale"`, `x: 60, width: 132`), `settings_hub_auth:token` (`"Shared Token"`, `x: 198, width: 104`), `settings_hub_auth:oidc` (`"Team OIDC"`, `x: 308, width: 92`)
       - Row 3 Labeled Auth Inputs (`y: 82` label, `y: 56` input):
         - Mode `"none"`: Label `"YOUR DISPLAY HANDLE (@USERNAME, OPTIONAL)"` (`x: 14, y: 82, width: 452`) + `settings_hub_user` (`x: 14, y: 56, width: 452, height: 24`, placeholder `"Defaults to your Mac username"`)
         - Mode `"token"`: Labels `"SHARED ACCESS TOKEN"` (`x: 14, y: 82, width: 274`) & `"YOUR HANDLE (OPTIONAL)"` (`x: 296, y: 82, width: 170`) + `settings_hub_token` (`x: 14, y: 56, width: 274, height: 24`, placeholder: `editingExistingHub ? "•••••• (Saved — enter new token to replace)" : "Paste token from host Mac"`) & `settings_hub_user` (`x: 296, y: 56, width: 170, height: 24`)
         - Mode `"oidc"`: Labels `"OIDC ISSUER URL"` (`x: 14, y: 82, width: 260`) & `"OIDC CLIENT ID"` (`x: 282, y: 82, width: 184`) + `settings_hub_issuer` (`x: 14, y: 56, width: 260, height: 24`, placeholder `"https://id.example.com"`) & `settings_hub_client_id` (`x: 282, y: 56, width: 184, height: 24`, placeholder `"cito"`)
       - Row 4 (`y: 12`):
         - Helper caption (`NSRect(x: 14, y: 15, width: 284, height: 14)`, `size: 10, color: .secondaryLabelColor`): `"Supports http://ip:4291 (Mac Share) or https:// (Team Hub)."`
         - `cancel_add_hub` (`NSRect(x: 306, y: 12, width: 72, height: 24)`) + `submit_add_hub` (`NSRect(x: 386, y: 12, width: 80, height: 24)`).

10. **`[SHARP]` `U10` — Mechanical Truncation Detector (`truncated_labels`), Offscreen PNG Snapshot Renderer (`test_render_png`) & Multimodal Visual Gate (`src/macos/cito-ui.swift`, `tests/support/ui.rs`)**:
    - In `src/macos/cito-ui.swift` `inspect()`:
      - Traverse all visible `MenuControl` instances and `settings_app_tier:*` badge labels across `items`, `settings_items`, and `directory_items`, and record in `snapshot["truncated_labels"]` (`[String]`) any control whose `ceil(measureTextWidth(label.stringValue, font: label.font)) > label.frame.width + 2.0`.
      - Support `{"op": "test_render_png", "surface": "menu" | "settings" | "directory", "path": "<file.png>"}` in `cito-ui.swift`, rendering the requested surface (`settingsPanel?.contentView`, `directoryPanel?.contentView`, or a composite `FlippedView` of `cardMenu.items`) to a 2x Retina PNG file on disk and emitting `diagnostic("rendered " + path)`.
    - In `tests/support/ui.rs` and `v25.hygiene`:
      - Assert `assert_eq!(s["truncated_labels"], json!([]))` across all `SettingsPanel` tabs/forms, `cardMenu` states, and `DirectoryPanel` states.
      - Render PNG snapshots of all 8 core UI states and inspect them with `uiux-auditor` multimodal image review until zero visual/UX defects remain:
        1. `menu_human_paused.png`: `cardMenu` with `human_paused_card` (`"Human Node (@<user>)"`, `listen_human` `"Enable"`) and configured-only `AVAILABLE SESSIONS` (`U1`–`U3`).
        2. `settings_apps.png`: `SettingsPanel → Agent Apps` with default closed `?` guide, full `"Experimental"` badge (`settings_app_tier:*`), and `apps_next_step_callout` (`"Human Channels →"` + `"Skip"`) (`U4`, `U5`, `U7`).
        3. `settings_guide_open.png`: `SettingsPanel` with `onboarding_banner` toggled open above the shifted `300pt` tab viewport (`y: 44..344`) with zero overlap (`U5`).
        4. `settings_channels_unconfigured.png`: `SettingsPanel → Human Channels` showing `"TARGET HUB:"` label, `stratum_hub:<id>` (`stratum_hub_title:<id>`, `"○ Gateway Token Required"`, `channel_setup_guide:<id>`), and `stratum_user:<id>` (`stratum_user_title:<id>`, `skip_channel:<id>` `"Skip for Now"`) (`U6`, `U11`).
        5. `settings_channels_pairing.png`: `SettingsPanel → Human Channels` with active Hub Gateway (`"● Hub Gateway Active"`, `clear_channel_service:<id>`) and active `pairing_instruction:<id>` (`copy_pair_command:<id>`, `cancel_pair_channel:<id>`) inside `stratum_user:<id>` (`U11`).
        6. `settings_hubs_token.png`: `SettingsPanel → Hubs & Network` showing `host_hub_card` (`copy_host_hub_token`, `"Port:"` label, `toggle_host_hub_auth`, `apply_host_hub_port`) and right-aligned `CONNECTED HUBS` rows (`paste_hub_invite`, `open_add_hub`) (`U7`, `U8`).
        7. `settings_hubs_edit_local.png`: `SettingsPanel → Hubs & Network` with `edit_hub:local` form (`"Edit Built-in Local Hub (local://)"`, `"YOUR DISPLAY HANDLE (@USERNAME)"`, placeholder `"e.g. alice"`) (`U9`).
        8. `settings_hubs_add_remote.png`: `SettingsPanel → Hubs & Network` with `add_hub_form` for a remote Hub showing all persistent field labels (`"HUB NAME"`, `"HUB URL (http://ip:4291 or https://host)"`, `"AUTH:"`, and auth-mode specific labels) (`U9`).

11. **`[SHARP]` `U11` — Two-Stratum Human Channel Card (`stratum_hub:<id>` vs. `stratum_user:<id>`) Separating Hub Gateway (`scope: "hub"`) from Personal Human Node Binding (`scope: "user"`) (`src/macos/cito-ui.swift`, `plugins/telegram/plugin.json`)**:
    - **Problem**: `settings_channel:<id>` rendered `scope: "hub"` gateway credentials (configured once per Hub by the Hub Owner/Admin) and `scope: "user"` personal node bindings (configured by each Human Node joining the Hub) as flat sibling rows inside one box, failing to distinguish shared Hub Gateway infrastructure from personal Human Node enrollment.
    - **Contract**:
      - On `SettingsPanel → Human Channels` (`settings_channels_tab`):
        - Add a persistent `"TARGET HUB:"` label (`NSRect(x: 20, y: 342, width: 78, height: 14)`, `size: 10, weight: .semibold, color: .secondaryLabelColor`) to the left of the Hub selector scroll view (`x: 102, y: 336, width: 398, height: 28`).
        - Keep all strings inside `src/macos/cito-ui.swift` 100% vendor-neutral (zero occurrences of `"Telegram"` or `"@BotFather"` in `src/macos/cito-ui.swift`), while sourcing channel-specific setup steps and field hints from `ch.setup_guide`, `f.help_text`, and `ch.pairing_help_text` declared in `plugins/<id>/plugin.json` (e.g. `plugins/telegram/plugin.json`).
        - Inside each Hub-routed channel card (`settings_channel:\(ch.id)`, width `480pt`), partition `visibleFields` into `hubFields = visibleFields.filter { $0.scope == "hub" }` and `userFields = visibleFields.filter { $0.scope != "hub" }`, detect whether the plugin defines any Hub-scoped fields (`let hasHubScopeFields = ch.fields.contains { $0.scope == "hub" }`), and compute explicit sub-container heights and frames:
          - `hubFieldsH = hubFields.reduce(CGFloat(0)) { $0 + ($1.help_text?.isEmpty == false ? 48 : 32) }`
          - `userFieldsH = userFields.reduce(CGFloat(0)) { $0 + ($1.help_text?.isEmpty == false ? 48 : 32) }`
          - `guideH = (!ch.configured && !guide.isEmpty && !hubFields.isEmpty) ? max(56, ceil((guide as NSString).boundingRect(with: NSSize(width: 432, height: .greatestFiniteMagnitude), options: [.usesLineFragmentOrigin], attributes: [.font: NSFont.systemFont(ofSize: 10.5)]).height) + 8) : 0`
          - `hubStratumH: CGFloat = 52 + guideH + hubFieldsH`
          - `userStratumH: CGFloat = 88 + userFieldsH + (instructionHeight > 0 ? instructionHeight + 8 : 0)`
          - `cardHeight: CGFloat = 60 + hubStratumH + 8 + userStratumH + 8`
          - Render **two visually distinct sub-containers** inside `settings_channel:\(ch.id)`:
          1. **Upper Sub-Container — `stratum_hub:\(ch.id)` (`NSRect(x: 12, y: 8 + userStratumH + 8, width: 456, height: hubStratumH)`, `radius: 8`)**:
             - Header label (`id: "stratum_hub_title:\(ch.id)"`, `NSRect(x: 12, y: hubStratumH - 22, width: 220, height: 14)`): `"1. HUB GATEWAY · ONCE PER HUB"` (`size: 10, weight: .bold, color: .secondaryLabelColor`, measured `195pt <= 220pt`).
             - Status badge on the right of `stratum_hub:\(ch.id)`:
               - When `ch.hub_provided || ch.status != "not_configured"`: `"● Hub Gateway Active"` (`NSRect(x: 236, y: hubStratumH - 22, width: 132, height: 14)`, `.systemGreen`) + (when `ch.fields.contains(where: { $0.scope == "hub" && $0.configured && $0.hidden != true })`) **`Clear Token`** button (`id: "clear_channel_service:\(ch.id)"`, `NSRect(x: 372, y: hubStratumH - 25, width: 76, height: 20)`, `.destructiveHover`).
               - When `!ch.hub_provided && ch.status == "not_configured"`: `"○ Gateway Token Required"` (`NSRect(x: 280, y: hubStratumH - 22, width: 164, height: 14)`, `alignment = .right`, `.secondaryLabelColor`).
             - Role description (`NSRect(x: 12, y: hubStratumH - 40, width: 432, height: 14)`, `size: 10.5, color: .secondaryLabelColor`) & fields:
               - If `!hubFields.isEmpty`: render `"Configured once by the Hub owner. Shared by all human nodes on '\(effectiveChannelHub)'."`, optional `channel_setup_guide:\(ch.id)` (`NSRect(x: 12, y: 8 + hubFieldsH, width: 432, height: guideH)` from `ch.setup_guide` when `!ch.configured`), and `hubFields` (`channel_field:\(ch.id):\(f.key)` with left label `x: 12, width: 140` and input `x: 158, width: 286` plus `f.help_text`).
               - If `hubFields.isEmpty && hasHubScopeFields` (non-admin OIDC user on a shared Hub where `scope: "hub"` fields have `hidden == true`):
                 - When `ch.hub_provided`: `"Provided by '\(effectiveChannelHub)' Hub administrator — no gateway setup needed on your Mac."`
                 - When `!ch.hub_provided`: `"Not yet configured by the '\(effectiveChannelHub)' Hub administrator."`
          2. **Lower Sub-Container — `stratum_user:\(ch.id)` (`NSRect(x: 12, y: 8, width: 456, height: userStratumH)`, `radius: 8`)**:
             - Header label (`id: "stratum_user_title:\(ch.id)"`, `NSRect(x: 12, y: userStratumH - 22, width: 432, height: 14)`): `"2. YOUR PERSONAL NODE (@\(currentUser)) · EACH HUMAN ON '\(effectiveChannelHub.uppercased())'"` (`size: 10, weight: .bold, color: .secondaryLabelColor`)
             - Role description (`NSRect(x: 12, y: userStratumH - 40, width: 432, height: 14)`, `size: 10.5, color: .secondaryLabelColor`): `"Bind your personal \(ch.display_name) account to receive agent wakes and send/reply to agents on '\(effectiveChannelHub)'."`
             - Renders `userFields` (`channel_field:\(ch.id):\(f.key)`), the active pairing instruction row (`pairing_instruction:\(ch.id)` at `NSRect(x: 14, y: 40, width: 278, height: instructionHeight)` + `copy_pair_command:\(ch.id)` `"Copy Command"` at `NSRect(x: 298, y: 42, width: 114, height: 22)` + `cancel_pair_channel:\(ch.id)` `"Cancel"` at `NSRect(x: 416, y: 42, width: 50, height: 22)` in `chCard` coordinates), and the personal node action bar (`y: 10` in `chCard` coordinates):
               - `pair_channel:\(ch.id)` (`"Pair Account…"` / `"Re-pair…"`, `NSRect(x: 14, y: 10, width: 112, height: 24)`)
               - When `ch.configured`: `toggle_channel:\(ch.id)` (`"Pause"` / `"Resume"`, `NSRect(x: 132, y: 10, width: 64, height: 24)`) + `unpair_channel:\(ch.id)` (`"Unpair"`, `NSRect(x: 202, y: 10, width: 62, height: 24)`, `.destructiveHover`)
               - When `!ch.configured`: `skip_channel:\(ch.id)` (`"Skip for Now"`, `frame: NSRect(x: 132, y: 10, width: 96, height: 24)`)
               - Right-aligned: `test_channel:\(ch.id)` (`"Send Test"`, `NSRect(x: 312, y: 10, width: 80, height: 24)`) + `save_channel:\(ch.id)` (`"Save"`, `NSRect(x: 398, y: 10, width: 68, height: 24)`, `.prominent`).

---

## 12. Stage 10 (`v0.20.2` — Phase `v26`): Root-Cause Equivalence-Class Closure (`R1`–`R5`), True Progressive Disclosure, Full Mechanical Geometry Invariants, Closed-Loop Error Surfacing, Bridge Readiness & `Daemon<->UI` Closed-Loop Verification

Stage 10 (`v0.20.2` — Phase `v26`) supersedes any conflicting layout, coordinate, or action rules in `§10`–`§11` and closes the five systemic root-cause equivalence classes (`R1`–`R5`) identified by the multi-perspective root-cause audit (`pathway-auditor`, `uiux-auditor`, and `structural-auditor`):

### 12.1 `R1` — True Two-Stratum Progressive Disclosure, Independent Scope Saving (`save_hub_service:<id>`, `edit_hub_service:<id>`) & Prerequisite Gating (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`, `hub.rs`)

1. **`[SHARP]` Independent Scope Collection & RPC Routing**:
   - Split `collectValues()` in `src/macos/cito-ui.swift` into scope-filtered collection (`collectValues(scopeFilter: "hub")` vs. `collectValues(scopeFilter: "user")`), and clear acknowledged secret drafts in `channelFieldDrafts` upon successful channel save so saved secrets are never stale-resubmitted.
   - In `crates/cito-native/src/core/daemon.rs`, support `"save_hub_service"` (alongside `"channel_save" | "channel_pair" | "channel_test"`): when `op == "save_hub_service"` (or `"scope": "hub"`), populate **only** `params["hub_config"]` and omit `"user_config"` and `"enabled"` so `hub.rs` updates `hub_plugin_services` without creating/modifying a row in `hub_human_channels` or resuming a paused personal binding. Also partition `values` against the target Hub's schema when a remote schema is present.
2. **`[SHARP]` Progressive Collapse of `stratum_hub:<id>` (`1. HUB GATEWAY · ONCE PER HUB`)**:
   - Track `var editingHubGateway = Set<String>()` keyed by `"\(effectiveChannelHub)|\(ch.id)"`.
   - Define:
     - `let gatewayActive = ch.hub_provided || ch.status != "not_configured" || (hasHubScopeFields && ch.fields.filter { $0.scope == "hub" }.allSatisfy { $0.configured })`
     - `let hubGatewayExpanded = !gatewayActive || editingHubGateway.contains(channelKey)`
   - **When Collapsed (`!hubGatewayExpanded`)**: render a compact single-row bar (`height: 36pt` for admin/local, `50pt` for non-admin with role caption) with:
     - `stratum_hub_title:\(ch.id)` (`"1. HUB GATEWAY · ONCE PER HUB"`, `NSRect(x: 10, y: hubStratumH - 25, width: 168, height: 14)`)
     - `stratum_hub_status:\(ch.id)` (`"● Hub Gateway Active"`, `NSRect(x: 182, y: hubStratumH - 25, width: 106, height: 14)`, `.systemGreen`)
     - When `!hubFields.isEmpty`: `edit_hub_service:\(ch.id)` (`"Change Token…"`, `NSRect(x: 292, y: hubStratumH - 28, width: 82, height: 20)`) and `clear_channel_service:\(ch.id)` (`"Clear Token"`, `NSRect(x: 378, y: hubStratumH - 28, width: 70, height: 20)`, `.destructiveHover`).
   - **When Expanded (`hubGatewayExpanded`)**: render `stratum_hub_title:\(ch.id)`, `stratum_hub_status:\(ch.id)`, `gatewayDescription`, `channel_setup_guide:\(ch.id)` (when `!gatewayActive`), `hubFields`, and the Step-1 action bar (`y: 6, height: 24`) containing:
     - `save_hub_service:\(ch.id)` (`"Save Gateway Token"`, `NSRect(x: 304, y: 6, width: 140, height: 24)`, `.prominent`)
     - When `gatewayActive`: `cancel_edit_hub_service:\(ch.id)` (`"Cancel"`, `NSRect(x: 232, y: 6, width: 66, height: 24)`) and `clear_channel_service:\(ch.id)` (`"Clear Token"`, `NSRect(x: 12, y: 6, width: 84, height: 24)`).
     - When `acceptSettings` confirms `gatewayActive == true`, automatically remove `channelKey` from `editingHubGateway` so Step 1 collapses.
3. **`[SHARP]` Step-2 Prerequisite Gating in `stratum_user:<id>` (`2. YOUR PERSONAL NODE`)**:
   - Express all subviews in `stratum_user:<id>` directly in `userStratum`'s `456pt` coordinate space (`x: 12..444`, removing the negative-offset `addPersonalControl` hack).
   - In `MenuControl`, guard `mouseDown` and `accessibilityPerformPress` with `guard isEnabled else { return }` so `isEnabled = false` genuinely blocks invocation.
   - When `!gatewayActive`: set `isEnabled = false` (and `alphaValue = 0.55`) on `userFields` inputs, `pair_channel:\(ch.id)`, `test_channel:\(ch.id)`, and `save_channel:\(ch.id)`, while keeping `skip_channel:\(ch.id)` (`"Skip for Now"`) enabled (`isEnabled = true, alphaValue = 1.0`), and display `"Complete Step 1 (Hub Gateway Token) above to enable personal pairing on '\(effectiveChannelHub)'."` in `personalRole`.

---

### 12.2 `R2` — Full Mechanical Geometry Invariants (`truncated_labels`, `overlapping_siblings`, `clipped_views`) & Coordinate Fixes (`src/macos/cito-ui.swift`, `crates/cito-native/tests/support/ui.rs`)

1. **`[SHARP]` Three Mechanical Geometry Invariants in `inspectMenu()` (`src/macos/cito-ui.swift`)**:
   - Upgrade `inspectMenu()` in `src/macos/cito-ui.swift` to compute and emit all 3 geometry arrays across `items` (`cardMenu`), `settings_items` (`SettingsPanel`), and `directory_items` (`DirectoryPanel`):
     1. `truncated_labels`: checks every visible `MenuControl.label`, every `settings_app_tier:*` label, AND every visible non-editable `NSTextField` chrome label (checking single-line width when `maximumNumberOfLines == 1` and wrapped bounding height when `maximumNumberOfLines != 1`, excluding dynamic user-data fields with `.byTruncatingMiddle` or data-row prefixes `dir_`/`act_`).
     2. `overlapping_siblings`: for every visible container (`doc` for `NSScrollView`, row cells for `NSTableView`, or `view` otherwise), checks all pairs of visible non-`NSVisualEffectView` direct subviews `(a, b)` for `a.frame.insetBy(dx: 0.5, dy: 0.5).intersects(b.frame.insetBy(dx: 0.5, dy: 0.5))`.
     3. `clipped_views`: for every visible container (`doc.bounds` for `NSScrollView` document children, or `view.bounds` otherwise), checks every visible child for `minX < -0.5 || minY < -0.5 || maxX > parentBounds.width + 0.5 || maxY > parentBounds.height + 0.5`.
2. **`[SHARP]` Resolution of All 8 Discovered Geometry Defects (`G1`–`G8`)**:
   - **`G1`**: `open_directory` label width `180pt` vs `directory_online_badge` at `x: 208` (preventing horizontal overlap inside `open_directory`).
   - **`G2`**: `stratum_hub` header vs description vertical spacing and single-row collapsed header coordinates.
   - **`G3`**: `stratum_user` `pairing_instruction` spacing and direct `userStratum` (`456pt`) coordinate system (`x: 12..444`).
   - **`G4`**: `settings_app` `titleW` cap so `settings_app_tier:<id>` never overlaps `settings_app_status:<id>`.
   - **`G5`**: `host_hub_card` disjoint Y rows with expanded card `height: 128pt`.
   - **`G6`**: `settings_desktop_banner_card` subtitle sized to `10.5pt, width: 366pt` so single-line text never truncates.
   - **`G7`**: `directoryScrollView?.isHidden = empty` when `directory_empty_state` is visible so `directory_empty_state` and `directory_scroll` never overlap.
   - **`G8`**: `cardMenu` `"Cito"` header title widened to `width: 36pt`.

---

### 12.3 `R3` — Closed-Loop UI Actions, Visible Error Surfacing (`settings_error_banner`, `add_hub_error`, `menu_error_row`, `hub.error`) & Invite/Form Validation (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`)

1. **`[SHARP]` Daemon Error & Hub Error Propagation (`crates/cito-native/src/core/daemon.rs`)**:
   - Track `last_hub_errors: HashMap<String, String>` on `DaemonService` (`daemon.rs`), recording errors from `login_hub` / `hub_action` / `test_hub` and surfacing `"error": self.last_hub_errors.get(&name)` in `settings_for_hub` (instead of hardcoded `null`), clearing it on successful connection/action.
   - When `dispatch_action` fails in `spawn_ui` (`daemon.rs:3155`), emit `{"op": "error", "error": e, "source_op": action["op"], "hub": action["hub"]}` followed by `s.push_ui()`.
   - Unify `"paste_hub_invite"` in `daemon.rs` to accept JSON invites, `http(s)://` URLs, and `cito setup --hub ...` CLI strings.
2. **`[SHARP]` UI Error Surfacing & Closed-Loop Form/Button State (`src/macos/cito-ui.swift`)**:
   - Handle `frame.op == "error"` in `src/macos/cito-ui.swift` by resetting `pendingMutation = false` and `hostHubPortPending = false`, routing the error to:
     - `composerErrorLabel` (if composer pending),
     - `addHubError` (`id: "add_hub_error"` inside `add_hub_form` if `addHubFormOpen`),
     - `settingsBannerError` (`id: "settings_error_banner"` with `settings_error_label` and `dismiss_settings_error` in `SettingsPanel`, shifting scroll viewport height down by `36pt` so zero overlap occurs), or
     - `menuBannerError` (`id: "menu_error_row"` in `cardMenu`),
     and rebuilding immediately.
   - In `submit_add_hub`: validate required fields (`name`, `url` starting with `http://`, `https://`, or `local://`, `token` when `!editingExistingHub && addHubAuth == "token"`, `issuer` and `client_id` when `addHubAuth == "oidc"`); on validation error, keep `addHubFormOpen = true` and display `add_hub_error` (`NSRect(x: 14, y: 36, width: 452, height: 15)`, `.systemRed`). When valid, set `addHubSaving = true` and close `addHubFormOpen` only when the resulting `settings`/`state` frame confirms the saved hub without error.
   - In `paste_hub_invite`: on empty/invalid clipboard, set `settingsBannerError = "Clipboard does not contain a valid Hub invite or http(s):// URL."` and rebuild; on valid invite, emit `paste_hub_invite` / `setup_save_hub` and show `"✓ Pasted"`.
   - Preserve `hostHubPortDraft` across unrelated refreshes and only transition `apply_host_hub_port` to `"✓ Saved"` when the acknowledged `host_hub.port` matches the submitted port and `host_hub.error == nil`.
   - Validate `selectedChannelHub` against the current `settings.hubs` inventory in `effectiveChannelHub` so removing a Hub falls back to `"local"` cleanly.

---

### 12.4 `R4` — Process Discovery vs. Bridge Socket Readiness, `needs_reload` UX in `cardMenu`, & Bounded Exponential Retry Backoff (`plugins/pi/{native.rs,cito-bridge.ts}`, `crates/cito-native/src/core/session_activity.rs`, `src/macos/cito-ui.swift`)

1. **`[SHARP]` Bridge Socket Identification & `needs_reload` Discovery (`plugins/pi/cito-bridge.ts`, `plugins/pi/native.rs`)**:
   - In `plugins/pi/cito-bridge.ts`: when `identify` is called before `session_start` has populated `ctx` (e.g. immediately after `/reload`), fall back to `process.cwd()` and the latest `.jsonl` session file in `~/.pi/agent/sessions` (or `pid-${process.pid}`) so an active `.sock` bridge always returns `ok: true` with a valid session identity.
   - In `plugins/pi/native.rs`: when `process_candidates` discovers a running `pi` process that has **no** responding `sockets/pi-pid-<p>.sock` (unless `CITO_PI_PROCESS_DISCOVERY == "1"` test mode is active without reload gating), mark the candidate session in `discover_sessions` with `"needs_reload": true` and `"reload_hint": "Run /reload in Pi to enable 1-click listen"`, and do NOT allow `resolve_caller` / `probe` to enroll a socket-less `needs_reload` process as an active listener. In `update_badge`, if `!path.exists()` or `SESSION_MISMATCH` occurs on a clear, return `ok: true` (and when `!path.exists()` on a non-null badge for an unbridged process, return a non-spamming result or rely on `SessionActivity` backoff).
2. **`[SHARP]` Vendor-Neutral `needs_reload` Row in `cardMenu` (`src/macos/cito-ui.swift`)**:
   - Decode `let needs_reload: Bool?` and `let reload_hint: String?` on `Candidate`.
   - When `c.needs_reload == true`, render a `46pt`-tall row in `available_scroll` (using cumulative `y` offsets for variable row heights) showing `candidate_title:\(c.harness):\(c.session_id)`, orange subtitle `candidate_hint:\(c.harness):\(c.session_id)` (`c.reload_hint ?? "Run /reload in \(visual.name) to enable 1-click listen"`), and button `reload:\(c.harness):\(c.session_id)` (`"/reload"`, `leadingSymbol: "doc.on.doc"`, copying `"/reload"` to the clipboard, flashing `"✓ Copied"`, and emitting `refresh_available`).
3. **`[SHARP]` Bounded Exponential Backoff in `SessionActivity` (`crates/cito-native/src/core/session_activity.rs`)**:
   - Add `failures: u32` to `Badge` in `session_activity.rs`, resetting to `0` on success or intent change, and backing off `250ms -> 500ms -> 1s -> 2s -> 4s -> 8s -> 15s` (`BADGE_RETRY * 2^(failures-1)` capped at `15s`), and retire inactive historical badge entries once cleared or after bounded failure.

---

### 12.5 `R5` — Real `DaemonService <-> Headless cito-ui` Closed-Loop Integration Suite & Multimodal Retina PNG Verification (`crates/cito-native/tests/support/ui.rs`)

1. **`[SHARP]` Closed-Loop `DaemonService <-> Headless cito-ui` Integration Harness & Retina Snapshot Verification (`crates/cito-native/tests/support/ui.rs`)**:
   - Add a real closed-loop test harness in `crates/cito-native/tests/support/ui.rs` that wires `cito-ui` stdout actions directly into a real `DaemonService` (`dispatch_action` / `action`) and feeds the daemon's real emitted `state`, `settings`, `directory`, `pair_code`, `channel_test_result`, `composer_result`, and `error` frames back into `cito-ui` stdin.
   - Assert all three mechanical geometry invariants (`truncated_labels == []`, `overlapping_siblings == []`, and `clipped_views == []`) on every step of the closed-loop workflows.
   - Render 2x Retina PNG snapshots of all core states (including collapsed vs. expanded `stratum_hub:<id>`, `settings_error_banner`, `add_hub_error`, and `needs_reload` candidate row) for multimodal `uiux-auditor` verification.

---

### 12.6 Complete `R1`–`R5` Verification & Acceptance Matrix

| ID | Equivalence Class | Authoritative Modules | Required Verification / Regression Test |
| :--- | :--- | :--- | :--- |
| **`R1`** | Two-stratum progressive disclosure, independent scope saving (`save_hub_service:<id>`, `edit_hub_service:<id>`) & Step-2 prerequisite gating | `src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`, `hub.rs` | `collectValues(scopeFilter: "hub" \| "user")` isolates strata; `save_hub_service` updates `hub_plugin_services` without touching `hub_human_channels`; `stratum_hub:<id>` collapses to a `36pt`/`50pt` single-row summary when `gatewayActive` with `edit_hub_service:<id>` (`"Change Token…"`) and `clear_channel_service:<id>`; Step 2 (`stratum_user:<id>`) disables personal inputs/actions (`isEnabled = false`, `alphaValue = 0.55`) except `skip_channel:<id>` when `!gatewayActive`. |
| **`R2`** | Mechanical geometry invariants (`truncated_labels`, `overlapping_siblings`, `clipped_views`) & coordinate fixes (`G1`–`G8`) | `src/macos/cito-ui.swift`, `crates/cito-native/tests/support/ui.rs` | `inspectMenu()` reports `truncated_labels == []`, `overlapping_siblings == []`, and `clipped_views == []` across `cardMenu`, `SettingsPanel`, and `DirectoryPanel`, resolving all 8 coordinate defects (`G1`–`G8`). |
| **`R3`** | Closed-loop UI actions, visible error surfacing (`settings_error_banner`, `add_hub_error`, `menu_error_row`, `hub.error`) & invite/form validation | `src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs` | `DaemonService` tracks `last_hub_errors`, emits `{"op": "error", ...}` on action failure, and unifies `paste_hub_invite`; `cito-ui` surfaces errors in `add_hub_error`, `settings_error_banner` (shifting scroll viewport by `36pt`), `menu_error_row`, and `composerErrorLabel`, validates `submit_add_hub` before closing, preserves `hostHubPortDraft`, and falls back `effectiveChannelHub` to `"local"` on Hub removal. |
| **`R4`** | Process discovery vs. bridge socket readiness, vendor-neutral `needs_reload` UX in `cardMenu`, & bounded exponential retry backoff | `plugins/pi/{native.rs,cito-bridge.ts}`, `crates/cito-native/src/core/session_activity.rs`, `src/macos/cito-ui.swift` | `cito-bridge.ts` `identify` falls back to `process.cwd()` / latest `.jsonl` before `session_start`; unbridged processes emit `needs_reload: true` and cannot be enrolled by `resolve_caller`/`probe`; `cardMenu` renders a `46pt` `needs_reload` row with `reload:<harness>:<sid>` (`"/reload"`); `SessionActivity` backs off `250ms..15s` and retires inactive badge entries. |
| **`R5`** | Real `DaemonService <-> Headless cito-ui` closed-loop integration suite & multimodal Retina PNG verification | `crates/cito-native/tests/support/ui.rs` | Closed-loop harness pipes `cito-ui` stdout actions through a real `DaemonService` and feeds real daemon frames back to `cito-ui` stdin, asserting `truncated_labels == []`, `overlapping_siblings == []`, and `clipped_views == []` at every step and rendering 2x Retina PNG snapshots of all core states. |

## 13. V28 — Stage 11 v0.20.3: Light/Dark Appearance Contrast (CitoPalette), Agent-Only Menu Bar Counter, Human Node Card Title Normalization, Agent-vs-Human Reply Boundary & Explicit Menu Bar Activity Entry (C1–C5)

Stage 11 preserves the symmetric Node/Hub/Channels ontology and plugin boundary. These changes correct existing UI behavior without adding public CLI, MCP, or daemon/UI protocol fields. The implementation lives in `src/macos/cito-ui.swift` (`Companion`, `MenuControl`, `DirectoryPeerCellView`) and the existing registration/daemon paths. Class names in earlier design sections describe surfaces; they do not require separate controller classes.

### 13.1 C1 — Agent-only session counter and Human readiness

- `uniqueSessionCount` counts distinct `(harness, session_id)` pairs only where `harness != "human"`. Registering the same agent on two hubs still contributes one session; custom plugin harness IDs count normally.
- `humanListening` independently reports whether any Human listener exists.
- The status button title is `""` with no agents and no unread signals, `" N"` with agents, `" 🔔U"` with unread signals, or `" N 🔔U"` with both. Human enrollment never adds a permanent `1`.
- `header_subtitle` reads `Ready · N agent(s) listening`, optionally followed by ` · Human ready`; without agents it reads `Ready · Human listening` or `Ready · Idle`. The bounded label uses middle truncation and a full tooltip.

### 13.2 C2 — Human enrollment and compact card titles

- The daemon `listen_session` action and shared `registration::register_locked` path default missing, empty, or whitespace-only Human titles to `Human`. The normalized title reaches the response, local SQLite row, and Hub registration. Agent titles and nonempty custom Human titles remain unchanged.
- Human cards display `Human Node (@user)` for default titles (`Human`, empty/whitespace, `Human Node…`) and the legacy auto-generated `@user` title. Identity comes from the listener's `user` or `owner_user`, then that listener's Hub account, then the selected account.
- Default `about` values `Human` and `Human receiver` never create a second description line. A distinct custom description still creates a `78pt` row; a default Human card stays a compact `64pt` row (`58pt` inner card) at startup and after Disable → Enable. Custom titles remain visible.

### 13.3 C3 — Dynamic semantic colors and contrast oracle

`CitoPalette` uses `NSColor(name: nil)` providers and `bestMatch(from: [.darkAqua, .aqua])`. RGB components are sRGB; alpha defaults to `1`.

| Token | Light (`aqua`) | Dark (`darkAqua`) |
| :--- | :--- | :--- |
| `successText` | `(0.04, 0.43, 0.16)` | `(0.28, 0.85, 0.44)` |
| `warningText` | `(0.64, 0.28, 0.00)` | `(1.00, 0.70, 0.25)` |
| `dangerText` | `(0.74, 0.10, 0.14)` | `(1.00, 0.44, 0.42)` |
| `infoText` | `(0.00, 0.34, 0.75)` | `(0.40, 0.74, 1.00)` |
| `copiedFill` | `(0.04, 0.43, 0.16, 0.14)` | `(0.19, 0.82, 0.35, 0.22)` |
| `copiedBorder` | `(0.04, 0.43, 0.16, 0.34)` | `(0.19, 0.82, 0.35, 0.45)` |
| `warningFill` | `(0.64, 0.28, 0.00, 0.14)` | `(1.00, 0.62, 0.04, 0.18)` |
| `warningBorder` | `(0.64, 0.28, 0.00, 0.32)` | `(1.00, 0.62, 0.04, 0.40)` |

- Status text and icons use these semantic tokens instead of raw system green/orange/red/blue. Secondary text uses opaque appearance-specific gray to avoid low-contrast system secondary-label alpha. White-on-filled controls retain dark blue/red fills in both appearances.
- Dynamic layer colors resolve inside `effectiveAppearance.performAsCurrentDrawingAppearance`. Copied chips composite `copiedFill` onto the canonical surface rather than stacking card tints; warning chips composite `warningFill` onto the native text-surface base. These opaque chip backings preserve the specified foreground/fill values while preventing stacked tint contrast losses.
- The headless-only `test_inspect_menu` (also `test_inspect`) and `test_render_png` commands accept optional `appearance: "light" | "dark"`. Appearance propagates to menu views and windows before layout. Snapshots use exactly 2 pixels per point.
- Offscreen capture uses `#ECEFF3` (Light) or `#1E2024` (Dark). Behind-window visual effects have no desktop to sample offscreen, so capture temporarily substitutes this declared backdrop, then restores live materials and root fills. Rendering runs under the effective drawing appearance. Snapshots verify deterministic material backdrops, not every possible desktop wallpaper.
- `inspectMenu()` resolves visible `NSTextField` text (including `MenuControl` titles) into sRGB, composites alpha and nested layer fills onto the same canonical backdrop, linearizes sRGB, and computes WCAG `(Lmax + 0.05) / (Lmin + 0.05)`. Non-disabled labels with effective view alpha greater than `0.6` and ratio below `4.5` appear in `low_contrast_labels`; each inspected label also exposes its measured ratio. Hidden labels are excluded.

### 13.4 C4 — Human reply boundary and agent Activity navigation

- Human-targeted wake banners say `Click to reply in Cito ↩`; clicking opens the existing inline reply composer for the sender.
- Every non-Human target says `Click to view in Activity ↗`; clicking clears the composer, opens Activity with All Activity selected, clears search, and selects the matching `signal_id` or index `0`. It never opens a Human reply composer. `wake_action_text` exposes the actual action string to headless inspection.
- Activity cells show Reply only for `item.for_me`. Both button activation and Return guard this boundary. Agent-to-agent activity remains viewable without exposing a Human reply action. This rule does not restrict explicit Message actions on Directory peers.

### 13.5 C5 — Explicit side-by-side menu entries

Zone 4 contains two `174 × 28pt` controls, at `x = 12` and `x = 194`, within the `356pt` content span:

- `open_directory`: `Directory…`, `magnifyingglass`, `N online`, tooltip `Search Network Directory (⌘K)`. Always opens Directory, regardless of unread count.
- `open_activity`: `Activity…`, `bell.badge.fill` with unread signals or `clock.arrow.circlepath` otherwise; trailing `U unread` or `History`, tooltip `Open Activity Stream`. Always opens Activity.
- Each emits the existing `open_directory` action with explicit `mode: "directory" | "activity"`.

### 13.6 Verification and release alignment

- `crates/cito-native/tests/support/ui.rs` exercises counters, real daemon Disable → Enable, default and legacy titles, duplicate agent enrollment, both wake actions, Reply/Return guards, explicit entry modes, and Light/Dark contrast across menu, Directory, and all three Settings tabs.
- Run `CITO_TEST_SNAPSHOT_DIR=/tmp/cito-v28-snapshots cargo test --test native ui::` and visually inspect `c2_human_reenabled_{light,dark}.png` and `c3_*_{light,dark}.png`. Both appearances must report `low_contrast_labels == []` alongside all three existing geometry invariants.
- Run `cargo test --workspace && cargo build --release` without compiler warnings, and `/usr/bin/ruby -c packaging/homebrew/cito.rb`. Verify zero concrete agent/channel names in core Rust and Swift, staged signed binaries, and `bin/cito --version` reporting `cito v0.20.3`.
- `crates/cito-native/Cargo.toml` remains the canonical product version. Cargo.lock, plugin distribution metadata, the Homebrew formula, README, and website release metadata align at `0.20.3`. Stage/phase identifiers and SQLite schema versions remain independent; no schema migration is required.

---

## 14. V29 — Stage 12 v0.21.1: Holistic UI/UX & Cross-Layer Pathway Remediation (`D01`–`D20`)

Stage 12 resolves the 5 Fundamental UI/UX & Pathway Defect Essences identified across Round 1 and Round 2 full-scope discovery (`uiux-auditor`, `structural-auditor`, `pathway-auditor`) and user acceptance testing. It introduces sender auto-enrollment and reply-routing closure (`D18`), a backward-compatible human-readable address alias contract (`@<user>` and `<user>/<harness>#<prefix>`), a unified domain-to-UI presentation boundary, state-driven progressive disclosure, full-window AppKit keyboard/focus ownership, causal request/idempotency correlation, and a 6-invariant mechanical verification oracle covering all 4 UI surfaces (`cardMenu`, `directoryPanel`, `settingsPanel`, and `panel` / `WakeBanner`).

### 14.1 Binding Contract (`MUST`)

#### 1. Principle 1 — Domain-to-UI Presentation Boundary & Jargon Elimination (`D01`–`D05`, `F02`, `F10`, `F11`)
- **Basis**: Prevent internal database/wire representations (`harness = "human"`, `url = "local://"`, `auth_mode`, `about = "Session"`, raw `CODE:` prefixes) from leaking into user-visible labels, subtitles, facets, or tooltips, while keeping machine error codes intact in JSON/RPC wire responses.
- **Human Receiver Presentation (`D01`)**:
  - **Menu Header Subtitle (`header_subtitle`)**: When active agent session count `N > 0`, display `"\(N) session\(N == 1 ? "" : "s") listening"`; when `N == 0`, display `"Idle"` if any human listener is active (`humanListening == true`), or `"Notifications paused"` otherwise.
  - **Personal Receiver Card & Peer Title**: Default human listener titles (`""`, `"Human"`, `"@<user>"`, `"@<user> (Me)"`, or `"Human Node..."`) MUST project to `"You (@<user>)"` on local menu cards (paused subtitle: `"Paused · Click Enable to receive messages"`) and `"@<user> (You)"` / `"@<user>"` in Directory peer rows.
  - **Activity Target & Wake Banner Header**: In `Activity` rows (`act_title:<signal_id>`), signals targeting a human listener (`target_harness == "human"`) MUST display target `"You"` (`"@<from_user> → You"`). In `WakeBanner` (`panel`), human-targeted notifications MUST display header `"Cito · Direct Message"` (instead of `"Cito · Human"`), and `displaySubtitle` MUST NOT prepend a duplicate `"[Passive] "` text prefix when the top-right badge (`labels[4]`) already displays `"Passive"`.
- **Boilerplate `about` Suppression (`D02`, `F02`)**:
  - When the GUI `Listen` action (`listen_session`) enrolls an agent session without a user-supplied description, or when a node's `about` is empty, case-insensitively equal to `title` or `@<user>`, or matches a system placeholder (`"Session"`, `"Agent session"`, `"Coding session"`, `"Human"`, `"Human receiver"`, `"Human node"`, `"Human channel"`), the UI presentation layer MUST treat `about` as absent (`nil`).
  - Active listener cards with no distinct `about` MUST render as compact single-line `58pt` cards (`rowHeight = 64pt`). Directory peer rows MUST format line 2 as `"@<user>"` followed by `" · <host_or_channels>"` (never `"@<user> @<host>"`), appending `" · <about>"` only when a non-boilerplate description exists.
- **Human-Domain Network & Filter Terminology (`D03`, `D05`, `F11`)**:
  - **`local` Hub Row (`settings_hub:local`)**: Subtitle MUST read `"Built-in on this Mac · Signed in as @<user>"` (never exposing `"local://"` or `"Handle:"`). The Edit Local Hub form header MUST read `"This Mac (Local Hub)"` with description `"Set the username shown when you send or receive messages on this Mac."` and field label `"YOUR USERNAME (OPTIONAL)"`.
  - **Remote Hub Rows (`settings_hub:<name>`)**: Subtitle MUST strip the `http://` or `https://` scheme from the display label (retaining the full URL in `toolTip`), map `auth_mode` to `"Trusted Network"` (`"none"`), `"Shared Token"` (`"token"`), or `"Team Sign-In"` (`"oidc"`), and surface any connection/authentication error (`hub.error`) or non-connected status on the row. Only render `[Sign In…]` on OIDC hub rows when `enabled && (status == "auth_required" || status == "disconnected")` so connected OIDC hub rows retain the full `222pt` subtitle width.
  - **Directory Facets**: Rename `"My Hosts"` to `"My Macs"` (`dir_scope:mine`) and `"All Harnesses"` to `"All Apps"` (`dir_harness:all`).
  - **`peer_dto` & `find_listeners` Symmetry (`D05`)**: Both `snapshot` (`my_remote_listeners`) and `find_listeners` (`directory` peers) MUST serialize peers through the same DTO projection, exposing only active human-readable channel/host names in `channels` (never raw `<host_uuid>:<harness>` keys or `UNLISTENED` channel entries).
  - **Inspection & Discovery Failure vs. Confirmed Absence (`F11`)**:
    - If reading a plugin's config file fails due to malformed syntax (`setup_inspect` error) or a remote Hub's `human_channels_list` RPC fails, the daemon MUST surface the error state (`status: "error"`, `detail: <error>`) rather than collapsing it into `configured: false` / empty channels or triggering first-launch onboarding.
    - In `SessionActivity::reap_discovery` (`session_activity.rs:289-311`), if a plugin's `discover_sessions` invocation fails, times out, or panics, retain the last-confirmed candidate list for that plugin (revalidating on `listen_session`) rather than overwriting the cache with an empty vector as if zero sessions were open.
- **Single-Pass Machine Error Prefix Stripping (`D04`, `F10`)**:
  - `transport.rs` MUST NOT prepend `"{code}: "` a second time when the Hub's error message already begins with `"{code}:"`.
  - `registration.rs` CAS retry loops MUST retry only genuine CAS/revision conflicts and return transport, authentication (`UNAUTHORIZED`), or permission (`FORBIDDEN`) errors immediately without overwriting them as `STALE_GENERATION`.
  - Every UI error surface (`menuBannerError`, `settingsBannerError`, `addHubError`, `hostHubPortError`, `composerErrorLabel`, `channelResults`, and `drawer:<hub>`) MUST format error strings through a single presentation helper that strips leading `^([A-Z0-9_]+:\s*)+` machine prefixes while preserving the full raw string in diagnostic logs and JSON wire payloads.

#### 2. Principle 2 — Unified Readable Address Contract: `Display == Clipboard == Search == Send Target` (`D06`, `F15`)
- **Basis**: Eliminate the split brain where UI chips display an unroutable `#` string (`tefx#4219`) while copying an opaque hash (`tefx-4219b3232b2346a5`) that hides the target application.
- **Canonical Storage Invariance**:
  - Database primary keys, Hub KV keys, NATS/HTTP subjects, generation fencing, and acknowledgment keys MUST retain the canonical `listener_id` (`c_<16hex>` or legacy `c_<12base32>`) and `public_id` (`<slug>-<suffix>`).
- **Readable Display & Copy Address Format (`display_address`)**:
  - **Reserved `@self` Invariant**: `"self"` is a reserved self-reference token (`@self` always resolves to the caller's own human listener on the target Hub); `validate_user` (`config.rs:118`) MUST reject `"self"` (case-insensitively) as a custom username so `@<user>` never collides with `@self`.
  - **Hub Qualification Rule (1-to-1 with `send` Default-Hub Routing)**:
    - Omit the `<hub>:` prefix **if and only if** `node.hub == default_hub`;
    - Prepend `<hub>:` **if and only if** `node.hub != default_hub`.
    - Because `DaemonService::send` routes any unqualified address to `self.config.default_hub`, this rule guarantees that every displayed/copied `display_address` routes to the exact same Hub whether single-hub or multi-hub, regardless of connection state.
  - **Human Nodes (`harness == "human"`)**:
    - On `default_hub`: `@<user>` (e.g., `@tefx`);
    - On any non-default Hub (`hub != default_hub`): `<hub>:@<user>` (e.g., `work-hub:@tefx`).
  - **Agent Session Nodes (`harness != "human"`)**:
    - Format using the normalized owner slug (`<user>`), the short canonical `harness` ID (`codex`, `pi`, `claude`, etc.), and the node's short suffix (`<fp>`):
      - On `default_hub`: `<user>/<harness>#<fp>` (e.g., `tefx/codex#4219`, `tefx/pi#8f1a`);
      - On any non-default Hub (`hub != default_hub`): `<hub>:<user>/<harness>#<fp>` (e.g., `work-hub:tefx/pi#8f1a`).
    - **Suffix Derivation & Total Disambiguation**: `<fp>` defaults to the first 4 characters of the `listener_id` suffix after `c_` (applicable to both 16-char hex and legacy 12-char base32 suffixes). If two or more live listeners on the same Hub share the same `(user, harness, 4-char prefix)`, widen `<fp>` to the shortest unambiguous prefix (`8`, `12`, or `16` chars); if a 12-char legacy suffix is an exact prefix of a 16-char hex suffix on the same `(hub, user, harness)`, exact suffix-length match takes precedence when `<fp>.len() == 12`, or fall back to full canonical `public_id`.
- **Strict Display-Clipboard Identity & Protocol-Neutral Pairing Copy**:
  - Every address copy control across Menu Active Cards (`copy:<scoped_id>`), Menu `MY OTHER HOSTS` (`copy_remote:<scoped_id>`), and Directory Peer Rows (`copy_dir:<scoped_id>` and Return key) MUST display `display_address` as its button label AND copy that exact `display_address` string to the clipboard via a single atomic helper that updates both `self.clipboardString` and `appPasteboard` and triggers `copied("✓ Copied")`.
  - Channel pairing command copy (`copy_pair_command:<id>`) MUST read the explicit `pairing_command` field provided by the channel plugin (`format_pair` RPC in `plugins/telegram/native.rs` returning `"pairing_command": format!("/start {code}")` and `hub.rs` storing/exposing `pairing_command` in the channel DTO, falling back in `cito-ui.swift` to `channel.pairing_code ?? instruction` with zero `/start` regex in `cito-ui.swift`, `F15`) and MUST invoke the same atomic copy helper so `copied("✓ Copied")` visual feedback is shown.
  - In `DirectoryPanel` Composer (`composer_target`), the target pill MUST display `display_address` (capped at `width <= 200pt` so `composerInputField` retains `>= 194pt` width).
- **Backend Target Resolution, MCP Schema & Search Compatibility (`daemon.rs`, `identity.rs`, `hub.rs`)**:
  - Both `validate_tool_input("cito_send")` (`daemon.rs:2560-2580`) and `tools_list()` (`daemon.rs:2598` `cito_send.inputSchema.properties.to.pattern`) MUST accept:
    1. `@self` (resolving to the caller's own principal human listener on the target Hub) and `@<user>` / `<hub>:@<user>` (matching exact `owner_user` case-insensitively first, falling back to `normalize_slug(owner_user)` only when no exact match exists);
    2. `<user>/<harness>#<prefix>` and `<hub>:<user>/<harness>#<prefix>` (matching active listeners on the target Hub where `owner_user` or `normalize_slug(owner_user)` matches `<user>`, `harness == <harness>`, and the `listener_id` suffix after `c_` starts with `<prefix>`);
    3. Shorthand `<harness>#<prefix>` (scoped to the caller's own principal first, or unique active listener on the Hub) and `<user>#<prefix>` (matching any harness when unique);
    4. Full canonical IDs (`c_<16hex>`, `c_<12base32>`, `<slug>-<16hex>`, `<slug>-<12base32>`, with optional `<hub>:` prefix) for 100% backward compatibility.
  - **Live-Filtering & Same-Principal Ambiguity Contract (`daemon.rs:794-811`)**:
    - Apply the `is_live` filter (`ACTIVE` | `DISCONNECTED_RETAINED`) to all `<user>/<harness>#<prefix>`, `<harness>#<prefix>`, and `<user>#<prefix>` matches before ambiguity checking so expired historical sessions never shadow or collide with live sessions.
    - When `matches.len() > 1` for any non-`@handle` target (after checking exact 12-char suffix match when `prefix.len() == 12`), `send` MUST unconditionally return `Err("INVALID_INPUT: Ambiguous target; use full listener_id".into())` even when the matching agent sessions share the same `principal_id`.
  - **DTO & Response Compatibility**:
    - Emit `display_address` on `snapshot` listener rows and `peer_dto`, and emit `from_display_address` and `target_display_address` in `activity_dto` (`daemon.rs:1508-1529`) and `banner` payloads (`daemon.rs:1397-1406`) so `replyToActivity` and `openDirectoryForReply` pass `from_display_address ?? from_scoped_id` into `openComposer`.
    - Update synthetic test fixtures in `crates/cito-native/tests/support/ui.rs` to supply valid `display_address` fields on synthetic listener/peer frames rather than adding test-only non-hash fallback heuristics to production Swift code.
    - Keep `result["to"]` and `result["target_display"]` in `cito_send` / `send_reply_from_human` responses as `scoped_id` / `public_id` so existing wire assertions remain intact.
  - **Directory Search Parity**: Both `daemon.rs:find_listeners` and `cito-ui.swift:matchesDirectoryPeer` MUST match `display_address`, `<user>/<harness>#<prefix>`, `#<prefix>`, `is:<harness>`, and `harness:<harness>` symmetrically.
- **Sender Auto-Enrollment on `cito_send` & Human-to-Agent Reply Closure (`D18`, `daemon.rs`, `hub.rs`, `cito-ui.swift`)**:
  - **Root Cause**: When an Agent session (`codex`, `pi`, `claude`) invokes `cito_send` without having called `cito_listen` first, `self.state.listener(hub, id)` returns `None`, causing `daemon.rs:1170` to publish `"from": { "listener_id": null }` and leaving the sending session unregistered on the Hub. Consequently:
    1. In Cito Desktop `Activity` (`daemon.rs:1541`), `from_scoped_id` falls back to `@<from_user>` (the Human's own username when the Agent runs on the same user's Mac), so clicking `[ ↩ Reply ]` targets `@<user>` (the Human themselves) and fails with `SELF_SEND_FORBIDDEN: Cannot send to your own session`.
    2. In Telegram (`hub.rs:1203, 1335`), `hub_channel_deliveries.from_listener_id` is stored as `NULL`, so replying in Telegram fails with `Reply failed: LISTENER_NOT_FOUND: no recent sender to reply to`.
  - **Binding Requirements**:
    1. **Auto-Enroll Unenrolled Agent Sender on `cito_send` (`daemon.rs`)**: Whenever `cito_send` is invoked by a resolved non-human session (`caller` with non-empty `harness != "human"` and non-empty `session_id`) that has no existing row in `local_listeners` on `hub` (`local_node(state, slot, harness, session)? == None`, preserving explicit `UNLISTENED` or `FENCED_INACTIVE` states), `daemon.rs` MUST automatically register the caller session on `hub` (normalizing blank/oversized `caller["title"]` so `about` validation `1..500` chars always succeeds, adopting the committed `canonical_listener_id` / `public_id` returned by registration, and triggering non-deadlocking receiver synchronization) before publishing the signal.
    2. **Non-Null `from.listener_id` Invariant (`daemon.rs:1170`)**: Whenever `from_id` is `Some(id)`, `signal["from"]["listener_id"]` MUST be populated with `local_sender`'s `public_id` or `public_listener_id(owner_user, id)`—never `null`.
    3. **No `@<from_user>` Fallback for Non-Human Senders Across All Reply Paths (`daemon.rs:1541`, `cito-ui.swift:3997-4032`)**: When `row["from_listener_id"]` is non-empty, `from_scoped_id` is its scoped address (`from_display_address` / `scoped_id`). When `row["from_listener_id"]` is `null` or empty: if `row["from_harness"] == "human"`, fall back to `@<from_user>`; if `row["from_harness"] != "human"` (a legacy or unbound non-human sender with no listener ID), set `from_scoped_id = Value::Null` and `from_display_address = Value::Null`, hide `[Reply]`, and guard `replyToActivity`, Activity Return key, and `openDirectoryForReply` (falling back in `WakeBanner` click to `openActivityForSignal`) so no UI path ever falls back to `@<from_user>` when replying to an Agent.
    4. **Telegram / Human Channel 7-Day Reply Retention Independent of Signal Stream Pruning (`hub.rs:1335, 1485-1505`)**:
       - In `hub.rs:inbound_reply`, require `from_listener_id IS NOT NULL AND from_listener_id != ''` on both `hub_channel_delivery_msgs` and `hub_channel_deliveries`, and extend the fallback reply window on `hub_channel_deliveries` from 10 minutes (`600_000` ms) to 7 days (`604_800_000` ms).
       - In `hub.rs:prune()`, decouple `hub_channel_deliveries` and `hub_channel_delivery_msgs` retention from the 30-minute `hub_signals` payload stream: prune `hub_channel_deliveries` by `WHERE delivered_ms < now - 604_800_000` (7 days) and prune `hub_channel_delivery_msgs` where its parent delivery row no longer exists in `hub_channel_deliveries`.
       - In `hub.rs:publish_human_reply`, if the original `in_reply_to` signal has already aged out of `hub_signals`, still publish the reply signal to the active target listener and treat the source signal `ack` as an idempotent no-op.

#### 3. Principle 3 — State-Driven Progressive Disclosure & Layout/Scroll Geometry (`D07`–`D11`)
- **Basis**: Eliminate overlapping scrollbars, bottom-of-viewport card clipping, misleading controls in `Off` states, and modal void/click-through bugs.
- **Universal Overlay Scrollers (`D11`)**:
  - Every `NSScrollView` in `cito-ui.swift` (`active_scroll`, `my_remote_scroll`, `available_scroll`, `mountSettingsCards`, `directory_scroll`, and all horizontal pill strips) MUST set `scrollerStyle = .overlay`.
  - No `NSScrollView` with `frame.height < 34pt` may set `hasHorizontalScroller = true`.
- **Directory Facet Bar (`D07`)**:
  - `DaemonService::snapshot()` MUST only include agent plugins (`self.plugins.agent_plugins()`, or `kind == "agent"`) in `harnesses`, and `rebuildDirectoryFacetBar()` MUST filter out `"human"` and any non-agent entries via plugin kind/metadata without hardcoding concrete channel plugin IDs.
  - Layout in `directory_facet_bar` (`width: 560pt`): `All` (`x: 16, w: 38`), `My Macs` (`x: 58, w: 68`), `All Apps` (`x: 138, w: 68`), followed by agent pills starting at `x: 210` using compact labels when needed so all built-in agent pills fit within `x: 210..544` without horizontal scrolling or clipping. If custom agent plugins exceed `544pt`, use `hasHorizontalScroller = false` and call `scrollToVisible` on the active filter pill.
- **`Human Channels` Progressive Disclosure & Viewport Budget (`D08`)**:
  - When `channelHubs.count <= 1`, hide the `TARGET HUB:` selector strip (`36pt` saved). When `channelHubs.count > 1`, size `selectorContent` to `max(398, selectorX)` and scroll the selected Hub pill into view.
  - For channels without `scope == "hub"` fields (`hasHubScopeFields == false`), `gatewayActive` MUST be `true` so `"○ Gateway Token Required"` is never shown.
  - When Step 1 (`stratum_hub`) is not yet configured (`!gatewayActive`), Step 2 (`stratum_user`) MUST collapse to a `40pt` single-line prerequisite card (`"2. Your Personal Account (@<user>) — Complete Step 1 above to enable personal pairing."`), hiding disabled personal inputs and buttons until Step 1 is active.
  - Both the `!gatewayActive` state (Step 1 expanded + Step 2 `40pt` collapsed) and the `gatewayActive` state (Step 1 `36pt` collapsed + Step 2 expanded `<= 182pt`) MUST keep total `Telegram` card height `<= 272pt` so the entire `Human Channels` tab fits inside the `368pt` scroll viewport (`minY >= 8pt`, `extra == 0`) with zero bottom clipping even when `TARGET HUB:` is visible.
- **`Share Hub on Local Network` Off-State Collapse (`D09`)**:
  - When `host_hub.enabled == false` (`Off` and no pending port edit / error), `host_hub_card` MUST collapse to a `56pt` header card (showing only title, subtitle, and the `[Off]` toggle button), hiding the inactive endpoint URL, `Copy 1-Click Join Code`, `Port`, `Auth`, and `Apply` controls and shifting `CONNECTED HUBS` upward. When `enabled == true` (or when displaying a startup/port error), expand to the full `124pt` card.
- **Menu Multi-Hub Header Strip, Hub Switcher Pill & Candidate Rows (`D10`)**:
  - `header_hub_strip` MUST lay out Hub pills left-to-right and scroll the active/alerting Hub pill into `contentView.bounds`; when `hubs.count > 2` and total pill width exceeds `stripWidth`, compact pill widths so no visible pill is sliced mid-chip at the right border.
  - Active card Hub switcher pill (`switch_hub:<scoped_id>`) width cap MUST increase from `64pt` to `88pt` so standard Hub names (`work-hub`, `corp-oidc`) do not middle-truncate.
  - Candidate session rows (`candidate:<harness>:<sid>`) MUST render `c.title` (primary `.labelColor`, `.byTruncatingTail`) and `" · <shortCwd><branchSuffix>"` (secondary `CitoPalette.secondaryText`, `.byTruncatingMiddle`) as separate labels or attributed ranges so a truncated title never collides into `...·`.
- **Settings Add/Edit Hub & Modal Uninstall Overlay (`D11`)**:
  - Shorten `settings_hub_name` placeholder to `"e.g. work or studio"` so it fits inside the `146pt` field without truncation. Remove the hidden `.zero`-frame `NSTextField`s in `edit_hub:local` by preserving `"local"` and `"local://"` directly in `syncAddHubFieldsFromControls()` when `editingOriginalHub == "local"`.
  - When `uninstallConfirmOpen == true`, mount a single full-tab modal container (`identifier: "uninstall_modal_overlay"`, `frame: NSRect(x: 0, y: 0, width: 520, height: 412)`) containing the centered confirmation card (`width: 440, height: 128, x: 40, y: 142`), hide `settings_footer` and the underlying tab scroll view while the modal is open, and style `uninstall_purge_data` as a left-aligned checkbox control.

#### 4. Principle 4 — Subtractive Clarity & Full Data Lifecycle Closure (`D12`–`D14`, `F01`, `F06`–`F09`)
- **Basis**: Remove zero-value UI chrome that duplicates navigation, filter out non-interactive background subprocesses, and complete the `Activity` read/expand/clear lifecycle.
- **Remove Redundant `?` Guide Button & Bottom Callout (`D12`)**:
  - Delete `toggle_onboarding_guide` (`?` header button), `onboarding_banner`, and `apps_next_step_callout` from `settingsPanel`.
  - When zero Agent Apps are configured (`!resolvedApps.contains(where: { $0.configured })`), display a concise 1-line onboarding subtitle in `Agent Apps` (`"Step 1: Click Set Up on your coding app below, then click + Listen in the Cito menu bar."`) and keep the `No active listeners` menu empty-state card with direct `[Set Up Agent Apps…]` and `[Human Channels…]` buttons.
- **Activity Lifecycle: Filter-Before-Page Query, Multi-Line Expansion, Full-Message Copy, Held-Reason & Atomic Clear (`D13`, `F06`–`F09`)**:
  - **Filter-Before-Page Query & Atomic Clear (`state.rs`, `daemon.rs`, `F06`, `F07`)**:
    - `LocalState` and `DaemonService` MUST support filter-before-limit activity queries (`filter: "me" | "all" | "unread"`, `query: &str`) against `local_signals` before applying the page limit (`80` rows), returning accurate total/unread counts for the active filter so `"For Me"`, `"Unread"`, and search matches are never hidden behind 80 newer read agent-to-agent rows.
    - In `daemon.rs` (`"clear_activity_history"`), execute a single atomic SQLite transaction on `self.state.db` that captures the target rows (scoped to `action["hub"]` when non-empty, or all hubs otherwise), inserts unacknowledged rows into `pending_hub_acks` (`INSERT OR REPLACE INTO pending_hub_acks ...`), and deletes that exact captured row set from `local_signals` (avoiding reentrant lock acquisition and never deleting a concurrent newly admitted signal without acknowledging it), followed by `self.flush_acks()` and `self.push_ui()`.
    - Add a `[Clear]` button (`identifier: "activity_clear"`, `leadingSymbol: "trash"`, `frame: NSRect(x: 484, y: 5, width: 60, height: 22)`, with `✓ Read All` at `x: 390, width: 88`) in the `Activity` facet bar that emits `{"op": "clear_activity_history"}`.
  - **Selected-Row Multi-Line Expansion, Full-Message Copy & Held-Reason Badge (`cito-ui.swift`, `daemon.rs`, `F08`)**:
    - In `DirectoryPeerCellView.configureActivity`, always set `line2MetaLabel.toolTip = item.message`. When `item.held_reason` is non-empty, append a human-readable status note (`" · Held (session reloaded)"` for `"stale_generation"`, `" · Held (app unavailable)"` for `"driver_unavailable"`) to the header line.
    - Implement `tableView(_:heightOfRow:)` for `directoryTableView`: in `activity` mode, when `row == directorySelectedIndex` and `item.message` contains newlines or exceeds 1 line, expand the row height (`up to 108pt`, `maximumNumberOfLines = 4`, `.byWordWrapping`), anchoring top subviews (`badgeView`, `line1TitleLabel`, action buttons) relative to `rowHeight` (`rowHeight - 40`, `rowHeight - 26`, `rowHeight - 36`). In `DirectoryPeerCellView.configure(with: peer, ...)`, explicitly reset `badgeView.frame.origin.y = 12`, `line2MetaLabel.maximumNumberOfLines = 1`, and `line2MetaLabel.lineBreakMode = .byTruncatingTail` so reused cells never retain expanded Activity coordinates. In `tableViewSelectionDidChange`, when `directoryMode == "activity"`, reconfigure the previous and current row cells and call `table.noteHeightOfRows(withIndexesChanged:)`.
    - Provide a message copy button on Activity rows (`identifier: "copy_activity_msg:\(item.signal_id)"`). To reconcile full-message copy with the §5.2 `bounded_frame` 512-byte preview limit, `activity_dto` sets `message_truncated: true` when the stored `local_signals.message` exceeds 512 bytes:
      - When `message_truncated != true`, clicking `copy_activity_msg:<signal_id>` immediately writes `item.message` to the clipboard and triggers `copied("✓ Copied")`.
      - When `message_truncated == true`, clicking `copy_activity_msg:<signal_id>` MUST NOT write the truncated preview to the clipboard or report premature success; instead, it emits `{"op": "copy_activity_message", "request_id": <req_id>, "hub": item.hub, "signal_id": item.signal_id}` and commits the authoritative full message to the clipboard + triggers `copied("✓ Copied")` only when the correlated response arrives (ignoring stale completions if a newer clipboard action occurred, and surfacing an error without clobbering the clipboard if retrieval fails; single-signal response frames up to `262,144` bytes remain within the §5.2 outgoing frame budget).
    - Fix `directorySummaryLabel` pluralization (`"\(count) signal\(count == 1 ? "" : "s") · \(unreadCount) unread"`).
  - **Maximum-Length Wake Envelope Compatibility (`state.rs`, `F09`)**:
    - In `LocalState::claim_wake_batch_for` (`state.rs:180`), raise the single-batch JSON card ceiling from `262,144` bytes (`256 KiB`) to `524,288` bytes (`512 KiB`) so a valid maximum-length `65,536`-character Unicode/emoji message (`up to 262,144 UTF-8 bytes` plus sender header and JSON escaping) is never rejected before plugin invocation with a false `held_reason = "driver_unavailable"`.
- **Interactive-Only Pi Session Discovery Across Process & Socket Producers (`D14`, `F01`)**:
  - In `plugins/pi/native.rs` (`discover_sessions` across both `process_candidates` and `socket_candidates`), require interactive session eligibility: reject processes with no controlling TTY (`tty == "??"` or `"?"` in `ps -axo pid=,uid=,tty=,command=`, unless `CITO_PI_ALLOW_HEADLESS_DISCOVERY=1` is set in synthetic non-PTY test fixtures) and reject command lines invoking non-interactive Pi modes (`--mode rpc`, `--mode json`, `--print`, `-p`, `--no-session`, `--version`, `--help`) so background subagent/RPC worker processes never appear as open interactive sessions in `AVAILABLE SESSIONS`, while preserving headless caller resolution (`resolve_caller` / `verify`) for MCP tool calls.

#### 5. Principle 5 — Native AppKit Responder Hierarchy, Causal Correlation, Non-Blocking Snapshots & Calibrated 6-Invariant Oracle (`D15`–`D17`, `F03`–`F05`, `F12`–`F14`, `F16`)
- **Basis**: Guarantee keyboard/drag ergonomics regardless of text-field focus, preserve active text editors during background state updates, prevent stale/duplicate async actions, avoid blocking the daemon mutex on remote HTTP calls during snapshots, and eliminate all 5 `inspectMenu()` oracle blind spots.
- **Window-Level Keyboard Ownership, Background Drag & Editor Preservation (`D15`, `F12`, `F13`)**:
  - Set `window.isMovableByWindowBackground = true` on both `settingsPanel` and `directoryPanel`.
  - Register `⌘K` as a menu keyEquivalent and in the local `.keyDown` monitor (`start()`) to open `DirectoryPanel(mode: "directory")`.
  - Route `ESC` (`keyCode == 53`), `⌘W`, `↑` (`126`), `↓` (`125`), and `⏎` (`36`/`76`) at the panel/monitor level whenever `settingsVisible` or `directoryVisible` is true and no active field editor has marked IME text (`hasMarkedText() == true`):
    - **Settings `ESC` Hierarchy**: (1) if `uninstallConfirmOpen`, close confirmation modal; (2) else if `addHubFormOpen`, cancel Add/Edit Hub form; (3) else if `!editingHubGateway.isEmpty`, exit gateway edit mode; (4) else call `closeSettingsPanel(emitAction: true)`.
    - **Directory/Activity `ESC` & `⏎` Hierarchy**: `ESC` closes the Composer if open, else calls `closeDirectoryPanel(emitAction: true)`. When `directoryComposerBar` is visible (`!isHidden`), `⏎` is owned exclusively by the Composer: if the input text is empty/whitespace, set `composerErrorLabel` to `"Enter a message to send."` rather than falling through to copying a peer ID or opening another reply. When closing the Composer (`closeComposer()`), restore first responder to `directorySearchField`.
    - Headless `"test_settings_key"` and `"test_directory_key"` hooks MUST invoke the exact same `handleSettingsKey` / `handleDirectoryKey` methods used by the live key monitor.
  - **Editor Focus & Selection Preservation Across Background `state` Frames (`F13`)**:
    - In `receive()` (`cito-ui.swift:1335`), when a `state` frame arrives and `settingsChanged == false`, do NOT call `rebuildSettingsPanel()`.
    - When `rebuildSettingsPanel()` is called while a field in `settingsFieldControls` is the active first responder in `settingsPanel`, capture the focused field's `identifier` and `selectedRange()` before rebuilding, and restore first responder and selection range on the matching `NSTextField` after mounting the new content view.
    - In `closeSettingsPanel`, reset transient modal flags (`uninstallConfirmOpen = false`, `uninstallPurgeData = false`).
- **Directory Query Correlation, Composer Idempotency, Hub-Scoped Banner Coalescing & Non-Blocking Snapshots (`D16`, `F03`–`F05`, `F14`, `F16`)**:
  - **Directory Query Correlation & Counts (`F03`, `F04`)**: `query_directory` requests and `"directory"` response frames MUST carry `request_id`, `query`, `scope`, and `harness`, plus `matched_count` (pre-truncation match count) and global `online_count` (total active online nodes across hubs from cached directory state, so a 0-match search query never resets `directoryOnlineCount` to `0`). `cito-ui` MUST track the latest `directoryRequestId` (and current `(query, scope, harness)`) and ignore stale `"directory"` responses that do not match.
  - **Composer Logical-Send Idempotency (`F05`)**: `Companion` MUST generate a stable `composerDraftIdempotencyKey` (`UUID`) for each logical message draft and pass `"idempotency_key": composerDraftIdempotencyKey` (alongside per-attempt `"request_id"`) in `send_signal`, mapping directly to the daemon's existing `idempotency_key` reservation seam (`local_sent_idempotency`). Retrying the same draft after a transport error reuses `composerDraftIdempotencyKey`, whereas editing the message text, changing target, or completing/cancelling the send rotates `composerDraftIdempotencyKey`.
  - **Hub & Target Scoped Banner Coalescing (`F14`)**: `WakeBanner` coalescing in `receive()` (`frame.op == "banner"`) MUST require `previous.hub == value.hub && previous.target_title == value.target_title` in addition to `target_session_id`, `target_harness`, and `effective_wake`.
  - **Cache-Only Snapshot Construction (`F16`)**: `DaemonService::snapshot()` (called on every `push_ui()`) MUST read remote Hub channel inventories from an in-memory per-Hub cache on `DaemonService` (`cached_remote_channels`, invalidated on Hub removal, URL/auth replacement, or principal change) updated when `open_settings`, `settings_for_hub`, or channel mutations run—never performing a synchronous network `human_channels_list` RPC on every background `push_ui()` while holding the daemon mutex.
- **Surface 4 (`WakeBanner`) Headless Rendering & Calibrated 6-Invariant Mechanical Oracle (`D17`)**:
  - In `show(_ value: Payload)`, always construct `panel` and populate `labels[0..4]` before the `if headless` return (skipping only `window.orderFrontRegardless()` and screen animation in headless mode). Dynamically size `labels[4]` (`badgeW = max(76, ceil(measureTextWidth(semanticLabel, font: .boldSystemFont(ofSize: 10))) + 16)`, `frame: NSRect(x: 372 - badgeW, y: 103, width: badgeW, height: 20)`) and set `labels[0].frame.size.width = 372 - badgeW - 68`, using `.labelColor.withAppearanceAlpha(0.16)` for `ClickView` border.
  - Extend `renderPNG(surface:path:)` to support `surface == "banner"` and extend `inspectMenu()` to project `banner_items` (`panel?.contentView`, with `visible: self.visible`) when `panel != nil`.
  - `inspectMenu()` MUST compute and emit **6 mechanical invariant arrays** (all required to be `[]` across all test states in Light and Dark appearances):
    1. `truncated_labels`: checks all visible non-editable `NSTextField`s (removing the `stringValue.count <= 22` exemption on UI chrome labels; checking `MenuControl` labels and `dir_user:*` inside data rows; and checking `placeholderString` fit + minimum width `>= 48pt` on all visible editable `NSTextField`s);
    2. `overlapping_siblings`: checks pairwise sibling intersection across all surfaces (with `uninstall_modal_overlay` hiding sibling scroll/footer views) AND flags any `NSScrollView` where `(hasVerticalScroller || hasHorizontalScroller) && scrollerStyle != .overlay` or `hasHorizontalScroller && frame.height < 34`;
    3. `clipped_views`: checks child frames against parent bounds (`documentView.bounds` for scroll views), verifies that any active/selected pill in a horizontal pill strip (`header_hub_strip`, `targetStrip`, `selector`, `directory_harness_scroll`) is fully inside `scroll.contentView.bounds`, and verifies that default unscrolled Settings tabs (`Agent Apps` with 3 apps, `Human Channels` with 1 channel, `Hubs & Network` with `Share Hub: Off` and `<= 2` hubs) fit within `scroll.contentSize.height` (`extra == 0`) while allowing intentional scrolling when `> 3` items or expanded forms are present;
    4. `low_contrast_labels`: WCAG AA contrast `< 4.5:1` across `items`, `directory_items`, `settings_items`, and `banner_items` in both `light` (`aqua`) and `dark` (`darkAqua`);
    5. `jargon_leaks`: scans all visible non-editable **UI chrome** labels (`!label.isEditable`, excluding user-supplied data fields: session/peer `title`, `about`, `cwd`/`repo`/`branch` paths, and signal `message` body) across all 4 surfaces for forbidden raw tokens (`local://`, `Handle:`, `Human Node`, `Human listening`, `Human receiver`, `All Harnesses`, standalone `Session`, `· none`, `· token`, `· oidc`, `· auth_required`, `· disconnected`, `[Passive]`, and `^[A-Z0-9_]{3,}:`);
    6. `address_mismatches`: verifies that every visible node address copy control (`copy:*`, `copy_remote:*`, `copy_dir:*`) has a non-empty address label (using `accessibilityLabel()` when `feedbackTimer?.isValid == true` during the `"✓ Copied"` flash) matching the exact address copied to the clipboard.

---

#### 6. AppKit Standard Edit Menu & Responder-Chain Shortcuts (`D19`)

- `Companion` installs a standard `NSApp.mainMenu` with an **Edit** submenu: Undo (`⌘Z`), Redo (`⇧⌘Z`), Cut (`⌘X`), Copy (`⌘C`), Paste (`⌘V`), and Select All (`⌘A`). Menu actions use the AppKit responder chain.
- `KeyPanel.performKeyEquivalent(with:)`, the local key monitor, and `handleSettingsKey` / `handleDirectoryKey` route those shortcuts to the active native field editor across Settings, Directory search, and the Composer. All editable `NSTextField` and `NSSearchField` controls support clipboard, selection, Undo, and Redo. `NSSecureTextField` permits Paste, Select All, Undo, and Redo while blocking Copy and Cut to prevent secret extraction. Marked IME text retains native ownership.
- Synchronize field values with `validateEditing()` without detaching the active field editor. Assigning `stringValue` during editing can abort the editor and break later cut/undo operations. Paste and Cut own balanced undo gesture groups without directly closing AppKit's automatic event group; restore the prior event-grouping mode afterward.
- This is native editing infrastructure only: no provider-specific token extraction, token regular expressions, or onboarding automation belongs in this path.

#### 7. User Plugin Override Priority & Runtime Built-In Staging Separation (`D20`)

- Internal built-in plugin staging uses `${CITO_STATE_DIR}/runtime/plugins/<id>/` (default `~/.cito/runtime/plugins/<id>/`). Reserve `${CITO_STATE_DIR}/plugins/<id>/` exclusively for user-authored plugins and user overrides; setup, repair, and ordinary uninstall must not overwrite or remove those sources. Explicit data purge remains separate.
- The `v0.21.1` upgrade migration removes legacy Cito-staged copies only from `${CITO_STATE_DIR}/plugins/<id>/`. Identify built-in IDs through the bundled manifests, never a concrete plugin-name list. Remove a matching built-in when its command begins with `./bin/cito-` and its manifest version differs from `CITO_VERSION`; missing versions also differ. Skip stale copies even when cleanup is disabled or removal fails. Preserve custom commands (including scripts and Node entrypoints) at any version and preserve current-version overrides. Native configuration loading validates environment and configuration inputs before cleanup. Setup discovers plugins without mutation and only authorizes cleanup after a successful mutating operation; `--dry-run`, `--status`, and rejected setup commands leave legacy directories untouched. Canonical aliases of bundled sources are never recursively removed: retain a parent-root alias, or unlink an individual user plugin symlink without touching its target. Bundled discovery must still select the authoritative manifests after skipping external aliases.
- The internal `human` desktop companion is reserved for bundled built-ins. Ignore external candidates named `human` by directory or manifest ID in `CITO_PLUGIN_PATH`, `CITO_PLUGIN_DIR`, and the user plugin root; remove matching legacy user-root companion copies regardless of version or command. This exception applies to both native and standalone Hub discovery.
- Both `load_plugins` (`plugins.rs`) and `hub_plugins` select same-ID manifests with this priority, highest first:
  1. `CITO_PLUGIN_PATH` roots, left-to-right;
  2. `${CITO_STATE_DIR}/plugins/<id>/` user overrides;
  3. bundled defaults or staged runtime built-ins (`Cito.app/Contents/Resources/plugins/<id>/`, `~/.cito/runtime/plugins/<id>/`).
- A custom same-ID plugin replaces the built-in without a second active descriptor. Removing it restores the built-in automatically. Agent setup requests execute against the selected external source; built-ins use runtime staging and retain their bundled repair source. The standalone Hub also retains legacy `CITO_PLUGIN_DIR` precedence above its user directory and below `CITO_PLUGIN_PATH`.
- Installed CLI shims export the runtime built-in root. Missing staged descriptors remain recoverable from the distribution, and App bundle assembly retains the bundled defaults even while a user override is active.

### 14.2 Implementation Notes & Hazard Map (`ADVISORY`)

- **Hazard 1 (`D06` Chip Width on `360pt` Menu Cards)**: Always use the short `harness` ID (`codex`, `pi`, `claude`) in `<user>/<harness>#<fp4>` and omit `<hub>:` when the node is on the default/single Hub so copy chips remain `<= 135pt` and leave `>= 160pt` for workspace paths on `360pt` menu cards.
- **Hazard 2 (`D13` Unflipped `DirectoryPeerCellView` Coordinates)**: Because `DirectoryPeerCellView.isFlipped == false`, when `tableView(_:heightOfRow:)` returns an expanded height (`up to 108pt`) for the selected Activity row, `configureActivity` (and `configure` when reusing a cell for a `52pt` Directory row) must set `badgeView.frame.origin.y = rowHeight - 40`, `line1TitleLabel.frame.origin.y = rowHeight - 26`, `msgControl.frame.origin.y = rowHeight - 36`, and `copyControl.frame.origin.y = rowHeight - 36`. Use identifier `"copy_activity_msg:\(item.signal_id)"` for the Activity message copy button so `address_mismatches` only inspects node address chips (`copy:*`, `copy_remote:*`, `copy_dir:*`).
- **Snapshot acceptance (`D17`)**: Canonical Light/Dark Retina captures require all six mechanical arrays to be empty before and after rendering. The separate extreme-length stress test intentionally exceeds fixed label widths: it requires nonempty `truncated_labels`, five other empty arrays, and full cleaned text/tooltips. It must not export PNGs into the canonical snapshot set. The strict chrome truncation check remains unchanged.
- **Full-message copy framing (`F08`)**: A maximum Unicode message plus its JSON envelope can exceed one outgoing frame. The implementation returns ordered `activity_message` chunks (`part`, `complete`, and the original `request_id`), each below the outgoing byte budget. The Companion assembles the authoritative message and changes the clipboard only after the final correlated chunk.
- **Hazard 3 (`D06` Backward Compatibility in Existing Tests)**: Keep full `listener_id` (`c_<16hex>`) and `public_id` (`<slug>-<16hex>`) in all JSON fields (`listener_id`, `public_id`, `scoped_id`) and add `"display_address"` alongside them; update synthetic test fixtures in `crates/cito-native/tests/support/ui.rs` to include `display_address` (or canonical hash suffixes) so production Swift code has a single clean `display_address` authority without test-only shims.

---

## 15. V31 — Stage 13 v0.21.2: Declarative Bar/Strip Layout Engine, Single-Authority Human/Channel Lifecycle & Bounded Delivery Retry Architecture (`M1`–`M5`)

Stage 13 replaces ad-hoc coordinate patching and fragmented backend state/retry loops with five orthogonal mechanisms (`M1`–`M5`), governed by strict test economy (updating existing tests in place at the lightest hermetic boundary without adding slow multi-process test bloat).

### 15.1 Binding Contract (`MUST`)

#### 1. `M1` — Declarative Horizontal Bar & Pill Strip Layout Engine (`src/macos/cito-ui.swift`)
- **Basis**: Eliminate hand-authored `NSRect(x: ...)` coordinate cascades and left-pinned pill strips that strand controls in the middle of headers, cards, or footers when conditional items hide or labels resize.
- **Declarative Bar Layout Primitive (`layoutBar`)**:
  - Introduce a single horizontal bar layout primitive (`layoutBar(in:insetLeft:insetRight:y:height:spacing:leading:flexible:trailing:)`) in `src/macos/cito-ui.swift` that:
    1. Packs visible `leading` items left-to-right starting at `insetLeft`;
    2. Packs visible `trailing` items right-to-left starting at `container.bounds.width - insetRight`;
    3. Stretches the optional `flexible` primary/secondary label span across the remaining `[leadingMaxX, trailingMinX]` interval.
  - All horizontal bars and action rows—including `cardMenu` `footer`, `drawer:<hub>`, `session:<id>` card header/meta row, `remote:<id>` row, `candidate:<h>:<s>` row, `settings_app:<id>` header, `stratum_hub:<id>` header/footer, `stratum_user:<id>` action row, `settings_hub:<name>` row, `settings_footer`, `directory_search_bar` (dynamically sizing `actPill` width when `"Activity · N"` changes), and `directory_footer`—MUST compute their horizontal positions via `layoutBar` (or equivalent edge-packing helper) rather than hardcoded conditional `x` ternary tables.
- **Declarative Pill Strip Primitive (`mountPillStrip`)**:
  - Introduce a single scrollable pill-strip primitive (`mountPillStrip(in:frame:id:alignment:spacing:pills:activePredicate:)`) used by all horizontal pill strips (`header_hub_strip`, `targetStrip`, `channel_hub_selector`, and `directory_harness_scroll`).
  - When `alignment == .trailing` and total pill width `< frame.width` (such as `header_hub_strip` and `targetStrip`), `mountPillStrip` MUST start at `cursorX = frame.width - totalWidth` so the rightmost pill is always flush with `frame.maxX` (`x = 368` in `cardMenu`), never stranded at `x = 138` in the middle of the header.
  - When total pill width `> frame.width`, `mountPillStrip` uses `OverlayScrollView` (`scrollerStyle = .overlay`, `hasHorizontalScroller = false`) and scrolls the active pill into view.
  - In `directory_facet_bar`, render a `1 × 14 pt` vertical separator (`x = 132, y = 9`, hidden in Activity mode) between the Scope pills (`All`, `My Macs`) and App pills (`All Apps`, ...), and give `activity_clear` the same capsule radius (`11`) and `.medium` font as `activity_mark_all_read`.
  - In `Human Channels`, place `Mac Desktop Banner` (global to this Mac) at the top of the tab above `TARGET HUB:` (which scopes per-Hub channel cards below it).

#### 2. `M2` — Default Single-Mac & Idle State Affordance Invariants (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`)
- **Basis**: Never expose controls in the default single-Mac (`local`) or `Idle` (`uniqueSessionCount == 0`) states that contradict the header status or disconnect/disable the user's own local Mac.
- **Single Healthy `local` Hub Header Suppression**:
  - When `hubs.count == 1 && (hubs[0].name == "local" || hubs[0].name == "default") && hubs[0].auth_mode == "none" && hubs[0].status == "connected" && hubs[0].error == nil`, `cardMenu` MUST NOT render `header_hub_strip` (`hub:local` `[ ● 🌐 ⌄ ]`) or `drawer:local` (`[Disconnect]`). `header_hub_strip` and hub drawers appear only when `hubs.count > 1` or when the single hub is remote, authenticated (`auth_mode != "none"`), connecting/disconnected, or has `error != nil`.
- **Agent-Only `Stop All` Gating (`uniqueSessionCount > 0`)**:
  - In `cardMenu` `footer`, `stop_all` (`[ ■ Stop All ]`) MUST be shown if and only if `uniqueSessionCount > 0` (when at least one coding agent session is listening). When `uniqueSessionCount == 0`, `stop_all` is hidden and `layoutBar` anchors `Settings…` at `x = 12` and `Quit Cito ⌘Q` at `maxX = 368`.
  - In `DaemonService::action_inner("unlisten_all")` (`daemon.rs`), `Stop All` MUST unlisten only non-human agent sessions (`row["harness"] != "human"`), preserving the user's `You (@<user>)` personal notification state. (Full `setup_disconnect_all` and `setup_uninstall_all` continue to unlisten all rows including `human`.)
- **Personal Card (`You (@<user>)`) Affordances**:
  - On active `harness == "human"` cards in `cardMenu`, replace the destructive-red `"xmark"` (`"Stop listening"`) button with a neutral `"Pause"` control (`identifier = "stop:\(listener.scoped_id)"`, `toolTip = "Pause personal notifications"`), omit `switch_hub` on `human` cards, and display active channel names (`chs.joined(separator: " · ")` or `"Desktop notifications"`, never raw filesystem `cwd` like `~` or `/tmp`) in the metadata label.
- **Sole Enabled `local` Hub Protection in `Hubs & Network`**:
  - On `settings_hub:local`, when `local` is the only enabled Hub (`resolvedHubs.filter { $0.enabled }.count <= 1 && hub.enabled`), hide the `[Enabled]` disable toggle (`toggle_hub:local`) so `layoutBar` right-aligns `[Edit]` at `maxX = 468`. In `add_hub_form`, label `submit_add_hub` as `"Save"` when `editingExistingHub && editingOriginalHub == "local"`.
  - In `DaemonService::settings_for_hub` (`daemon.rs:1803`), resolve `user` via `self.config.effective_user(name)` for disabled hubs just like active hubs.

#### 3. `M3` — Complete Terminology Sweep, Monochrome Status Bar & Calibrated Layout/Jargon Oracles (`src/macos/cito-ui.swift`, `plugins/telegram/plugin.json`)
- **Basis**: Close all sibling branches missed in earlier terminology sweeps, comply with macOS HIG monochrome status bar rules, and upgrade `inspectMenu()` to catch unanchored horizontal bars and emoji/jargon regressions mechanically.
- **Sibling Terminology Closure**:
  - `add_hub_form`: use `"YOUR USERNAME (OPTIONAL)"` on all auth branches (`none`, `token`, and `local`), `"ACCESS"` in uppercase, and `"Hub URL must start with http:// or https://."` in client URL validation.
  - `Human Channels` & `plugins/telegram/plugin.json`: use consistent Title Case headers (`"1. Shared Bot Gateway"` and `"2. Your Personal Account (@<user>)"`), status `"● Bot Ready"`, `"everyone on this Hub"` (instead of `"all human nodes"` / `"every human"`), `"receive notifications and reply"` (instead of `"receive wakes"`), `"Show floating banners on this Mac when notifications arrive."`, `"Paused · Click Resume to receive notifications"`, and omit `"on 'local'"` when `channelHubs.count <= 1`.
  - Empty states & Composer: replace `"Call cito_listen in a session..."` with `"Open a coding session in your app or configure Human Channels"`, `"Message via Hub…"` with `"Write a message…"`, `"UI action exceeds 1 MiB"` with `"Message is too large to send"`, `"No matching nodes"` / `"No active nodes on this Hub"` with `"No matching sessions or people"` / `"No active listeners"`, and `"Channel inventory"` with `"Human Channels"`.
- **Monochrome macOS Status Bar (`formatStatusButton`) & `WakeBanner` Pill Centering**:
  - `formatStatusButton` MUST NOT use the color emoji `🔔` (`U+1F514`). Render unread counts in the status bar using a monochrome template SF Symbol attachment (`bell.fill` with `isTemplate = true`) or plain monospaced digits.
  - In `WakeBanner`, center `labels[4]` (`"Notification"` / `"Passive"`) vertically inside a `20pt` `SurfaceView` pill container (`y: 3, height: 14`).
- **Upgraded `inspectMenu()` Mechanical Oracles**:
  - Expand `jargon_leaks` regex on visible non-editable UI chrome labels to include `(?i)(\\bhandle\\b|human nodes?|every human|each human|\\breceive wakes\\b|\\bwake banners?\\b|\\bcito_listen\\b|\\bhub service:|\\bui action exceeds\\b)`, plus `"on 'local'"` when `hubs.count <= 1`, and flag any non-template color emoji scalar (`value > 0x27BF && isEmoji`) in `status?.button`.
  - In `clipped_views` (or `overlapping_siblings`), verify horizontal edge-anchoring: for `header_hub_strip` and `targetStrip` with visible children, require `maxChildX >= scroll.contentSize.width - 2.0`; for `footer`, `settings_footer`, `directory_footer`, `stratum_hub:*`, `stratum_user:*`, `settings_hub:*`, and `settings_app:*`, require the rightmost visible top-row item to reach `maxX >= parentBounds.width - 20.5`.

#### 4. `M4` — Single-Authority Human/Channel Lifecycle & Lease Reconciliation (`crates/cito-native/src/core/{lease.rs,registration.rs,receiver.rs,hub.rs,daemon.rs}`)
- **Basis**: Eliminate the three-way state split-brain across `local_listeners`, `hub_kv` (`state` + `channels[*]`), and `hub_human_channels` that allowed Telegram to keep receiving messages when `You (@<user>)` was paused (`UNLISTENED`) in the menu bar and caused `receiver.rs`, `hub.rs`, and `lease.rs` to compute divergent node `state` values.
- **Unified Registration State & Lease Authority (`core/lease.rs`)**:
  - Extract a single authoritative function `pub fn reconcile_registration_state(reg: &mut Value, now: u64)` in `core/lease.rs` and call it from `registration::register_locked`, `receiver::detach_channel_expected`, and `hub::attach`:
    - Mark any non-`hub` channel in `reg["channels"]` whose `now.saturating_sub(updated_at_ms) > 1_800_000` as `"UNLISTENED"`.
    - Set `reg["state"] = "ACTIVE"` if and only if `!reg["human_paused"].as_bool().unwrap_or(false)` and at least one channel in `reg["channels"]` has `state == "ACTIVE"` (with `host_id == "hub"` or `now.saturating_sub(updated_at_ms) <= 1_800_000`); otherwise set `reg["state"] = "UNLISTENED"`.
- **Master Human Node Pause/Enable vs. Individual Channel Pause**:
  - Pausing `You (@<user>)` in `cardMenu` (`unlisten` on `harness == "human"`) represents an explicit Hub-wide pause of the user's Human Node: `detach_channel_expected` (when `channel.ends_with(":human")`) sets `reg["human_paused"] = json!(true)` and `reconcile_registration_state` sets `reg["state"] = json!("UNLISTENED")` (even if another host or `hub:<plugin>` channel exists in `reg["channels"]`), while preserving `hub_human_channels` credentials/bindings. `hub::attach` and `restore_attachments` MUST preserve `reg["human_paused"]` so background channel reconciliation never silently resumes a paused Human Node.
  - Re-enabling `You (@<user>)` (`register_listener` for `harness == "human"`) clears `reg["human_paused"] = json!(false)` and preserves any existing `hub:<plugin>` entries in `reg["channels"]` (`registration.rs:212`).
  - `HubEngine::replay()` (`hub.rs:1199`) MUST check that the principal's Human Node in `hub_kv` is not paused (`reg["human_paused"] != true` and `reg["state"] != "UNLISTENED"` when a `hub_kv` row exists) before dispatching new channel deliveries to Telegram!
  - Conversely, turning `Mac Desktop Banner` to `Off` only suppresses the floating HUD banner on this Mac while keeping `You (@<user>)`, `Activity`, and `Telegram` active.
- **Human Channel Delivery Acknowledgment Reconciliation**:
  - Pausing `You (@<user>)` preserves unread signals and never implicitly marks them acknowledged (`acked_at_ms` remains `NULL`). Only an explicit consumer action—replying via `inbound_reply` / `send_reply_from_human` or acknowledging on Desktop (`ack_signals` / `clear_activity_history`)—updates `hub_signals.acked_at_ms`, and `local_signals` reconciles `acked_at_ms` when the Mac listener polls.

#### 5. `M5` — External I/O Timeout Separation, `EFFECT_UNKNOWN` Classification, Multipart Progress Checkpointing & Bounded Exponential-Backoff Delivery State Machine (`plugins/telegram/native.rs`, `crates/cito-native/src/core/{hub.rs,transport.rs,auth.rs,login.rs,state.rs,receiver.rs}`)
- **Basis**: Eliminate duplicate Telegram messages caused by post-send read timeouts + immediate 250 ms retries (`T1`), multipart partial-send re-execution, 500 ms SSE reconnect loops in `transport.rs`, and permanently stranded `driver_unavailable` agent signals.
- **Persistent `ureq::Agent` with Separated Connect/Read Timeouts, `EFFECT_UNKNOWN` Classification & Multipart Progress (`plugins/telegram/native.rs`)**:
  - Replace ephemeral `ureq::post(...)` calls in `plugins/telegram/native.rs` with a persistent `ureq::Agent` configured via `ureq::AgentBuilder::new().redirects(0).timeout_connect(Duration::from_secs(3)).timeout_read(Duration::from_secs(25))` (`35s` for `getUpdates`).
  - Partition `sendMessage` failures into three distinct outcomes on the returned `delivered` frame:
    1. **Confirmed Retryable Rejection (`retryable: true, effect_unknown: false`)**: HTTP status `408 | 429 | 500..=599` or pre-transmit connection failure (`io::ErrorKind::ConnectionRefused | AddrNotAvailable` / connect error before request body write);
    2. **Permanent Rejection (`retryable: false, effect_unknown: false`)**: HTTP status `400 | 401 | 403 | 404` (extracting Telegram's JSON `"description"` field);
    3. **Effect Unknown (`effect_unknown: true, retryable: false`)**: post-transmit socket read `TimedOut` / `UnexpectedEof` after `sendMessage` request bytes were written, where Telegram may have already accepted the message.
  - In `delivery()`, accept `already_sent_ids: Vec<String>` (resuming at `chars.chunks(4096).skip(already_sent_ids.len())`) and always return the confirmed prefix `external_msg_ids` alongside `ok`, `retryable`, and `effect_unknown`.
- **Bounded Exponential-Backoff Delivery State Machine in `HubEngine` (`core/hub.rs`)**:
  - Add `attempts INTEGER NOT NULL DEFAULT 0`, `next_retry_ms INTEGER NOT NULL DEFAULT 0`, and `last_error TEXT` to `hub_channel_deliveries` via the additive `PRAGMA table_info` migration in `HubEngine::open`.
  - Both subprocess crash recovery on `in_flight` rows (`ensure_process`) and `delivery_result()` frames with `f["effect_unknown"] == true` transition the row to `status = 'effect_unknown'` (recording any confirmed prefix in `hub_channel_delivery_msgs`) so `replay()` never blindly re-sends a chunk whose effect is unknown:
    - When `ok == false` and `effect_unknown == true`: set `status = 'effect_unknown'` and `last_error = error`.
    - When `ok == false` and `effect_unknown != true` and `retryable != false` and `attempts < 5`: set `status = 'failed'`, `next_retry_ms = now_ms() + (2_000_i64 << attempts.min(5)).min(60_000)` (`2s, 4s, 8s, 16s, 32s, 60s`), and `last_error = error`.
    - When `ok == false` and `effect_unknown != true` and (`retryable == false` or `attempts >= 5`): set `status = 'failed_permanent'` and `last_error = error`.
  - In `HubEngine::replay()`, exclude rows where `d.status IN ('delivered', 'failed_permanent', 'effect_unknown') OR (d.status = 'in_flight' AND d.delivered_ms > now - 60_000) OR (d.status = 'failed' AND d.next_retry_ms > now)`.
  - Reset `failed` / `failed_permanent` rows (`attempts = 0, next_retry_ms = 0`, preserving `hub_channel_delivery_msgs` confirmed chunks) when the user updates channel credentials in `human_channels_configure`, and surface `last_error` in `channel_list()` when the latest delivery for that binding failed after `binding.updated_ms`.
- **Unified `ureq` Client Hygiene & SSE Read-Idle Timeout Fix (`core/transport.rs`, `core/auth.rs`)**:
  - Configure `HubClient::new` with `redirects(0)` and `timeout_connect(Duration::from_secs(5))`, and bound error-response JSON reads with `.take(...)`.
  - In `HubClient::start_events` (`transport.rs:274`), use `client.agent.get(&url)` (configured on an SSE agent with `timeout_connect(5s)` and `timeout_read(Duration::from_millis(2500))`, NOT overall request `.timeout(...)`) and `.take(65_536)` per line so `2500ms` acts strictly as a per-read idle timeout (> `1000ms` server `:keepalive` interval) and healthy SSE streams stay connected indefinitely.
- **Hub Instance Scope for Sent Idempotency (`core/daemon.rs`), Transient Agent Wake Recovery (`core/state.rs`) & Stable Lockfile Inodes (`plugin_support.rs`)**:
  - Include the Hub's normalized target URL (`slot.config["url"]`) in the `dedup` hash input in `DaemonService::send` (`daemon.rs:1435`: `hash(&format!("{endpoint}|{}|{canonical}|{key}", string(&slot.config, "url")))`) so editing a Hub's URL under the same name never collides with sent idempotency records from the old URL.
  - In `LocalState::restore_passive` (`state.rs:293`), allowunread signals held with transient `held_reason = 'driver_unavailable'` on an `ACTIVE` listener to be restored to `'RECEIVED'` after the wake retry backoff (`wake_retries`) elapses (while keeping `stale_generation`, `session_switched`, and `unlistened` held).
  - In `plugin_support.rs:mutate` (`line 233`) and `clean_sidecars` (`line 125`), never unlink `.cito.lock` (keep lockfile inodes permanent across both ordinary updates and `setup_remove`, cleaning only `.bak.*` and `.tmp-*` sidecars, matching `core/login.rs:FileLock`).

### 15.2 Verification & Anti-Over-Testing Policy (`MUST`)

- **Strict Test Economy (No Test Bloat)**:
  - Do **NOT** create new multi-process/daemon E2E test functions for individual UI coordinate or label tweaks; update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs}` and `crates/cito-native/src/core/*_tests.rs` in place.
  - Keep `M1`–`M3` UI layout, bar reflow, single-Mac `local` header/footer, monochrome status button, and `inspectMenu()` checks inside the existing headless UI test pass in `ui.rs` (and only write PNG files to disk when `CITO_TEST_SNAPSHOT_DIR` is set or updating the 7 `site/assets/screenshots/*.png` assets).
  - Verify `M4`–`M5` backend state transitions (`reconcile_registration_state`, `human_paused` gating `replay()`, `hub_channel_deliveries` exponential backoff & `failed_permanent`, and URL-scoped `dedup`) with fast, deterministic unit/in-memory tests (zero multi-second `sleep` calls).

---

## 16. Stage 14 (`v0.21.3`) Unified Spatial Grid (`CitoMetrics`), Real App Icons (`icon.png`), and `Hubs & Network` Tab Redesign (`N1`–`N3`)

### 16.1 Binding Contract (`MUST`)

#### 1. `N1` — Unified Spatial Grid (`CitoMetrics`), Card Padding Symmetry & Mechanical `inspectMenu()` Padding Gate (`src/macos/cito-ui.swift`)
- **Basis**: Eliminate inconsistent outer keylines (`10pt` vs `12pt` in `cardMenu`, `4pt` vs `6pt` tab gaps and `x = 486` close button in `settings_header`) and zero/near-zero vertical card padding (`0pt` top padding on `toggle_host_hub`, `2pt` top padding on `host_hub_card` title, `3pt` top padding on `settings_hub:<name>` buttons, `5pt` top padding on `chCard` title, `0pt` error-to-button gap in `add_hub_form`).
- **Spatial Token Grid (`CitoMetrics`) & Outer Keyline Alignment**:
  - **Menu (`cardMenu`, `width: 380pt`)**: All rows (`header`, `human_paused_card`, `session:<id>`, `empty`, section headers, `directory_row`, `footer`) MUST share a uniform **`12pt` outer horizontal keyline (`x = 12..368`, `width = 356pt`)**. Increase `human_paused_card` from `46pt` (in a `52pt` row) to **`50pt`** (in a `56pt` row) with `title` at `y = 26, height = 16` (`8pt` top padding), `subtitle` at `y = 8, height = 14` (`8pt` bottom padding), and `Resume` at `y = 14, height = 22` (`14pt` top/bottom padding).
  - **Settings (`settingsPanel`, `width: 520pt`)**:
    - In `settings_header`, lay out the three tab pills with uniform `6pt` horizontal spacing (`hubsTab` at `x = 336`) and align `closeBtn` (`24 × 24`) to `x = 476` (`maxX = 500`, flush with the `x = 20..500` card rail below).
    - Standardize inner horizontal padding across all bordered Settings cards (`settings_app:<id>`, `bannerCard`, `chCard`, `host_hub_card`, `settings_hub:<name>`, `add_hub_form`, `uninstall_confirm_box`) so every visible child satisfies `minX >= 8pt` and `maxX <= card.width - 8pt` (with standard `12..14pt` inset).
- **Card Vertical Padding Symmetry Across All Tabs**:
  - **`Settings -> Hubs & Network -> host_hub_card` (`width: 480pt`, `height: 56pt` collapsed / `136pt` expanded, `y = hostExpanded ? 220 : 300`)**:
    - Anchor the top header band relative to `H = hostCard.bounds.height` so collapsed and expanded states share identical top padding without post-hoc `-68` coordinate shifting:
      - `title` (`"Share Hub on Local Network"`, `13pt .semibold`): `NSRect(x: 14, y: H - 27, width: 350, height: 18)` (`9pt` top inset).
      - `subtitle` / `host_hub_error` (`11pt .regular`, `height: 15`): `NSRect(x: 14, y: H - 46, width: 350, height: 15)` (`10pt` bottom inset when collapsed at `H = 56`). When `hostHubPortError ?? hostHub.error` is non-empty, render `host_hub_error` in this subtitle slot in `CitoPalette.dangerText` (preventing any overlap with expanded rows).
      - `toggle_host_hub` (`width: 80, height: 24`): placed via `layoutBar(in: hostCard, insetLeft: 14, insetRight: 14, y: H - 40, height: 24, trailing: [toggleHostBtn])` (**`16pt` top inset** in both collapsed `H = 56` and expanded `H = 136` states).
    - When `hostExpanded == true` (`H = 136`):
      - Row 1 (Endpoint & Invite bar): `layoutBar(in: hostCard, insetLeft: 14, insetRight: 14, y: 48, height: 24, flexible: endpointLabel, trailing: (showToken ? [tokenBtn] : []) + [copyInviteBtn])`.
      - Row 2 (Port & Access bar): `layoutBar(in: hostCard, insetLeft: 14, insetRight: 14, y: 12, height: 24, leading: [portLabel, portField, applyBtn], trailing: [authBtn])` (`12pt` bottom inset).
  - **`Settings -> Hubs & Network -> settings_hub:<name>` Rows**:
    - Increase base 2-line `rowHeight` from `48pt` to **`56pt`** (`max(56, subtitleHeight + 42)` so wrapped subtitles at `y = 9..9+subtitleHeight` always keep `>= 2pt` separation below the top bar at `y = rowHeight - 31`):
      - Status dot (`8 × 8 pt`) at `NSRect(x: 14, y: rowHeight - 24, width: 8, height: 8)`.
      - Top bar (`nameLabel` + trailing action buttons, `height: 22`) via `layoutBar(in: row, insetLeft: 28, insetRight: 12, y: rowHeight - 31, height: 22, flexible: nameLabel, trailing: signIn + [edit, toggleBtn] + remove)` (**`9pt` top inset**).
      - `subtitleLabel` at `NSRect(x: 28, y: 9, width: subtitleWidth, height: subtitleHeight)` (**`9pt` bottom inset**).
  - **`Settings -> Hubs & Network -> add_hub_form`**:
    - Set `height = 156pt` for `local` username edit and `height = 204pt` for remote Hub add/edit (`12pt` top inset for Row 1 labels, `10pt` bottom inset for action buttons at `y = 10, height = 24`, and `add_hub_error` at `y = 38, height = 15` so error text never touches buttons or inputs).
  - **`Settings -> Human Channels` (`bannerCard`, `TARGET HUB:`, `chCard` Header & Strata)**:
    - Give `bannerCard` `height = 54pt` at `y = 304` (`title` at `y = 29, height = 17` with `8pt` top inset; `subtitle` at `y = 9, height = 15` with `9pt` bottom inset; `toggleBanner` at `y = 15, height = 24` with `15pt` top/bottom inset).
    - When `channelHubs.count > 1`, place `TARGET HUB:` strip at `y = 270, height = 26` (`8pt` below `bannerCard`) and start `chCursorY = 262` (`8pt` below strip; `296` when `channelHubs.count == 1`).
    - Increase `chCard` header band from `42pt` to **`48pt`** (`chBadge` `24 × 24` at `y = cardHeight - 34`; `chTitle` + `chStatusLabel` via `layoutBar` at `y = cardHeight - 26, height = 16` with **`10pt` top inset**; `chDetail` at `y = cardHeight - 42, height = 14` with `6pt` gap above `hubStratum`). Maintain `>= 8pt` top/bottom padding inside `hubStratum` and `userStratum` while keeping total `chCard` height `<= 254pt` in the default 2-hub / 1-channel state so `settings_channels_tab` remains scroll-free in the `368pt` viewport.
- **Mechanical `inspectMenu()` Card Inner-Padding Oracle**:
  - In `inspectMenu()`, for every visible bordered card/stratum container (`view is SurfaceView && (view.layer?.borderWidth ?? 0) > 0 && view.frame.width >= 60 && view.frame.height >= 32`, excluding `34 × 34` icon badges):
    - Verify that every visible child (`!child.isHidden && !(child is NSVisualEffectView)`) has **`minY >= 5.5` and `maxY <= parentBounds.height - 5.5` and `minX >= 5.5` and `maxX <= parentBounds.width - 5.5`**.
    - Append `"\(name(view)) > \(name(child)): card padding < 6pt"` to `clipped_views` on any violation, and add `"card_padding"` to `test_oracle_calibration`.

#### 2. `N2` — Real Bundled Brand Icons (`plugins/{codex,pi,claude,telegram}/icon.png`) with Vendor-Neutral Core Fallback (`plugins/*/icon.png`, `src/macos/cito-ui.swift`)
- **Basis**: Replace generic tinted SF Symbol placeholders for built-in Agent Apps (`codex`, `pi`, `claude`) and Human Channels (`telegram`) with crisp `128 × 128` RGBA brand icons while keeping Core (`crates/cito-native/src/core/` and `cito-ui.swift`) vendor-neutral.
- **Plugin-Scoped Icon Assets & Resolution (`pluginIconImage(for id: String) -> NSImage?`)**:
  - Ship `plugins/codex/icon.png`, `plugins/pi/icon.png`, `plugins/claude/icon.png`, and `plugins/telegram/icon.png` in their respective plugin directories (staged automatically by `setup.rs` and `packaging/release.py`).
  - In `cito-ui.swift`, resolve `<id>/icon.png` across `CITO_PLUGIN_PATH`, `${CITO_STATE_DIR}/plugins/<id>/icon.png`, `${CITO_STATE_DIR}/runtime/plugins/<id>/icon.png`, `CITO_BUILTIN_PLUGINS_DIR/<id>/icon.png`, and bundle-relative `../Resources/plugins/<id>/icon.png` / `../../plugins/<id>/icon.png`, caching the loaded `NSImage` by plugin `id`.
  - In `makeHarnessBadge`, `DirectoryPeerCellView` (`configure` & `configureActivity`), and `settings_channel:<id>` `chBadge`:
    - When `pluginIconImage(for: id)` returns an `NSImage`, render it full-bleed (`icon.frame = badge.bounds`, `imageScaling = .scaleProportionallyUpOrDown`, `contentTintColor = nil`) inside the continuous rounded-rect `badge` (`fillColor = .clear`, `strokeColor = .labelColor.withAppearanceAlpha(0.14)`, `cornerRadius = radius`, `masksToBounds = true`).
    - When no `icon.png` exists (`human` or custom third-party plugins), retain the tinted `SurfaceView` and white SF Symbol fallback.

#### 3. `N3` — `Hubs & Network` Tab Cohesive Invite Workflow, Tailscale MagicDNS Priority & Frictionless Hub Name Normalization (`crates/cito-native/src/core/daemon.rs`, `src/macos/cito-ui.swift`)
- **Basis**: Make cross-Mac Hub sharing and joining self-explanatory, prefer stable Tailscale MagicDNS hostnames over ephemeral LAN DHCP IPs, and remove the arbitrary lowercase-only `HUB NAME` rejection.
- **Unified `Invite Code` Terminology & Actionable Inline Guidance**:
  - Rename `copy_hub_invite` button text to **`"Copy Invite Code"`** (copied feedback: `"✓ Invite Copied"`) and `paste_hub_invite` button text to **`"Paste Invite Code"`** (pasted feedback: `"✓ Joined"`).
  - When `host_hub_card` is expanded (`hostHub.enabled == true` and no error is active), display actionable subtitle text: **`"On another Mac, click Paste Invite Code below (or run in Terminal) to connect."`**
- **Tailscale MagicDNS / IPv4 Priority in `advertised_hub_host(bind: &str)` (`crates/cito-native/src/core/daemon.rs:3058`)**:
  - When `bind` is wildcard/empty (`"" | "0.0.0.0" | "::"`):
    1. Check `CITO_TAILSCALE_HOST` (for deterministic tests) or query Tailscale (`tailscale status --json` via `/opt/homebrew/bin/tailscale`, `/usr/local/bin/tailscale`, `/Applications/Tailscale.app/Contents/MacOS/Tailscale`, or `PATH` when `CITO_UI_HEADLESS != "1"` and `CITO_DISABLE_TAILSCALE_PROBE != "1"`, cached with a 30s TTL so `snapshot()` never blocks on repeated subprocess spawns): if `BackendState == "Running"`, return `Self.DNSName` trimmed of trailing `.` (e.g., `mac-studio.tailnet.ts.net`), or the first IPv4 in `Self.TailscaleIPs` if `DNSName` is empty.
    2. Fall back to the existing UDP default-route LAN IPv4 probe (`192.0.2.1:9`), then resolvable `local_host_name()`, then `"127.0.0.1"`.
- **Frictionless `HUB NAME (OPTIONAL)` Normalization & Auto-Derivation (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`)**:
  - In `add_hub_form` (for remote Hubs), label the field **`"HUB NAME (OPTIONAL)"`** with placeholder **`"Auto-filled from URL"`**.
  - On submit in `cito-ui.swift` and in `DaemonService::setup_save_hub` (`daemon.rs:2324`):
    - If `name` is empty, derive it automatically from the URL host (taking the first hostname label before `.` when non-numeric, or replacing `.` with `-` for IPv4 literals, fallback `"remote"`), and if creating a new Hub (`original_hub` is empty) whose derived name is `"local"` or already belongs to an existing Hub with a different URL, disambiguate with a numeric suffix (`<name>-2`, `<name>-3`, matching `parse_hub_invite`).
    - Normalize any explicitly provided `name` via `normalize_slug` (lowercasing ASCII letters and collapsing spaces/punctuation into `-`, trimmed to 32 chars) before calling `validate_hub_name`, so mixed-case or spaced names like `"Office Mac"` or `"Work"` succeed seamlessly as `"office-mac"` / `"work"` (while rejecting an empty post-normalization slug such as `"!!!"`, a new Hub explicitly named `"local"`, or a create operation where `original_hub` is empty and the normalized `name` already belongs to an existing Hub with a different URL).

#### 4. `N4` — Purpose-Driven GitHub Pages Visual Walkthrough & 1:1 DPI-Proportional Screenshot Stage (`site/index.html`, `site/docs/guide.html`, `site/styles/main.css`, `site/scripts/hero-preview.js`)
- **Basis**: Screenshots on `https://tefx.github.io/cito/` must (a) demonstrate **concrete user workflows and how to accomplish each step** rather than dumping a sterile 4-card UI component catalog with duplicate `<div class="native-panel">` HTML text transcripts, and (b) render at **strictly uniform 1:1 AppKit point scale (`1pt === 1pt`)** so compact surfaces (`menu-dark.png` `380pt`, `banner-dark.png` `388pt`) are never stretched to `147%` magnification beside `560pt` surfaces (`directory-dark.png`, `activity-dark.png`) and tab switching never jumps in height.
- **Unified 1:1 DPI-Proportional Screenshot Stage System (`.screenshot-stage-viewport`, `.docs-figure--staged`, `.screenshot-img`)**:
  - Define a canonical **Reference Stage (`560pt × 460pt` logical bounding envelope)** across `site/index.html` and `site/docs/guide.html`:
    - Every screenshot `<img>` carries `class="screenshot-img"` and `style="--logical-w: <pt>; --logical-h: <pt>;"` (`380×334` for `menu-dark.png`, `388×136` for `banner-dark.png`, `520×460` for `settings-*-dark.png`, `560×440` for `directory-dark.png` / `activity-dark.png`), and CSS sizes it strictly as `width: calc((var(--logical-w, 560) / 560) * 100%); max-width: calc(var(--logical-w, 560) * 1px); max-height: 100%; height: auto;`.
    - In `site/index.html`, wrap each Moment's screenshot in `.screenshot-stage-viewport` (`aspect-ratio: 560 / 460`, centered with a dark studio backdrop) so switching tabs inside any Moment (`menu` ↔ `directory`, `banner` ↔ `activity`, `apps` ↔ `channels` ↔ `hubs`) produces **zero container height shift** and preserves identical `1pt` typography/button scale across all 7 screenshots.
    - Preserve the tallest responsive caption by overlapping workflow panels in a shared CSS grid track; inactive panels retain layout space with `visibility: hidden` and remain unavailable to pointer, focus, and accessibility navigation. Match vertical viewport padding to horizontal padding multiplied by `460 / 560`, and draw the screenshot edge with an inset outline rather than a layout-consuming border. These refinements preserve the fixed viewport and proportional image formulas without caption-driven shell shifts or independently clamped image height.
    - In `site/docs/guide.html`, wrap every inline screenshot in `<figure class="docs-figure docs-figure--staged"><div class="docs-figure-stage">...</div><figcaption class="docs-figcaption">...</figcaption></figure>` (`max-width: 38rem` = `608px` outer with `1.5rem` horizontal stage padding = `560px` inner canvas) so documentation figures share the exact same `1:1` proportional scale.
- **Homepage (`site/index.html`) — Replace Component Dump with Scenario-Driven Visual Proof**:
  - Remove the standalone `"Four surfaces. One signaling network."` (`surface-showcase`) component dump and all duplicate `<div class="native-panel"><div class="native-row">...` text-transcript blocks that repeated the text already visible inside the PNG screenshots.
  - Embed the real 2× Retina screenshots directly into the **three workflow moments (`Moment 01`, `Moment 02`, `Moment 03`)** inside `.moment-stage-shell[data-surface-preview]` with numbered `.step-tab` buttons (`.step-num` + label, keyboard-accessible via `ArrowLeft`/`ArrowRight`/`Home`/`End` in `hero-preview.js`) and `.screenshot-callout` captions explaining *what action the user takes and why*:
    1. **Moment 01 · Cross-Harness Discovery & Signaling (`Stop playing courier between your agents`)**:
       - Interactive 2-step visual switcher (`data-surface-preview`):
         - Step 1 (`menu-dark.png`, `--logical-w: 380`): **`1. Enroll from Menu Bar`** — *"Click `+ Listen` next to any open Codex, Pi, or Claude Code session in the menu bar (or run `/cito on` inside the session)."*
         - Step 2 (`directory-dark.png`, `--logical-w: 560`): **`2. Discover in Directory`** — *"Sessions find each other by repository or task and send direct wake signals to readable `user/app#id` addresses."*
    2. **Moment 02 · Human in the Loop (`You are a node on the same network`)**:
       - Interactive 2-step visual switcher (`data-surface-preview`):
         - Step 1 (`banner-dark.png`, `--logical-w: 388`): **`1. Instant Desktop Alert`** — *"When an agent messages `@self`, a floating banner appears on your Mac alongside your paired Telegram chat."*
         - Step 2 (`activity-dark.png`, `--logical-w: 560`): **`2. 1-Line Reply in Activity`** — *"Click the banner or press `↩ Reply` in Activity—Cito wakes the waiting agent and clears your Telegram unread state."*
    3. **Moment 03 · 1-Click Setup & Multi-Mac Topology (`From one Mac to your whole team`)**:
       - Interactive 3-step visual switcher (`data-surface-preview`):
         - Step 1 (`settings-apps-dark.png`, `--logical-w: 520`): **`1. Connect Apps`** — *"Click `Set Up` in `Settings… → Agent Apps` to register MCP tools and wake hooks with explicit consent."*
         - Step 2 (`settings-channels-dark.png`, `--logical-w: 520`, showing real `Chat ID` field label): **`2. Pair Telegram`** — *"Save your bot gateway token once, then click `Pair Account…` to bind your personal chat via a 10-character `/start` command."*
         - Step 3 (`settings-hubs-dark.png`, `--logical-w: 520`): **`3. Link Macs & Hubs`** — *"Toggle on `Share Hub on Local Network`, click `Copy Invite Code`, and click `Paste Invite Code` on your second Mac."*
- **Getting Started Guide (`site/docs/guide.html`) — Contextual Inline Step Placement**:
  - Remove the 6-screenshot wall at the end of Section 2 (`lines 107–112`) and place each `.docs-figure--staged` screenshot **inline inside the exact section it teaches**, paired with an action-oriented `<figcaption>`:
    - Section 2 (`Daily Workflow: Menu Bar, Directory & Activity`): `menu-dark.png` (`--logical-w: 380`), `directory-dark.png` (`--logical-w: 560`), and `activity-dark.png` (`--logical-w: 560`) placed alongside their corresponding workflow paragraphs;
    - Section 3 (`Agent Apps Setup`): `settings-apps-dark.png` (`--logical-w: 520`) placed at the top of Section 3;
    - Section 4 (`1-Click Telegram Pairing`): `settings-channels-dark.png` (`--logical-w: 520`) and `banner-dark.png` (`--logical-w: 388`, inside `.docs-figure-stage--compact`) placed inside Section 4;
    - Section 5 (`LAN, Tailscale & Team Hub Setup`): `settings-hubs-dark.png` (`--logical-w: 520`) placed inside Section 5 (`Sharing Across Your Own Macs`).

### 16.2 Verification & Anti-Over-Testing Policy (`MUST`)

- Update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs}` in place (including `"Copy Invite Code"`, `"Paste Invite Code"`, `"HUB NAME (OPTIONAL)"`, and `"card_padding"` in `test_oracle_calibration`); do **NOT** add new multi-process/daemon E2E test functions.
- Regenerate the 7 canonical website screenshots in `site/assets/screenshots/*.png` so they showcase the unified card spacing, real app icons (`Codex`, `Pi Coding Agent`, `Claude Code`, `Telegram`), and redesigned `Hubs & Network` tab.

---

## 17. Stage 15 (`v0.21.4` Candidate): Crafted macOS Utility De-Boxing, Hit-Test Reachability, Progressive Disclosure & Minimalist Web Architecture (`P1`–`P5`)

### 17.1 Binding Contract (`MUST`)

#### 1. `P1` — AppKit Hit-Test Reachability, Hidden-Subview Hygiene & Action Feedback Symmetry (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`, `plugins/codex/native.rs`)
- **Basis**: Eliminate silent click interception by hidden controls (`MenuControl.hitTest` / `ClickView.hitTest`) and silent no-op save/check actions (`save_channel:telegram` and `Re-check` in `Agent Apps`).
- **AppKit Hit-Test & Hidden-Subview Contract (`src/macos/cito-ui.swift:459, 604`)**:
  - `MenuControl.hitTest(_ point: NSPoint)` and `ClickView.hitTest(_ point: NSPoint)` MUST check `guard !isHidden && !isHiddenOrHasHiddenAncestor && alphaValue > 0.01 else { return nil }; return frame.contains(point) ? self : nil`.
  - Disable hover/click affordances (`isEnabled = false` or pass-through) on non-interactive `MenuControl` instances where `pressed == nil` (`composer_target`, single-hub `switch_hub:<id>`).
  - In `host_hub_card` (`cito-ui.swift:3085–3150`), only instantiate and mount expanded subviews (`endpointLabel`, `token`, `copyInvite`, `portLabel`, `portField`, `auth`, `apply`) when `hostExpanded == true`.
  - In `directory_facet_bar` (`cito-ui.swift:3585`), initialize `activity_clear` (`clear`) with `clear.isHidden = true` and only lay out/expose `Activity` controls when `directoryMode == "activity"`.
  - In `settings_hub:<name>` (`cito-ui.swift:3349`), do not add `toggleBtn` to `row` when `local && resolvedHubs.filter { $0.enabled }.count <= 1 && hub.enabled`.
  - **Mechanical `inspectMenu()` Hit-Test Oracle**: For every visible, enabled `MenuControl` with `pressed != nil` across `cardMenu`, `settingsPanel`, and `directoryPanel` whose center point is currently visible inside any enclosing `NSClipView` (`control.visibleRect.contains(NSPoint(x: control.bounds.midX, y: control.bounds.midY))`, excluding background elements when `uninstallConfirmOpen == true`), convert `NSPoint(x: control.bounds.midX, y: control.bounds.midY)` to the surface root (`item.view` or `window.contentView`) and verify `root.hitTest(pt) === control`; append `"<surface> > <id>: occluded in hitTest"` to `overlapping_siblings` on any violation, and add `"hit_test"` to `test_oracle_calibration`.
- **Action Feedback Symmetry (`Save` in `Human Channels` & `Re-check` in `Agent Apps`)**:
  - Track `channelSavedUntil: [String: Date]` in `cito-ui.swift`. When `acceptSettings` acknowledges a channel save (`savedChannel == channel.id`), set `channelSavedUntil["\(target)|\(channel.id)|\(entry.scope)"] = Date().addingTimeInterval(1.5)` and collapse `editingPersonalChannel.remove(key)` (when `entry.scope == "user"` and `channel.configured == true`), displaying `"✓ Saved"` feedback (`saveBtn.copied("✓ Saved")` or status badge feedback).
  - In `Settings → Agent Apps` (`cito-ui.swift:2742`, `daemon.rs:2197`), when clicking `Re-check` (`setup_app:<id>` when `app.isConfigured == true`), emit a `"settings"` frame from `daemon.rs` on `setup_apply` / `setup_remove_app` and trigger `btn.copied("✓ Checked")` feedback.
  - In `plugins/codex/native.rs:69–75`, require `cito.get("enabled").and_then(Item::as_bool) != Some(false)` in `configured()`, accept `cmd == "cito-mcp" || cmd.ends_with("/cito-mcp")`, and remove an empty `[mcp_servers]` table on `setup_remove`.

#### 2. `P2` — Crafted macOS Utility De-Boxing, Clutter Removal & 3-State Progressive Disclosure (`src/macos/cito-ui.swift`, `plugins/telegram/plugin.json`)
- **Basis**: Transform Cito's UI from an industrial multi-box control panel into a calm, native macOS utility by eliminating Russian-doll borders, tautological subtitles, and permanent setup form sprawl.
- **Control & Border Hierarchy (`MenuControl.Style` & `SurfaceView`)**:
  - Soften card surfaces (`fill: .labelColor.withAppearanceAlpha(0.045)`, `border: .labelColor.withAppearanceAlpha(0.08)`).
  - Convert chrome, navigation, and tertiary actions from bordered `.neutral` boxes to borderless `.subtleHeader` (or borderless destructive-on-hover):
    - `cardMenu`: `copy:<scoped_id>` address chips, `stop:<scoped_id>` (`✕` on sessions and `Pause` on `You`), and footer items (`stop_all`, `open_settings`, `quit`).
    - `Settings`: group the 3 top tabs (`settings_tab:apps`, `channels`, `hubs`) inside a single segmented track (`SurfaceView`, `radius: 8, fill: .labelColor @ 0.05, border: .labelColor @ 0.08`) with borderless inactive tabs; make `close_settings` (`✕`), `dismiss_settings_error` (`✕`), `settings_launch_at_login` checkbox, `uninstall_purge_data` checkbox, and `open_uninstall_confirm` borderless at rest (`.subtleHeader`); make `Re-check` / `Disconnect` in `Agent Apps` and `Edit` / `Remove` in `Connected Hubs` quiet `.subtleHeader` controls, and style `Enabled` on `Connected Hubs` as `.neutral` rather than bright blue `.prominent`.
    - `Directory / Activity`: group `dir_mode:directory` and `dir_mode:activity` in a segmented track; make `close_directory` (`✕`), `activity_mark_all_read` (`✓ Read All`), and `activity_clear` (`Clear`) borderless `.subtleHeader` controls.
- **`Settings → Agent Apps` Single-Row Compact Cards & `Experimental` Pill Alignment (`cito-ui.swift:2684–2745`)**:
  - Change section header from `"CONNECTED AGENT APPS"` to **`"AGENT APPS"`** and show the onboarding helper line only when 0 apps are configured.
  - Omit tautological second-line `detail` text when `detail == "Click Set Up to connect"` or `detail == "Connected"`, rendering healthy `Connected` / `Not Set Up` apps as compact **single-row `52pt` cards** (`badge` `28 × 28` at `y = 12`, `layoutBar` at `y = 14, height = 24`), expanding to `76pt` only when `!app.isInstalled`, `app.status == "error"`, or a non-default diagnostic exists.
  - Place `settings_app_tier:<id>` (`Experimental`) in `leading` immediately to the right of `title` (`8pt` gap, `height: 20` centered at `y = 16`, with optical text baseline `y = 3.5, height: 14`) rather than in `trailing`.
- **`Settings → Human Channels` (`Telegram`) 3-State Progressive Disclosure & Copy Cleanup (`cito-ui.swift:2765–3045`, `plugins/telegram/plugin.json`)**:
  - Change `"TARGET HUB:"` to sentence-case **`"Target Hub"`**.
  - Omit `chDetail` under `Telegram` unless `ch.error != nil` or `channelResult != nil`.
  - Remove `field_help` lines (`2878–2883`) below inputs and remove the `[ Skip for Now ]` button (`skip_channel:<id>`) from visible UI.
  - Update `pairing_help_text` in `plugins/telegram/plugin.json` to **`"Generate a one-time /start code to link your Telegram chat (expires in 10 minutes)."`**
  - **Progressive Disclosure States**:
    - **When `!gatewayActive`**: `stratum_hub:<id>` shows concise 1-line setup guidance (`"Create a bot via @BotFather on Telegram and paste its token below."`), the `Bot Token` field, and `[ Save Gateway Token ]`; `stratum_user:<id>` shows a single compact prerequisite line (`"2. Your Personal Account (@\(currentUser)) — Complete Step 1 above to enable personal pairing."`, preserving `personalDescription` instead of deleting it).
    - **When `gatewayActive` (Collapsed Gateway Bar)**: `stratum_hub:<id>` collapses to a `36pt` 1-line bar (`border: nil`, controls at `y = 7, height = 22`) with `Change Token…` widened to `96pt` and `Clear Token` (`width: 78`, uniform `10.5pt` font).
    - **When `gatewayActive && ch.configured && !editingPersonalChannel.contains(channelKey) && instruction.isEmpty`**: `stratum_user:<id>` (`height: 36pt, border: nil`, controls at `y = 6, height = 24`) collapses the editable `Chat ID` field and `Save` button into a clean connected summary row showing **`● Paired (@\(currentUser) · \(chatId))`** (or `"✓ Saved"` when `Date() < channelSavedUntil`) on the left and **`[ Send Test ]` `[ Pause / Resume ]` `[ Edit… ]` `[ Unpair ]`** on the right! Clicking `[ Edit… ]` (`edit_user_channel:<id>`, or receiving `test_focus_field` / `test_settings_input` on `channel_field:<id>:...`) expands the `Chat ID` input field and `[ Save ]` button.
    - **When `gatewayActive && (!ch.configured || editingPersonalChannel.contains(channelKey) || !instruction.isEmpty)`**: show concise OR guidance (`"Click Pair Account… to link via Telegram, or enter your Chat ID manually."`), placeholder `"Auto-filled by Pair Account, or enter Chat ID"`, and widen `pairing_instruction:<id>` (`10.5pt` monospaced) so 37-char `/start` instructions never orphan-wrap `@bot`.

#### 3. `P3` — Canonical `Pi → Codex → Claude Code` Ordering, Smart Hub Sharing/Naming & Modal/Empty-State Polish (`crates/cito-native/src/core/{plugins.rs,daemon.rs}`, `src/macos/cito-ui.swift`, `plugins/*/plugin.json`)
- **Canonical Harness Ordering (`Pi → Codex → Claude Code`) (`crates/cito-native/src/core/plugins.rs:487`, `plugins/{pi,codex,claude}/plugin.json`)**:
  - Set `"order": 0` in `plugins/pi/plugin.json`, `"order": 1` in `plugins/codex/plugin.json`, `"order": 2` in `plugins/claude/plugin.json`, parse `"order"` in `PluginDescriptor`, and sort by `(kind_rank, p.order, tier_rank, p.display_name.clone())` in `plugins.rs:487–497` so **`Pi Coding Agent` is #1, `Codex` is #2, and `Claude Code` is #3** across all UI surfaces, CLI outputs, and docs.
- **`Hubs & Network` — Tailscale vs. LAN Switcher, Smart Hub Naming & Sentence-Case Form (`crates/cito-native/src/core/daemon.rs`, `src/macos/cito-ui.swift`)**:
  - **Tailscale vs. LAN Address Validity & Switcher (`daemon.rs:2010–2024, 2698–2806`, `cito-ui.swift:3060–3150`)**:
    - In `tailscale_host_from_status` (`daemon.rs:2698`), require `status["Self"]["Online"] != Value::Bool(false)` and only use `Self.DNSName` when `status["CurrentTailnet"]["MagicDNSEnabled"] != Value::Bool(false)` and `!dns.ends_with(".local")`, falling back to the IPv4 entry in `Self.TailscaleIPs`. Filter `!addr.ip().is_loopback()` in the `local_host_name()` fallback (`daemon.rs:2801`).
    - In `settings_for_hub` (`daemon.rs:2010`), expose both `"tailscale_url"` / `"tailscale_invite_command"` and `"lan_url"` / `"lan_invite_command"` (when both exist and differ), and derive `--hub <slug>` in `invite_command` from the host machine name (`hub_name_from_url`) rather than hardcoding `--hub lan` (and disambiguate colliding invite hub names in `parse_hub_invite`).
    - In `host_hub_card`, when both `tailscale_url` and `lan_url` exist and differ, render a compact 2-pill switcher **`[ Tailscale ]` `[ Local IP ]`** (`host_hub_addr:tailscale`, `host_hub_addr:lan`) so the user can toggle which address is displayed and copied by `Copy Invite Code` (defaulting to Tailscale).
  - **Smart Hub Name Derivation from IP & Domain URLs (`cito-ui.swift:22–50, 3180–3290`, `daemon.rs:2349–2435, 2930–2943`)**:
    - When deriving a default Hub name from a URL host: if the host is a private LAN IPv4 (`192.168.*`, `10.*`, `172.16–31.*`), derive **`"lan"`**; if it is a Tailscale CGNAT IPv4 (`100.64–127.*`), derive **`"tailscale"`**; if it is another IP literal, derive **`"remote"`**; if it is a domain/hostname, take the first non-empty alphanumeric slug label (falling back to `"remote"` if none), and disambiguate against `existing.filter { !editing || $0.name != original_hub }` in both Create and Edit flows. Trim `url` before parsing and store the normalized URL returned by `validate_nats_url`.
    - In `add_hub_form`, use clean sentence-case labels (**`Hub URL`**, **`Hub Name (Optional)`**, **`Access`**, **`Username (Optional)`**, **`Shared Access Token`**, **`OIDC Issuer URL`**, **`OIDC Client ID`**), place **`Hub URL` on the left (`x = 14, width = 298`) and `Hub Name (Optional)` on the right (`x = 320, width = 146`)**, update `Hub Name`'s placeholder live in `controlTextDidChange` to show `"Default: \(derived)"` (`"e.g. studio or work"` when URL is empty), and **delete the redundant bottom-left `protocolHelp` label (`"Supports http://..."`)**.
    - In `settings_hub:<name>` rows (`cito-ui.swift:3311`), widen `subtitleWidth` to `438pt` so connected Hub subtitles never wrap prematurely at `222pt`.
- **Directory Human Row De-duplication, Empty-State Centering, Modal Alignment & Error Sanitization (`cito-ui.swift`)**:
  - In `DirectoryPeerCellView.configure(with peer:)` (`cito-ui.swift:951`), when `peer.harness == "human"`, hide `line2UserLabel` (`stringValue = ""`) and start `line2MetaLabel` at `x = 52` so `@bob` is never printed twice on stacked lines; update `directorySearchField.placeholderString` in `Activity` mode to `"Search messages, people, or sessions…"`.
  - In `updateDirectoryEmptyState()` (`cito-ui.swift:3838–3879`), set `.alignment = .center` on `title` and `subtitle`, and call `closeDirectoryPanel(emitAction: true)` when clicking `dir_empty_open_apps` or `act_empty_open_channels`.
  - In `uninstall_confirm_box` (`cito-ui.swift:3380–3437`), hide `settings_error_banner` while `uninstallConfirmOpen == true`, add a 1-line subtitle explaining `Disconnect All` (removes app integrations, keeps Cito running) vs. `Confirm Uninstall` (removes Cito shims/runtime and quits), style `uninstall_purge_data` as a borderless `.subtleHeader` checkbox row, lay out the bottom buttons cleanly via `layoutBar`, and preserve `harness == "human"` in `setup_disconnect_all` (`daemon.rs:2549`) when `purge_data != true`.
  - In `presentationError(_:)` (`cito-ui.swift:158`), replace internal `CITO_OIDC_ISSUER` / `CITO_OIDC_CLIENT_ID` / `CITO_HUB_TOKEN` / `(http://127.0.0.1 is allowed for tests)` tokens with clean user-facing phrasing, humanize `hub.status` in `pill.toolTip` (`cito-ui.swift:1947`), and map `"red"` to `CitoPalette.dangerText` in `tintColor(_:)`.

#### 4. `P4` — Minimalist Website Consolidation, Equal-Height Grid Tabs, Direct Download Links & Hand-Drawn Architecture Diagram (`site/**`, `README.md`)
- **Direct Download Asset Links & External Link Targets (`Web 3, 10, 11`)**:
  - Point all `"Download Cito.app for macOS"`, `"Download Cito.app"`, and `"Cito-macos-arm64.zip"` links in `site/index.html`, `site/docs/guide.html`, and `README.md` directly to `https://github.com/Tefx/cito/releases/latest/download/Cito-macos-arm64.zip`.
  - Add `target="_blank" rel="noopener noreferrer"` to all external `https://github.com/Tefx/cito...` links across `site/index.html`, `site/docs/guide.html`, `site/docs/plugins.html`, and `site/docs/architecture.html`.
- **Zero-Wrap Pills/Links, Equal-Height Tab Stacking & `.cta-banner` Fix (`Web 1, 2, 8, 11`)**:
  - Remove `overflow-wrap: anywhere;` from `body` in `site/styles/main.css` (scoping `overflow-wrap: break-word;` to prose/code containers), and add `white-space: nowrap; flex-shrink: 0;` to `.node-pill`, `.scene-tab`, `.step-tab`, `.text-link`, `.nav-cta`, `.secondary-button`, `.eyebrow`, `.moment-index`, and `.harness-tag`, plus `flex-shrink: 0;` on `.node-dot` and `.step-num` and `align-self: flex-start` on `.harness-card .text-link`.
  - Stack `.hero-scene-panel` (if retained) and `.moment-stage-panel` in a shared CSS Grid cell (`grid-area: 1 / 1` or `2 / 1` with `display: flex !important; visibility: hidden; pointer-events: none;` on `[hidden]`) so switching tabs in Hero or Moments produces **0px height difference**.
  - Match `.cta-banner, .closing-card` in `site/styles/main.css:542` so the bottom `"Get started in 30 seconds"` card renders with its centered card background, border, and padding.
- **Accurate Scenarios, `Pi → Codex → Claude Code` Ordering & Open Reply Composer Capture (`Web 4, 5, 6, 7, 8`)**:
  - Order all harness mentions, cards, and guide sections as **`Pi Coding Agent` (`Pi`) → `Codex` → `Claude Code`** across `site/index.html`, `site/docs/guide.html`, `site/docs/architecture.html`, `README.md`, and `.agents/plugins/marketplace.json`.
  - Update Hero Scene 1 and Moment 01 to the real workflow: **Pi for orchestration, Codex for backend API/engineering (`codex · api-server`), and Claude Code for frontend UI (`claude · web-app`)**.
  - Attribute `/cito on` accurately to Pi (`"or run /cito on in Pi"`).
  - Use concrete readable address examples (`@self`, `@alice`, `alice/pi#8a4f`) instead of abstract `user/app#id`.
  - Capture `site/assets/screenshots/activity-dark.png` with clean non-error rows and the bottom **inline reply composer bar (`directory_composer`) open** (`To: bob/pi#8a4f`, draft text `"Ship v2.4 now — docs can follow."`).
- **Hand-Drawn Excalidraw-Style Architecture SVG (`Web 9`, `site/docs/architecture.html`)**:
  - Replace the 3D decorative render (`architecture-topology.png`) in `site/docs/architecture.html` with a hand-drawn Excalidraw-style architectural SVG diagram (`site/assets/art/architecture-topology.svg`) showing **Agent Session Nodes (`Pi`, `Codex`, `Claude Code` via `cito-mcp` + wake drivers) ↔ Cito Hub (`cito-native daemon :4290`, `HubEngine` SQLite WAL, `HubServer :4291`, Leases, Split Read State & Bounded Retry) ↔ Human Node (`@self` / `@alice` across Mac Menu Bar & Telegram Bot Gateway) + Remote/Team Hubs (`Tailscale / LAN :4291 & OIDC SSE`)**.

### 17.2 Verification, Fast Single-Pass Review & No-Formal-Release Policy (`MUST`)

- **Strict Test Economy**: Update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs,distribution.rs,site-clipboard.mjs}` in place; do **NOT** add new multi-process E2E test functions.
- **Fast Single-Pass Review Only**: Run a single-pass `contract-and-simplicity` review & fix (do **NOT** run multi-round review loops).
- **No Formal Release Tagging Yet**: Push verified changes to `origin/main` for immediate local user testing on macOS; do **NOT** push a new release tag or trigger `release.yml` notarization/Homebrew publication until the user completes hands-on testing and explicitly approves release.

---

## 18. Stage 16: Unified macOS Control Taxonomy, Intrinsic Layout Engine, Chinese IME Support, Distinct Directory/Activity Surfaces & Self-Invite Guard (`Q1`–`Q6`)

### 18.1 Binding Contract (`MUST`)

#### 1. `Q1` — Unified macOS Control Taxonomy & Modal Action Consistency (`src/macos/cito-ui.swift`)
- **Basis**: Eliminate visual inconsistency where destructive or secondary actions (`Disconnect` in `Agent Apps`, `Confirm Uninstall` in `uninstall_confirm_box`, `Remove` in `Connected Hubs`, `Unpair`, `Clear Token`) lost their button silhouette (`border = nil, fill = .clear`) and appeared as floating plain text next to bordered pill buttons (`Set Up`, `Cancel`, `Disconnect All`).
- **Five-Class Control Taxonomy (`MenuControl.Style`)**:
  1. **`.prominent` (Primary Constructive Button)**: Solid accent fill (`CitoPalette.prominentFill`), `1pt` border (`white @ 0.22`), white `.medium` label/icon (`Set Up`, `Save`, `Save Gateway Token`, `Join Hub`, `Copy Invite Code`, `Send ⏎`, `Resume`, active segmented tab).
  2. **`.neutral` (Secondary Pill Button)**: Subtle pill fill (`labelColor @ 0.08`), `1pt` border (`labelColor @ 0.14`), primary `.labelColor` (`Cancel`, `Re-check`, `Send Test`, `Pause`, `Pair Account…`, `Paste Invite Code`, `Add Hub…`, `Apply`, `+ Listen`, `Message`, `Reply`, `Enabled`/`Disabled`).
  3. **`.destructiveHover` (Secondary Destructive Pill Button)**: **Must retain the exact same resting pill silhouette as `.neutral`** (`fill = .labelColor.withAppearanceAlpha(0.08)`, `border = .labelColor.withAppearanceAlpha(0.14)`, `foreground = CitoPalette.secondaryText`), transitioning on hover to solid red (`CitoPalette.destructiveFill`, `border = .white.withAlphaComponent(0.26)`, `foreground = .white`). Used for `Disconnect` (`remove_app:<id>`), `Remove` (`remove_hub:<name>`), `Unpair` (`unpair_channel:<id>`), and `Clear Token` (`clear_channel_service:<id>`), so right-edge action columns in `Agent Apps` and `Connected Hubs` have uniform button geometry at rest.
  4. **`.destructive` (Primary Destructive Confirmation Pill Button)**: Tinted red pill at rest (`fill = CitoPalette.dangerText.withAppearanceAlpha(0.14)`, `border = CitoPalette.dangerText.withAppearanceAlpha(0.38)`, `foreground = CitoPalette.dangerText`), transitioning on hover to solid red (`CitoPalette.destructiveFill`, `foreground = .white`). Used for `Confirm Uninstall` (`confirm_uninstall_all`).
  5. **`.subtleHeader` (Borderless Ghost Chrome / Checkbox / Menu Footer / Inactive Segment)**: `fill = .clear`, `border = nil` at rest (`fill = .labelColor @ 0.10` on hover). Restricted strictly to: (a) icon-only close/dismiss/row actions (`✕` `close_settings`, `close_directory`, `dismiss_*`, `stop:<id>` on menu session cards, icon-only `copy_activity_msg:<id>` / `delete_activity:<id>`), (b) inactive tabs inside a bordered segmented track, (c) checkboxes (`Start Cito at Login`, `uninstall_purge_data`), (d) collapsible section headers, (e) menu bar footer items (`Stop All`, `Settings…`, `Quit Cito ⌘Q`), and (f) inline copyable address chips (`copy:<id>`).
- **Modal Action Alignment (`uninstall_confirm_box`, `cito-ui.swift:3455–3505`)**:
  - Title: **`"Disconnect or Uninstall Cito"`** (matching the two buttons `Disconnect All` and `Confirm Uninstall`, removing the orphan word `"Reset"`).
  - Delete the verbose subtitle (`"Disconnect keeps Cito running; uninstall removes Cito and quits."`).
  - Checkbox (`uninstall_purge_data`, `.subtleHeader`): `"Also delete local data and Hub settings"`.
  - Bottom bar via `layoutBar(in: confirmBox, insetLeft: 12, insetRight: 12, y: 14, height: 24, leading: [cancelUninstallBtn], trailing: [disconnect, confirmUninstallBtn])` with `cancelUninstallBtn` (`.neutral`), `disconnect` (`.neutral`), and `confirmUninstallBtn` (`.destructive`).

#### 2. `Q2` — Systemic Intrinsic-Sizing Engine & Anti-Verbosity Subtitle Trim (`src/macos/cito-ui.swift`)
- **Basis**: Replace hardcoded widths/offsets (`width: 140, x: 158` on form fields, `x = 6` icon + centered `label` gap drift inside `MenuControl`, and `y = 3.5` text offset inside `Experimental`) with intrinsic font/text measurement primitives, and eliminate static subtitles that merely repeat card titles or button labels.
- **1. Centered `[Icon + 5pt Gap + Text]` Unit in `MenuControl.layoutContents()` (`cito-ui.swift:420–465`)**:
  - Add `MenuControl.layoutContents()` (called from `init`, `setLeadingSymbol`, `copied`, and `frame` didSet):
    - Compute `cellH = ceil(label.cell?.cellSize.height ?? (font.ascender - font.descender + font.leading))` and `labelY = floor((bounds.height - cellH) / 2)`.
    - When `leadingSymbolView` is visible (`!label.isHidden`) and `label.alignment != .left`, measure `textW = max(0, min(bounds.width - 28, ceil(measureTextWidth(label.stringValue, font: font)) + 6))` (the `6pt` allowance accommodates native `NSTextFieldCell` drawing margins so labels such as `Reply` and `Copy` do not truncate), compute `groupW = 12 + 5 + textW` and `startX = max(6, floor((bounds.width - groupW) / 2))`, and place `leadingSymbolView` at `x = startX, y = floor((bounds.height - 12) / 2), width = 12, height = 12` and `label` at `x = startX + 17, y = labelY, width = textW, height = cellH`. (Icon-only controls initialized with `symbol:` keep `centerSymbolView` centered in `bounds` with `label.isHidden = true`).
    - When `label.alignment == .left` (such as checkboxes and collapsible headers), place `leadingSymbolView` at `x = 6` and `label` at `x = 23`.
  - Add `MenuControl.intrinsicWidth(_ title: String, font: NSFont, leadingSymbol: String?, paddingX: CGFloat = 11, minWidth: CGFloat = 0) -> CGFloat` and use it to size segmented tabs (`appsTab`, `channelsTab`, `hubsTab`, `dir_mode:*`, `host_hub_addr:*`) and action buttons without hardcoded width guesses.
- **2. Optical Vertical Centering for Static Badges (`makePillBadge`, `cito-ui.swift:2785, 4865`)**:
  - Introduce `makePillBadge(_ text: String, id: String, height: CGFloat = 20, font: NSFont = .systemFont(ofSize: 9.5, weight: .semibold), textColor: NSColor, fill: NSColor, border: NSColor?, paddingX: CGFloat = 8) -> SurfaceView`:
    - Measure `size = label.cell?.cellSize ?? NSSize(width: measureTextWidth(text, font: font), height: 12)`, set `w = max(88, ceil(size.width) + paddingX * 2)` and `h = ceil(size.height)`, and place `label` at `NSRect(x: 0, y: floor((height - h) / 2), width: w, height: h)`.
    - Use `makePillBadge` for `settings_app_tier:<id>` (`Experimental`) and `banner_semantic_pill` (`Notification` / `Passive`).
- **3. Intrinsic Form Label-to-Field Column Alignment (`cito-ui.swift:2995–3010`)**:
  - In `Human Channels` field rows (`Bot Token`, `Chat ID`, etc.), compute `labelW = max(56, min(110, (fields.map { ceil(measureTextWidth($0.label, font: .systemFont(ofSize: 11, weight: .medium))) }.max() ?? 56) + 6))` and `inputX = 12 + labelW + 10` (`inputW = container.bounds.width - 12 - inputX`). For `Bot Token` / `Chat ID`, `inputX` becomes **`78pt`** (eliminating the `90pt` blank gap!).
- **4. Anti-Verbosity Subtitle Trim Across All Surfaces**:
  - **`Mac Desktop Banner` (`cito-ui.swift:2830`)**: Remove `"Show floating banners on this Mac when notifications arrive."` and render `settings_desktop_banner_card` as a crisp **single-row `46pt` card** (`"Mac Desktop Banner"` vertically centered on the left at `y = 14.5`, `[ ● Enabled ]` on the right at `y = 11, height = 24`).
  - **`Share This Mac as a Hub` (`host_hub_card`, `cito-ui.swift:3160`)**: When collapsed (`!hostExpanded`), remove `"Let your other Macs or teammates connect to this Mac."` and render `host_hub_card` as a **single-row `46pt` card** (`"Share This Mac as a Hub"` at `y = 14.5`, `[ Off ]` at `y = 11, height = 24`). When expanded without error, remove `"On another Mac, click Paste Invite Code below..."` so `host_hub_card` is a compact `136pt` (or `108pt` without address switcher) card.
  - **`Telegram` (`settings_channel:<id>`, `cito-ui.swift:2960–3050`)**:
    - Remove `"Click Pair Account… to link your account, or enter your Chat ID manually."` (`personalDescription`) when `gatewayActive == true`; the row directly presents `[ 🔗 Pair Account… ]` and `Chat ID` (`placeholder: "Auto-filled by Pair Account, or enter Chat ID"`).
    - When `!gatewayActive`, keep `stratum_hub` focused with 1-line guide `"Create a bot via @BotFather (/newbot) on Telegram and paste its token:"` (`placeholder: "123456:ABC-DEF…"`).
  - **`This Mac (Local Hub)` Edit Form (`cito-ui.swift:3291`)**: Remove `"Set the username shown when you send or receive messages on this Mac."` (shrinking the local edit form from `156pt` to `124pt`).

#### 3. `Q3` — Native macOS Chinese Input Method (IME / Pinyin) Support (`src/macos/cito-ui.swift`)
- **Basis**: `KeyPanel` (`settingsPanel` and `directoryPanel`) previously used `styleMask: [.borderless, .nonactivatingPanel]` and intercepted `Up`/`Down`/`Return`/`Escape` in a pre-responder `NSEvent` local monitor before `NSTextInputContext` could process Pinyin composition and candidate selection.
- **IME & Key-Window Contract (`cito-ui.swift:386–396, 1403–1418, 2560, 3592`)**:
  1. Remove `.nonactivatingPanel` from `settingsPanel` and `directoryPanel` (retaining `.nonactivatingPanel` strictly on `WakeBanner`'s `panel` at line `4805`). Override `canBecomeKey { true }` and `canBecomeMain { true }` on `KeyPanel`, and in `makeFirstResponder(_ responder: NSResponder?)`, activate `(currentEditor() as? NSTextView)?.inputContext?.activate()` so macOS Text Services Manager (`TSM`) binds the Pinyin candidate window to the active text field.
  2. In `settingsKeyMonitor` (`cito-ui.swift:1403`), when `window?.firstResponder is NSTextView` (an editable field editor is active), do **NOT** intercept `Up (126)`, `Down (125)`, `Return (36/76)`, or `Escape (53)` in the local event monitor; let `NSWindow.sendEvent` dispatch them through `NSTextView.keyDown` → `NSTextInputContext.handleEvent`. When the IME is not composing, `NSTextView` automatically forwards `moveUp:`, `moveDown:`, `insertNewline:`, and `cancelOperation:` to `control(_:textView:doCommandBy:)` (`lines 4173–4187`), preserving all keyboard navigation and Escape/Return shortcuts without breaking Chinese IME input!

#### 4. `Q4` — Visual & Structural Distinction Between `Directory` and `Activity` + Per-Message Delete (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`)
- **Basis**: Make `Directory` (Online Node Roster) and `Activity` (Signal Inbox) immediately distinguishable at a glance, and support deleting individual messages in `Activity` without cluttering the row.
- **`Directory` (Online Node Roster — Presence & Address Centric, `rowHeight = 52pt`)**:
  - **Online Presence Dot on Badge**: Render an `8 × 8 pt` green online indicator dot (`CitoPalette.successText` with a `1.5pt` surface border ring) as a sibling of `badgeView` inside `DirectoryPeerCellView` at `NSRect(x: badgeView.frame.maxX - 7, y: badgeView.frame.minY - 1, width: 8, height: 8)` (`onlineDot.isHidden = false` in `configure(with:)`, `onlineDot.isHidden = true` in `configureActivity(with:)` so it stays within cell bounds and never trips `clipped_views`).
  - **Row Layout**: Line 1 shows `Title · repo · branch`; Line 2 shows `@user · host` (or `Human · host`). Right side shows `[ ❐ display_address ]` (`copy_dir:<id>`, `.neutral`) and `[ 💬 Message ]` (`msg_dir:<id>`, `.neutral`). Explicitly hide `unreadAccentBar` and `deleteControl` inside `configure(with:)` so reused `DirectoryPeerCellView` instances never leak `Activity` subviews into `Directory` mode.
- **`Activity` (Signal Inbox — Message, Time & Unread-Bar Centric, `rowHeight = 60pt` / `108pt` expanded)**:
  - **Unread Left Accent Bar**: Render a `3pt` vertical accent bar (`x = 4, y = 8, width = 3, height = rowHeight - 16, radius = 1.5`, color `CitoPalette.infoText`, `unreadAccentBar.isHidden = !item.unread`) on unread activity rows, and a `1pt` horizontal row divider (`labelColor @ 0.05`) at `y = 0`.
  - **Header & Relative Timestamp (Line 1, `y = rowHeight - 24`)**: Display `@\(item.from_user) → \(item.target_harness == "human" ? "You" : item.target_title)\(held)` followed by relative age (e.g. `" · 2m ago"` / `" · Just now"` formatted from `item.created_at`).
  - **Full-Width Message Preview (Line 2, `y = 8`)**: Render `item.message` in primary `.labelColor` (`12pt .regular`, `CitoPalette.secondaryText` when read) so the message body is the primary visual focus.
  - **Compact Right-Side Actions + Per-Message Delete (`delete_activity:<signal_id>`)**:
    - On the right (`y = rowHeight - 30`), render `[ ↩ Reply ]` (`reply_activity:<id>`, `.neutral`, when `canReply == true`), `[ ❐ Copy ]` (`copy_activity_msg:<id>`, `.neutral`), and a compact icon-only delete button **`delete_activity:<signal_id>`** (`symbol: "trash"`, `width: 24, height: 22`, `.subtleHeader`, tooltip `"Delete message"`).
    - Clicking `delete_activity:<signal_id>` (or pressing `⌫` / `Delete` keyCode `51` / `117` in `Activity` mode when not editing text) emits `["op": "clear_activity_history", "hub": item.hub, "signal_id": item.signal_id]`.
    - In `crates/cito-native/src/core/daemon.rs:2727` (`"clear_activity_history"`), when `action["signal_id"].as_str()` is non-empty, acknowledge and delete only the matching signal (`WHERE hub = ? AND signal_id = ?`) instead of clearing the entire hub history!

#### 5. `Q5` — Clean 2-Line `AVAILABLE SESSIONS` Rows in `cardMenu` (`src/macos/cito-ui.swift:2400–2460`)
- **Basis**: Eliminate double truncation (`Contact review-sess...  · ~/Pro.../service`) caused by splitting a single `274pt` line into `160pt` title + `114pt` path.
- **2-Line Candidate Row Geometry (`height: 44pt`, or `58pt` when `needsReload == true`)**:
  - **Line 1 (`y = rowHeight - 21, height = 16, x = 24, width = 268`)**: `candidate_title:<harness>:<session_id>` displays `c.title` (`12pt .medium`, `.labelColor`, `.byTruncatingTail`) across the full `268pt` left column (`+67%` wider).
  - **Line 2 (`y = needsReload ? 21 : 6, height = 14, x = 24, width = 268`)**: `candidate_cwd:<harness>:<session_id>` displays `"\(shortCwd)\(branchSuffix)"` (`10.5pt .regular`, `CitoPalette.secondaryText`, `.byTruncatingMiddle`) across its own dedicated `268pt` line (`+135%` wider, no leading `" · "` prefix; when `needsReload == true` at `rowHeight = 58`, `Hint` sits at `y = 5, height = 14`, `Path` at `y = 21, height = 14`, and `Title` at `y = 37, height = 16`, preserving `>= 2pt` separation between all 3 lines).
  - **Right Action Button (`x = 298, y = floor((rowHeight - 22) / 2), width = 70, height = 22`)**: `[ + Listen ]` (`listen:<harness>:<session_id>`) or `[ ❐ /reload ]` (`reload:<harness>:<session_id>`), vertically centered in the row. When `candidateRows.count > 4`, set `available_scroll` height to `176pt` (`4 × 44pt`).

#### 6. `Q6` — Self-Invite Loop Guard & Browser-Friendly `HubServer` Root Response (`crates/cito-native/src/core/{daemon.rs,hub.rs}`)
- **Basis**: Prevent a Mac from connecting to its own shared `host_hub` as a duplicate remote Hub when the user clicks `Copy Invite Code` and then `Paste Invite Code` on the same machine, and prevent browsers from prompting to download an extensionless binary file when visiting `http://<host>:4291/` directly.
- **Self-Invite Guard in `DaemonService` (`crates/cito-native/src/core/daemon.rs:2591`)**:
  - In `"paste_hub_invite"`, before saving the Hub, check if `self.host_server.is_some()` and the invite URL's `(host, port)` matches this Mac's own active `host_hub` port and any local host identity (`endpoint_url`, `tailscale_url`, `lan_url`, `127.0.0.1`, `localhost`, `::1`, `local_host_name()`, or `<local_host_name>.local`). If it matches this Mac's own shared Hub, return `Err("INVALID_INPUT: This invite code belongs to this Mac's own shared Hub (already active as 'local'). Paste it in Cito on your other Mac to connect.".into())`.
- **Browser-Friendly `GET /` on `HubServer` (`crates/cito-native/src/core/hub.rs:2129`)**:
  - In `HubServer::handle_http`, when `method == "GET"` and `path == "/"`, return `HTTP/1.1 200 OK` with `Content-Type: text/html; charset=utf-8` and a concise HTML status page (`"Cito Hub is active on this Mac. Open Cito → Settings → Hubs & Network → Paste Invite Code on another Mac to connect."`) so opening the Hub URL in a browser never triggers an extensionless file download prompt.

### 18.2 Verification, Fast Single-Pass Review & No-Formal-Release Policy (`MUST`)

- Update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs}` in place; do **NOT** add new multi-process E2E test functions.
- Run a single-pass fast `contract-and-simplicity` review & fix (no multi-round loops) and push to `origin/main` without creating a release tag so the user can test immediately on macOS.

---

## 19. Stage 17: Complete Surface Audit Closure, `.destructiveHover` Primary Text Contrast, Intrinsic Button Sizing, Smart Path Compression & Titled-Panel Chinese IME (`R1`–`R7`)

### 19.1 Binding Contract (`MUST`)

#### 1. `R1` — `.destructiveHover` Resting Primary Text Color (`src/macos/cito-ui.swift:613–616`)
- **Basis**: In macOS HIG, dim gray text (`CitoPalette.secondaryText`) inside a bordered gray button bezel (`fill = labelColor @ 0.08, border = labelColor @ 0.14`) signifies a **Disabled (`isEnabled = false`)** control, making `Disconnect`, `Clear Token`, `Unpair`, `Remove`, and `Disconnect All…` look disabled next to `.neutral` buttons (`Re-check`, `Change Token…`, `Edit…`).
- **Contract**: In `MenuControl.applyVisualState()`, set `.destructiveHover`'s resting `foreground` to **`normalColor`** (`.labelColor` primary text, identical to `.neutral` at rest), transitioning on hover (`isHovered == true`) to `fill = CitoPalette.destructiveFill`, `border = .white.withAlphaComponent(0.26)`, `foreground = .white`.

#### 2. `R2` — Option A `Disconnect All…` Footer Button & 2-Button Confirmation Modal (`src/macos/cito-ui.swift:3595–3665`, `crates/cito-native/src/core/daemon.rs:3014`)
- **Basis**: Align the Settings footer button with the confirmation modal and remove the non-functional in-app `Confirm Uninstall` button (per user selection of Option A).
- **Contract**:
  1. Rename the Settings footer button (`open_uninstall_confirm`) from `"Remove All Integrations…"` to **`"Disconnect All…"`** (`width: MenuControl.intrinsicWidth("Disconnect All…", paddingX: 12)`).
  2. In `uninstall_confirm_box`:
     - Title (`12.5pt .semibold`): **`"Disconnect All Integrations"`**
     - Checkbox (`uninstall_purge_data`, `.subtleHeader`): **`"Also delete local data and Hub settings"`**
     - Bottom bar via `layoutBar(in: confirmBox, insetLeft: 12, insetRight: 12, y: 14, height: 24, leading: [cancelUninstallBtn], trailing: [disconnect])` with **two buttons only**:
       - Left: **`"Cancel"`** (`cancel_uninstall_all`, `.neutral`, `width: MenuControl.intrinsicWidth("Cancel")`)
       - Right: **`"Disconnect All"`** (`confirm_disconnect_all`, **`.destructive`**, `width: MenuControl.intrinsicWidth("Disconnect All")`, emitting `["op": "setup_disconnect_all", "purge_data": uninstallPurgeData]`).
     - Remove `confirm_uninstall_all` (`"Confirm Uninstall"`) from the GUI modal (`setup_uninstall_all` remains handled in `daemon.rs` for CLI compatibility).
  3. In `crates/cito-native/src/core/daemon.rs:3014` (`"setup_disconnect_all"`), when `action["purge_data"] == true`, also reset `config.json` (`defaultHub = "local"`, `hubs = {}`) and clear local state (`DELETE FROM local_signals` and `DELETE FROM local_listeners WHERE harness != 'human'`).

#### 3. `R3` — Eliminate All Remaining Hardcoded Button Widths & Ellipsis Truncation (`src/macos/cito-ui.swift:104–115, 2525, 3160–3250`)
- **Basis**: Residual hardcoded widths (`width: 96` on `Change Token…`, `width: 44` on `Edit…`, `width: 76` / `58` on `Send Test` / `Pause` / `Unpair`, and `width: 88` on `Settings…`) caused AppKit to render `"Change Token..."` and `"Edit..."` with truncation ellipses in Light Mode.
- **Contract**:
  - In `layoutBar` (`cito-ui.swift:107`), remove `minWidth: control.frame.width` or ensure every `MenuControl` in `Human Channels` (`edit_hub_service:<id>`, `clear_channel_service:<id>`, `pair_channel:<id>`, `edit_user_channel:<id>`, `test_channel:<id>`, `toggle_channel:<id>`, `unpair_channel:<id>`) and `cardMenu` (`open_settings` with `paddingX: 6`) uses `MenuControl.intrinsicWidth(title, font: font, leadingSymbol: symbol)` without shrinking below its intrinsic width. Delete the manual width overrides at `cito-ui.swift:3245–3247` (`testBtn.frame.size.width = 76; toggle.frame.size.width = 58; unpair.frame.size.width = 58`).

#### 4. `R4` — Clean State C (`Connected & Paired`) in `Human Channels` & `Share This Mac as a Hub` Header Padding (`src/macos/cito-ui.swift:3015–3100, 3270–3340`)
- **Basis**: Eliminate the orphan `"1."` prefix and redundant `"● Bot Ready"` badge when Telegram is already connected and paired, and give `Share This Mac as a Hub` balanced top margin when expanded.
- **Contract**:
  - In `Human Channels` (`settings_channel:<id>`): when `personalCollapsed == true` and `!hubGatewayExpanded` (both Gateway and Personal Account are active and collapsed), set `stratum_hub_title:<id>` to **`"Bot Gateway"`** (dropping `"1. Shared "`) and hide `stratum_hub_status:<id>` (`"● Bot Ready"`, since the card header right above it already displays `"● Connected (@username)"`).
  - In `Share This Mac as a Hub` (`host_hub_card`): when `hostExpanded == true`, place `"Share This Mac as a Hub"` at `y = hostHeight - 30.5` and `toggle_host_hub` via `layoutBar(in: hostCard, insetLeft: 14, insetRight: 14, y: hostHeight - 34, height: 24, trailing: [toggleHostBtn])` (`10pt` top margin instead of `6pt`).

#### 5. `R5` — Smart Path Compression in `AVAILABLE SESSIONS` & Left-Aligned `copy:<id>` Address Chip in `cardMenu` (`src/macos/cito-ui.swift:2240–2260, 2525–2545`)
- **Basis**: Preserve the complete project directory name (`<leaf>`) on Line 2 of `AVAILABLE SESSIONS` instead of middle-truncating inside the project name (`~/Documents/Codex/202...dynamics-com-en-dcd-2`), and align `❐ @tefx` flush left with `You (@tefx)` on active session cards.
- **Contract**:
  - On `copy:<scoped_id>` (`cito-ui.swift:2242`), set `copy.label.alignment = .left` and call `copy.layoutContents()` (with `startX = 0` when `label.alignment == .left` and `style == .subtleHeader` on `copy:` chips, or `x = titleX - 6`) so the copy icon and address align flush left with `title` at `x = titleX`.
  - Add `compactSessionPath(_ shortCwd: String, branchSuffix: String, maxWidth: CGFloat = 264, font: NSFont = .systemFont(ofSize: 10.5)) -> String`:
    - If `measureTextWidth(shortCwd + branchSuffix, font: font) <= maxWidth`, return `shortCwd + branchSuffix`.
    - Otherwise, split `shortCwd` into path segments: if there are `>= 3` segments (e.g. `["~", "Documents", "Codex", "2026-11-10-dynamics-com-en-dcd-2"]`), try `"~/…/\(segments.suffix(2).joined(separator: "/"))" + branchSuffix` first, and if still `> maxWidth`, use `"~/…/\(segments.last!)" + branchSuffix` (falling back to `NSTextField` truncation with `toolTip = "\(shortCwd)\(branchSuffix)"` only when `<leaf> + branchSuffix` alone exceeds `maxWidth`).

#### 6. `R6` — `Directory` Initial Peer Seeding, `onlineDot` Removal & `Activity` Dynamic Row Height + `xmark` Delete Icon (`src/macos/cito-ui.swift:950–1160, 1820, 3960, 4410`)
- **Basis**: Prevent `Directory` from flashing `"No active listeners"` before the async `"directory"` frame arrives, remove the overlapping corner `onlineDot` on `Directory` icons, eliminate the `46pt` dead empty space on 1-line selected `Activity` rows, and avoid duplicating the `🗑` trash icon on every `Activity` row.
- **Contract**:
  - Remove `onlineDot` from `DirectoryPeerCellView`.
  - In `receive` (when `frame.op == "state"`) and `openDirectoryPanel`: if `directoryAllPeers.isEmpty`, seed `directoryAllPeers = listeners + myRemote` and call `applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)`.
  - In `Activity` (`DirectoryPeerCellView.configureActivity` and `tableView(_:heightOfRow:)`):
    - Change the multi-line expansion condition from `measureTextWidth(text, font: .systemFont(ofSize: 11.5)) > 330` to **`measureTextWidth(text, font: .systemFont(ofSize: 12)) > 480 || text.contains("\n")`**, and compute the expanded row height dynamically from the wrapped text height (`min(108, max(60, 38 + ceil(wrappedRect.height)))`) so a 1-line selected message stays at `60pt` with zero empty space below it.
    - Change `deleteControl` (`delete_activity:<signal_id>`) symbol from `"trash"` to **`"xmark"`**.

#### 7. `R7` — Titled Full-Size Content `KeyPanel` for 100% Native Chinese IME Candidate Windows (`src/macos/cito-ui.swift:395–430, 2675, 3700`)
- **Basis**: Ensure macOS Text Services Manager (`TSM`) always attaches floating Pinyin candidate windows to editable text fields inside `settingsPanel` and `directoryPanel`.
- **Contract**:
  - Initialize `KeyPanel` (`settingsPanel` and `directoryPanel`) with `styleMask: [.titled, .fullSizeContentView]`, setting `titleVisibility = .hidden`, `titlebarAppearsTransparent = true`, and hiding `.closeButton`, `.miniaturizeButton`, and `.zoomButton`.
  - In non-headless mode (`!headless`), set `NSApp.setActivationPolicy(.regular)` when opening `settingsPanel` or `directoryPanel`, and restore `NSApp.setActivationPolicy(.accessory)` when both `settingsVisible == false && directoryVisible == false`.

### 19.2 Verification & Convergent `contract-and-simplicity` Review-Fix Loop Policy (`MUST`)

- Update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs}` in place; do **NOT** add new multi-process E2E test functions.
- Regenerate all 7 dark-mode website screenshots (`site/assets/screenshots/*.png`) and verify both Light and Dark mode renders visually.
- Run a **convergent `contract-and-simplicity` review-and-fix loop** until both `Contract & Wiring: CLEAN` and `Simplicity & Economy: ZERO_NEW_ISSUES` converge, and push to `origin/main` (without creating a formal release tag yet).

---

## 20. Stage 18: Multi-Hub Card & Directory Coalescing, Smart Home-Directory Naming, Inline/CLI/MCP Session Renaming & Complete CLI/MCP Surface Parity (`S1`–`S6`)

### 20.1 Binding Contract (`MUST`)

#### 1. `S1` — Multi-Hub Session Coalescing & Fixed-Stop Secondary Alignment in `cardMenu` (`src/macos/cito-ui.swift:2220–2375`, Questions 1 & 2)
- **Basis**: When a second Hub (`tefx-mbp-personal`) is connected alongside `local`, `listeners` contains one row per `(hub, listener_id)`, causing `cardMenu` to render **two `You (@tefx)` cards** (`local:@tefx` and `@tefx`) and **two cards for the same local agent session `pi#3368`** (`local:tefx/pi#3368` and `tefx/pi#3368`) even though the header says `"1 session listening"`. Furthermore, placing `path` (`"Desktop notifications"` or `cwd · branch`) at `x = titleX + copyWidth + 8` shifted it horizontally whenever `copyWidth` varied across cards.
- **1. Coalesce Local Listeners by `(harness, session_id)` in `cardMenu` Zone 1**:
  - Group `orderedListeners` by `sessionKey = "\(listener.harness):\(listener.session_id)"` so `cardMenu` renders **at most 1 card for `You (@tefx)` (`human:default`)** and **1 card per local coding session (`(harness, session_id)`)**, matching `uniqueSessionCount`.
  - **Primary Representative Selection (`primaryListener`)**: Within each `(harness, session_id)` group, select the representative row whose `hub == effectiveTargetHub` (or the default connected shared Hub when `hubs.count > 1`, falling back to the first connected Hub or first row). Use `primaryListener.scoped_id` and `primaryListener.display_address` for the card's `session:<scoped_id>` and `copy:<scoped_id>` identifiers so unprefixed default-Hub addresses (`@tefx`, `tefx/pi#3368`) are shown by default and existing single-Hub/target-Hub tests remain 100% compatible.
  - **Multi-Hub Action & Target-Hub Selection on Coalesced Cards**:
    - Within each `(harness, session_id)` group, choose `primaryListener` by preferring the explicitly selected enrolled Hub (`selectedTargetHub`), including a temporarily disconnected Hub, then `effectiveTargetHub` when a target was explicitly selected, then the connected default non-`local` Hub, then `"local"`.
    - Clicking `Pause` (`stop:<scoped_id>` on `human`) or `×` (`stop:<scoped_id>` on an agent session) unlistens all active registrations in that `(harness, session_id)` group across **all enrolled Hubs** (persisting local revocation in `local_listeners` regardless of Hub connectivity so the card disappears in one click and does not resurrect on reconnect).
    - Clicking `Team / Self` (`scope:<scoped_id>`) updates `scope` across all non-local Hubs in the group.
    - On agent session cards when `hubs.count > 1`: `switch_hub:<scoped_id>` keeps its existing single-registration switch behavior when the session is only on 1 Hub (`group.count == 1`), and when `group.count > 1` it displays `primaryListener.hub` and cycles `selectedTargetHub` across the session's enrolled Hubs so the user can inspect or copy any Hub's scoped address on demand.
- **2. Eliminate `"Desktop notifications"` Boilerplate on `You (@tefx)` Cards**:
  - On the `You (@tefx)` card (`listener.harness == "human"`): omit the static `"Desktop notifications"` label when `channels` only contains `"Mac"` / `"Desktop notifications"` / empty (`path.isHidden = true`), showing only external connected channels (such as `"Telegram"`) when present. Keep `path` (`cwd · branch`) on agent session cards at `x = titleX + copyWidth + 8` so it retains maximum horizontal width without truncation.

#### 2. `S2` — Smart Home-Directory Session Naming, `.local` Hostname Stripping & Shadow Deduplication in `Directory` (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/{daemon.rs,registration.rs}`, Question 3)
- **Basis**: When a coding session is started in `$HOME` (`/Users/tefx`), `basename(cwd)` is `"tefx"` (identical to the username `@tefx`), causing 3 agent rows in `Directory` and `cardMenu` to be titled `"tefx"`. In addition, `DirectoryPeerCellView` did not strip `.local` from `peer.channels`, and `local:` loopback registrations duplicated shared-Hub rows for the same local session.
- **1. Never Use `$HOME` Username as an Auto-Derived Agent Session Title (`nodeDisplayTitle` & `daemon.rs` / `registration.rs`)**:
  - Explicit custom titles (`S3.1`, `1..128` chars) bypass all automatic-title heuristics verbatim across registration, discovery, and GUI rendering.
  - For auto-derived titles (when no custom title is set for `(harness, session_id)`), apply the fallback only when the trimmed raw title is empty, matches the username or `$HOME` basename (with or without an `@` prefix) case-insensitively, or is a generic placeholder (`agent`, `agent session`, `session`, `cli session`, `human`, `human node`, `human channel`):
    - Use the same generic-label classification to reject `about` boilerplate, including the username and `$HOME` basename with or without `@`. If `let distinct = distinctAbout(about, title: raw, user: user)` is non-empty, return **`distinct`**.
    - Otherwise, return **`"\(harnessName ?? harness) (~)"`** (e.g., **`"Pi Coding Agent (~)"`** or **`"Codex (~)"`**) instead of the bare username.
  - Apply the same rule in `crates/cito-native/src/core/daemon.rs` (`default_session_title` / `listen` fallback). Preserve meaningful existing titles regardless of `cwd`, including a previously promoted title equal to `about`; rendering `title == about == "Auth refactor"` at home must remain `"Auth refactor"` without a duplicate description.
- **2. Strip `.local` & Internal Channel Tokens Everywhere (`cleanHostDisplay`)**:
  - Add helper `cleanHostDisplay(_ raw: String, localHost: String? = nil) -> String` in `cito-ui.swift` that strips `.local` case-insensitively (`replacingOccurrences(of: "\\.local\\b", with: "", options: [.regularExpression, .caseInsensitive])`).
  - In `DirectoryPeerCellView.configure(with:)` (`cito-ui.swift:1107`), filter out internal tokens (`"Mac"`, `"Hub"`, `"human"`, `"Desktop notifications"`) from `peer.channels`, apply `cleanHostDisplay` to every hostname in `peer.host_name` and `peer.channels`, and deduplicate while preserving order so `"Zhaomengs-MacBook-Air.local, Zhaomengs-MacBook-Pro-16.local"` renders cleanly as **`"Zhaomengs-MacBook-Air, Zhaomengs-MacBook-Pro-16"`**.
  - When `peer.repo` is empty on an agent row in `Directory`, display `shortCwd` (` · ~` or ` · ~/Projects/cito`) in `line1RepoLabel` so Line 1 always provides workspace context.
- **3. Deduplicate `local:` Loopback Shadow Peers in `Directory`**:
  - When building `directoryFilteredPeers` in `cito-ui.swift` (and in `daemon.rs` `directory_peers` when `hub` filter is not explicitly `"local"` and at least one non-`local` Hub is connected): if a peer on `"local"` has the exact same `(user, harness, session_id, host_id)` (or `(user, "human")` for self) as a peer already present on a connected shared Hub, omit the redundant `local:` shadow entry so each real session and person appears **once** with its shared address (`tefx/pi#3368`, `@tefx`).

#### 3. `S3` — Manual Agent / Session Renaming Across GUI, CLI, and MCP (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/{daemon.rs,cli.rs,registration.rs,session_activity.rs}`, Question 4)
- **Basis**: Allow users and agents to give any session a clear, durable custom name (e.g., `"Cito UI Audit"`, `"Backend API"`) that persists across heartbeats and discovery refreshes without cluttering the GUI.
- **1. Durable Custom Title Persistence (`custom_title` / `title` Priority)**:
  - In `crates/cito-native/src/core/registration.rs` (`listen`) and `daemon.rs`:
    - Accept optional `"title"` (`1..128` chars) in `cito_listen` arguments and `"rename_session"` UI/CLI control action (`{"op": "rename_session", "harness": ..., "session_id": ..., "listener_id"?: ..., "title": ...}`).
    - When a user or agent explicitly sets `title` via `rename_session`, `cito rename`, or `cito_listen(title: ...)`, persist it in `local_listeners.title` (and `hub_kv` registration `title`) and record it in an in-memory/SQLite custom-title map (`session_custom_titles` keyed by `(harness, session_id)`) so `session_activity.rs` discovery refreshes and automatic heartbeat re-registrations **never overwrite a user/agent-assigned title** with the auto-discovered transcript/folder title!
- **2. Minimalist Zero-Clutter GUI Inline Rename (`cardMenu` & `Directory`)**:
  - On each active agent session card in `cardMenu` (`cito-ui.swift:2260`):
    - Lay out right-hand `sessionActions` first via `layoutBar(in: card, insetLeft: titleX, y: cardHeight - 28, height: 22, spacing: 2, trailing: sessionActions)` to establish `rightEdge = sessionActions.map(\.frame.minX).min() ?? (card.bounds.width - CitoMetrics.cardInset)`.
    - When `renamingSessionKey == "\(listener.harness):\(listener.session_id)"`, hide `title` and `renameBtn` and place an editable `NSTextField` **`rename_field:<scoped_id>`** at `NSRect(x: titleX, y: cardHeight - 27, width: max(80, rightEdge - titleX - 6), height: 20)` pre-filled with the current title.
    - Otherwise, size `title` to `titleW = max(24, min(rightEdge - titleX - 24, ceil(measureTextWidth(displayTitle, font: .systemFont(ofSize: 13, weight: .semibold))) + 4))` at `NSRect(x: titleX, y: cardHeight - 26, width: titleW, height: 18)`, and place a compact `18 × 18 pt` `.subtleHeader` icon button **`rename:<scoped_id>`** (`symbol: "pencil"`, tooltip `"Rename session"`) at `NSRect(x: titleX + titleW + 2, y: cardHeight - 26, width: 18, height: 18)` so `title`, `rename:<scoped_id>`, and `sessionActions` never overlap (`overlapping_siblings == []`).
    - Pressing **`Return`** inside `rename_field:<scoped_id>` emits `["op": "rename_session", "harness": listener.harness, "session_id": listener.session_id, "title": newTitle]`, clears `renamingSessionKey`, and updates the card and Hub registrations immediately. Pressing **`Escape`** cancels inline rename.

#### 4. `S4` — Complete CLI Interface & Security-Bounded MCP Surface (`crates/cito-native/src/{main.rs,core/cli.rs,core/setup.rs,core/daemon.rs}`, Questions 5 & 6)
- **Basis**: Provide 100% CLI parity with GUI capabilities for terminal/SSH workflows without redundant subcommand aliases, while expanding MCP strictly within the **Session Self-Determination + Read-Only Status** security boundary.
- **1. Complete CLI Surface (`cito`)**:
  - **`cito rename <title> [--harness <id> [--session <sid>]] [--pi[=<sid>]] [--codex[=<sid>]] [--claude[=<sid>]] [--json]`**:
    - Top-level subcommand in `main.rs` / `cli.rs` that renames the current or specified local session across all enrolled Hubs (`rename_session`).
  - **`cito listen [--title <title>] [--scope <self|workspace>] [--team] [--hub <hub>] [--harness <id> [--session <sid>]] [about...]`**:
    - Support `--title <title>` (or `-t <title>`) on `cito listen`. When `--title` is omitted and positional `[about...]` is provided on a session whose default title would be the generic folder/home basename, also promote `about` to `title`.
  - **`cito unlisten [--all] [--hub <hub>] [--harness <id> [--session <sid>]] [--json]`**:
    - Add `--all` to `cito unlisten` (calling `unlisten_all`, matching GUI `Stop All`).
  - **`cito status [--json]`**:
    - Top-level subcommand in `main.rs` / `cli.rs` that queries the local daemon snapshot and prints active listeners, available sessions (`AVAILABLE SESSIONS`), and connected Hubs (`--json` outputs the structured snapshot).
  - **`cito setup` Parity Flags (`crates/cito-native/src/core/setup.rs`)**:
    - `--invite <invite-code-or-url>`: parses and joins a Hub invite code/URL from the CLI (matching GUI `Paste Invite Code`, including the self-invite loop guard).
    - `--print-invite`: prints this Mac's current `host_hub` invite command when Hub sharing is enabled (matching GUI `Copy Invite Code`).
    - `--disconnect-all [--purge-data]`: disconnects all Agent App integrations while keeping the daemon running (matching GUI `Disconnect All…`).
- **2. Security-Bounded MCP Surface (`cito-mcp`, `crates/cito-native/src/core/daemon.rs:3455–3610`)**:
  - **Enhance `cito_listen`**: Add optional `"title": {"type": "string", "minLength": 1, "maxLength": 128}` to `cito_listen` `inputSchema` and `validate_tool_input`, allowing an agent session to set or update its own display title at any time (`cito_listen({ title: "Auth API Refactor", about: "..." })`).
  - **Add Read-Only `cito_status` MCP Tool**:
    - `{"name": "cito_status", "description": "Inspect this session's current Cito registration, address, connected hubs, and unread inbox count.", "inputSchema": {"type": "object", "properties": {"hub": {"type": "string"}}, "additionalProperties": false}}`
    - Returns sanitized, secret-free metadata for the calling session: `{"ok": true, "registered": bool, "display_address": ..., "title": ..., "about": ..., "scope": ..., "unread_count": ..., "hubs": [{"name": ..., "status": ..., "is_default": ...}]}` (**never** returning `token`, `bot_token`, `chat_id`, or other credentials).
  - **Explicit MCP Security Exclusions**: Administrative mutations (`setup`, `join_hub`, `share_hub`, `unlisten_all`, `channel_save`, `disconnect_all`, and cross-session `rename`) MUST remain strictly CLI/GUI-only.

### 20.2 Verification & Convergent `contract-and-simplicity` Review-Fix Loop Policy (`MUST`)

- Update existing unit/UI/daemon tests in `crates/cito-native/tests/support/{ui.rs,daemon.rs,cli.rs}` in place to cover `S1`–`S4`; do **NOT** add new heavy multi-process E2E test functions.
- Run a **convergent `contract-and-simplicity` review-and-fix loop** until both `Contract & Wiring: CLEAN` and `Simplicity & Economy: ZERO_NEW_ISSUES` converge, and push to `origin/main` (without creating a formal release tag yet).

---

## 21. Stage 19: Network-Scoped View Management, Trusted-Hub Auto-Listen, Scalable Session Grouping, Fast Codex SQLite Discovery & UI Polish (`N1`–`N6`)

### 21.1 Binding Contract (`MUST`)

#### 1. `N1` — Strictly Network-Scoped Management in `cardMenu` & Hiding Administratively Disabled Hubs (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`, Questions 3, 5, 7, 10)
- **Basis**: Trying to merge multiple independent Hubs (`local` + `tefx-mbp-personal`) onto one flat card forced a truncated `[ ● tefx-...rsonal ]` switcher pill onto every session card while also repeating a second `[ local ] [ tefx-mbp-personal ]` strip above `AVAILABLE SESSIONS`. Meanwhile, administratively disabled Hubs (`enabled == false`) continued to appear in `cardMenu` (`Offline · Disconnected [ Connect ]`) and `Human Channels` (`Target Hub`, producing a blank page when selected).
- **1. Hide Administratively Disabled Hubs (`enabled == false`) from Operational Views (`Questions 3 & 5`)**:
  - In `DaemonService::ui_state()` (`crates/cito-native/src/core/daemon.rs:2002`): include only **enabled** Hubs (`self.slots`, where `enabled != false`) in the operational `state.hubs` array (disabled Hubs remain in `settings.hubs` so they can be managed in `Settings → Hubs & Network`).
  - In `Settings → Human Channels` (`src/macos/cito-ui.swift:3080`): filter `channelHubs` to `settings?.hubs.filter { $0.enabled != false }.map { $0.name } ?? hubs.filter { $0.enabled != false }.map { $0.name }`, and if `selectedChannelHub` points to a disabled Hub, fall back to the first enabled Hub so selecting a disabled Hub never renders a blank page. Also in `daemon.rs:2053` (`settings_for_hub`), if `channel_hub == "local"` and `"local"` is not in `self.slots`, still query `self.local.rpc("human_channels_list", &json!({}), None)` so `"local"`'s channel inventory is never empty.
- **2. Top Header Hub Strip as the Single Active Network Switcher (`Questions 7 & 10`)**:
  - In `cardMenu` (`src/macos/cito-ui.swift:1257, 2080–2660`):
    - **Single Network Scope (`effectiveTargetHub`)**: Update `effectiveTargetHub` so that if `!selectedTargetHub.isEmpty && hubs.contains(where: { $0.name == selectedTargetHub })`, it returns `selectedTargetHub` (even if temporarily disconnected), and falls back to the default/first connected Hub or `hubs.first?.name ?? ""`. When `hubs.count > 1`, clicking `hub:<name>` in `header_hub_strip` always sets `selectedTargetHub = hub.name` and rebuilds `cardMenu` (and if `hub.status != "connected" || hub.error != nil`, also toggles `drawer:<name>`). Highlight the selected Hub pill (`hub.name == effectiveTargetHub`) in `header_hub_strip`.
    - **Scope All Menu Zones to `effectiveTargetHub`**:
      - Filter `listeners` in Zone 1 (`You (@tefx)` and active agent sessions) to rows where `listener.hub == effectiveTargetHub` (when `!effectiveTargetHub.isEmpty`) so every card in `cardMenu` belongs unambiguously to the currently selected network!
      - Filter `myRemote` in Zone 2 (`MY OTHER HOSTS`) to `peer.hub == nil || peer.hub == effectiveTargetHub`.
      - **Remove Per-Card `switch_hub:<scoped_id>` Pills** from active session cards in Zone 1 (freeing `~95pt` of horizontal space on every session card so titles and buttons never collide or truncate as `tefx-...rsonal`!).
      - **Listen on the Selected Network**: Always include `effectiveTargetHub` in `listen_session`, including when only one Hub is enabled and the configured default is disabled. Disable Listen when no Hub is enabled.
      - **Remove Duplicate `targetStrip`** on `AVAILABLE SESSIONS` (and allow `test_control` with `"target_hub:<name>"` to alias `"hub:<name>"` so any existing test selecting `target_hub:<name>` switches `selectedTargetHub` via the top header switcher!).

#### 2. `N2` — Zero-Friction `Auto-Listen` on `local` / Trusted Networks & Bounded Scrollable Session List (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`, Question 9)
- **Basis**: On `local` and personal/trusted Hubs (`auth_mode == "none"` or `token`), requiring the user to manually click `+ Listen` for every open coding session is unnecessary friction; and when many agents are active, the active session list must stay compact and scrollable.
- **1. Automatic Enrollment of Open Live Sessions (`auto_listen`, `daemon.rs`)**:
  - Persist `auto_listen: bool` per Hub in `config.json` (`hubs.<name>.autoListen`, exposed in `state.hubs[].auto_listen` and `settings.hubs[].auto_listen`, defaulting to `false` when unset in `config.json` so existing unit tests are unaffected).
  - Add a compact pill control **`toggle_auto_listen`** (`autoListen ? "Auto-Listen: On" : "Auto-Listen"`, style `autoListen ? .prominent : .neutral`, `height: 18, radius: 9`) on the right side of the `AVAILABLE SESSIONS` header row in `cardMenu` that emits `["op": "set_auto_listen", "hub": effectiveTargetHub, "enabled": !autoListen]`.
  - When `auto_listen == true` for a connected Hub `H`:
    - During `DaemonService::poll()` (and immediately when `set_auto_listen` enables it), for every discovered candidate session in `session_activity` that has a **live process (`endpoint_pid.is_some()` and `needs_reload != true`)**, is not already `ACTIVE` on `H`, and was not explicitly muted/unlistened by the user (`status != "UNLISTENED"` for `(H, harness, session_id)`), automatically call `self.listen(H, &json!({"about": candidate["title"], "scope": "self"}), &candidate)`!
    - When the user clicks `×` (`stop:<scoped_id>`) on an active session, `unlisten_id` sets `status = 'UNLISTENED'` in `local_listeners`, preventing `auto_listen` from re-enrolling that session until the user explicitly clicks `+ Listen`.
- **2. Bounded Scrollable Active Session List**:
  - Keep standard 2-line active cards at `cardHeight = 58` (`rowHeight = 64`, preserving `>= 6pt` vertical separation between `title` and `copy` and `>= 10pt` badge padding), and cap `activeRows` at `4` visible rows (`256pt` `OverlayScrollView` `active_scroll` when `activeRows.count > 4`) so 5–15 active sessions remain compact and scrollable without dominating the screen.

#### 3. `N3` — Fast `1.5ms` Bounded Codex SQLite Discovery & Remote/CLI Process Detection (`plugins/codex/native.rs`, `crates/cito-native/src/core/daemon.rs`, Question 1)
- **Basis**: `~/.codex/state_5.sqlite` on a real developer Mac is **209 MB with 5,111 rows**. `plugins/codex/native.rs:193` ran `SELECT * FROM threads WHERE archived = 0 ORDER BY ...` **without a `LIMIT`**, serializing all 5,101 full rows from SQLite and taking `330ms–2000ms` (hitting the `1500ms` `DISCOVERY_TIMEOUT`), causing Codex sessions to intermittently disappear! In addition, `desktop_command` ignored `codex` CLI / `codex app-server` processes.
- **Contract (`plugins/codex/native.rs:21–40, 175–215`)**:
  1. In `rows(req)` (`plugins/codex/native.rs`), select only available metadata columns and apply thread eligibility predicates in SQL **before `LIMIT 32`** when `req["op"] == "discover_sessions"`. Exclude archived threads, missing or empty IDs/working directories, non-user `thread_source` values, and subagent `source` values; optional source columns and NULL source values remain supported. This bounds serialization to the 32 most recent eligible metadata rows without reading transcript/turn blobs into Rust. Caller resolution remains unbounded, and `unlisten` can still resolve archived threads. The discovery latency target remains `< 2ms`; query projection and limits alone do not establish that timing on every database.
  2. In `desktop_running(req)` (`plugins/codex/native.rs:28`), accept any running process owned by `uid` that matches **`desktop_command(cmd) || matches_process(cmd)`** (including `codex` CLI and `codex app-server`) so Codex sessions are discovered whether the user is running `ChatGPT.app`, `Codex.app`, or `codex` CLI / `app-server`!
  3. In `DaemonService::action_inner` (`"setup_apply"` / `"refresh_available"`, `daemon.rs:2579, 3322`), after calling `activity.refresh_discovery()`, immediately run `activity.tick(&self.state, &live)` and briefly poll up to `400ms` for fast discovery workers to finish before calling `self.push_ui()` so newly enabled apps' sessions appear in `cardMenu` on the very first render!

#### 4. `N4` — Per-App Isolated `setup_apply` Staging & Daemon Startup Auto-Staging (`crates/cito-native/src/core/{setup.rs,daemon.rs}`, Question 4)
- **Basis**: Clicking `Repair` on `Pi Coding Agent` called `ensure_cli_shims_native()`, which re-staged **all** built-in plugins (`pi`, `codex`, `claude`, `telegram`) at once, causing `Codex`'s `Repair` button to unexpectedly flip to `Connected` before the user clicked it.
- **Contract**:
  1. Add `ensure_single_plugin_staged(state_dir: &Path, plugin: &PluginDescriptor)` in `setup.rs` (staging only `plugin.id` and the shared `bin/cito` + `bin/cito-mcp` shims), and call it inside `"setup_apply"` (`daemon.rs:2576`) instead of re-staging all other agent plugins.
  2. At `DaemonService::new` startup (`daemon.rs`), call `ensure_cli_shims_native(&config.state_dir, plugins)` automatically so upgrading or reinstalling `Cito.app` self-heals `~/.cito/runtime/plugins/*` on launch without showing false `⚠ Repair Needed` states.

#### 5. `N5` — Uniform `Team / Self` Toggle Geometry, Action-Verb Hub Buttons & Unread-Only Menu Bar Badge (`src/macos/cito-ui.swift`, Questions 2, 6, 8)
- **Basis**:
  - **Question 2**: Clicking `scope:<scoped_id>` (`Team` / `Self`) called `setLeadingSymbol` (which runs `layoutContents()`) *before* updating `label.stringValue`, and used `spacing: 2` (`2pt`) between `Team/Self` and `Pause`.
  - **Question 6**: In `Settings → Hubs & Network`, `[ Edit ] [ Enabled ] [ Remove ]` mixed a state adjective (`Enabled`) with action verbs (`Edit`, `Remove`).
  - **Question 8**: `formatStatusButton` displayed `uniqueSessionCount` (`1`) next to the macOS menu bar icon, which looks like `"1 unread notification"` even when `unreadCount == 0`.
- **Contract (`src/macos/cito-ui.swift`)**:
  1. **Uniform `Team / Self` Geometry (`Question 2`, `cito-ui.swift:2360–2400`)**:
     - Size `scopeBtn` to a fixed uniform width `scopeWidth = max(MenuControl.intrinsicWidth("Self", leadingSymbol: "lock.fill"), MenuControl.intrinsicWidth("Team", leadingSymbol: "person.2.fill"))` (`68pt`), and in `layoutBar` preserve `scopeBtn`'s fixed `68pt` width so switching between `Self` and `Team` never shifts adjacent controls by even `1px`!
     - In `scopeBtn.pressed`, update `scopeBtn?.label.stringValue` **before** calling `scopeBtn?.setLeadingSymbol(...)`, and use standard `spacing: CitoMetrics.gap` (`6pt`) in `layoutBar(in: card, ..., trailing: sessionActions)`.
  2. **Action-Verb Hub Buttons (`Question 6`, `cito-ui.swift:3490`)**:
     - On each Hub card in `Settings → Hubs & Network` (`toggle_hub:<name>`), change the button label from `"Enabled"` / `"Disabled"` to the action verb **`"Disable"`** (when `hub.enabled == true`, `.neutral`) and **`"Enable"`** (when `hub.enabled == false`, `.prominent`), matching the action verbs `"Edit"` and `"Remove"`.
  3. **Unread-Only Menu Bar Number (`Question 8`, `cito-ui.swift:89`)**:
     - In `formatStatusButton(_ button: NSButton, unreadCount: Int, image: NSImage?)`, display a numeric badge next to the menu bar icon **only when `unreadCount > 0`** (`let title = unreadCount > 0 ? " \(unreadCount)" : ""`). When `unreadCount == 0`, show only the Cito icon (`makeCitoStatusIcon` already distinguishes `.active` vs `.idle` vs `.warning`).

### 21.2 Verification, Fast Single-Pass `contract-and-simplicity` Review & No-Release Policy (`MUST`)

- Update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs}` in place; do **NOT** add new heavy multi-process E2E test functions.
- Run a **fast single-pass `contract-and-simplicity` review & fix** (no multi-round loops), verify `cargo test --workspace && cargo build --release`, and push to `origin/main` without creating a release tag so the user can immediately install and use it on both Macs.

---

## 22. Stage 20: First-Principles Unified Session Model, Recessed Segmented Controls, Hub-Level Visibility Policy & SQLite Busy-Timeout Resilience (`U1`–`U5`)

### 22.1 Binding Contract (`MUST`)

#### 1. `U1` — Fixed-Height `You (@tefx)` Card & Automatic Human Enrollment Across All Enabled Hubs (`src/macos/cito-ui.swift:2225–2270`, `crates/cito-native/src/core/daemon.rs:790, 1450`, Questions 1 & 3)
- **Basis**: Active `You (@tefx)` rendered with `rowHeight = 64, cardHeight = 58`, whereas paused `You (@tefx)` (`human_paused_card`) rendered with `rowHeight = 56, cardHeight = 50`, causing an `8pt` height jump when switching networks. Furthermore, `register_automatic_human` only auto-enrolled `human` on `default_hub` (or left `local` un-enrolled), making `local` display `"Notifications paused"` even though the user never paused it.
- **Contract**:
  1. **Unified `58pt` Card Height**: Set `human_paused_card` (`cito-ui.swift:2232`) to the exact same outer geometry (`rowHeight = 64, cardHeight = 58`, badge at `x = 10, y = 14, width = 28, height = 28`, title at `y = 32`, subtitle `"Notifications paused"` at `y = 10`, and `[ Resume ]` button at `y = 18`) as the active `You (@tefx)` card so switching networks or toggling `Pause` ↔ `Resume` produces **0pt vertical shift**!
  2. **Automatic Human Enrollment on All Enabled Non-OIDC Hubs (`daemon.rs`)**: In `register_automatic_human` / `reload_slots`, auto-enroll the `human` listener across **every enabled Hub** (unless that Hub's `human` row was explicitly set to `UNLISTENED` by the user or requires unauthenticated OIDC login), so switching between `tefx-mbp-personal` and `local` shows `You (@tefx)` active on both networks by default.

#### 2. `U2` — True macOS HIG Recessed Segmented Tracks for Network & Filter Switchers (`src/macos/cito-ui.swift:2125–2185, 3915–3975`, Question 2)
- **Basis**: In macOS HIG, mutually exclusive view/network switchers and scope-bar filter pills must be visually distinct from momentary push buttons (`.neutral`). Drawing unselected Hub pills (`[ ● local ]`) and unselected `Directory` filter pills (`My Macs`, `Pi`, `Codex`) with `.neutral` button borders made them look like action buttons.
- **Contract (`src/macos/cito-ui.swift`)**:
  1. **Recessed Segmented Track for `header_hub_strip` in `cardMenu`**:
     - When `hubs.count > 1`, style `header_hub_strip` (`height: 22`, right-aligned at `maxX = 368` so `strip_anchor == []`) with a recessed background (`NSColor.labelColor.withAppearanceAlpha(0.06)`, `cornerRadius = 11`). Use native scroll-view background drawing so the track renders and resolves its semantic color in both appearances.
     - Inside `header_hub_strip`, style the selected Hub pill (`hub.name == effectiveTargetHub`) as `.prominent` (when connected and error-free) or `.neutral` (when showing an error/disconnected state), and style **unselected** Hub pills (`hub.name != effectiveTargetHub`) as **`.subtleHeader` (borderless transparent at rest, subtle hover fill)** so the switcher is unmistakably a macOS Segmented Control!
  2. **Scope-Bar Styling for `Directory` / `Activity` Filter Pills (`updateDirectoryFacetStyles`, `cito-ui.swift:3955`)**:
     - For filter pills in `directoryFacetBar` (`dir_scope:*`, `dir_harness:*`, `act_filter:*`), style the active filter pill as `.prominent` and **unselected filter pills as `.subtleHeader` (borderless at rest)**, while keeping right-hand action buttons (`✓ Read All`, `🗑 Clear`) as `.neutral` bordered push buttons.
  3. **Settings Switchers**:
     - In Human Channels, place `channel_hub_selector:<name>` pills inside `channel_hub_strip`, using the same appearance-aware recessed fill (`labelColor` at `0.06`) and `11pt` corners. Keep the selected Hub `.prominent` and unselected Hubs `.subtleHeader`.
     - In the Add / Edit Hub form, group `settings_hub_auth:none|token|oidc` (`Trusted Network`, `Shared Token`, `Team Sign-In`) inside `settings_hub_auth_track`, using a recessed `labelColor` fill at `0.05` and `7pt` corners. Keep the selected mode `.prominent` and unselected modes `.subtleHeader`. A borderless track may use compact padding; bordered tracks require at least `6pt` child padding. Preserve empty `clipped_views` and `overlapping_siblings` results.

#### 3. `U3` — Unified Action Vocabulary (`Pause` / `Resume` / `Listen`) & Deterministic `Auto-Listen` Feedback (`src/macos/cito-ui.swift:2350–2660`, `crates/cito-native/src/core/daemon.rs`, Questions 3, 5, 6, 7)
- **Basis**:
  - Clicking `Auto-Listen` when open sessions required `/reload` (`needs_reload == true`) or when only historical SQLite threads (`endpoint_pid == nil`) existed produced zero visible feedback (`Question 5`).
  - Remembering hidden `UNLISTENED` state per session while `Auto-Listen: On` is active created invisible magic that users cannot see or remember after a few clicks (`Question 6`).
  - `You (@tefx)` used `Pause` / `Resume` while Agent sessions used `×` / `+ Listen` (`Question 3`).
- **Contract**:
  1. **Eliminate Hidden Per-Session Exception Memory Under `Auto-Listen` (`Question 6`)**:
     - Make `Auto-Listen` (`auto_listen`) a clean, stateless mode switch on the Hub:
       - **When `Auto-Listen` is toggled ON (`set_auto_listen` with `enabled: true`)**: Clear any prior `UNLISTENED` markers for agent sessions on that Hub and immediately enroll all eligible open sessions (`endpoint_pid != nil && needs_reload != true`).
       - **When a user manually clicks `Pause` (`stop:<scoped_id>`) on an agent session while `Auto-Listen` is ON**: Automatically turn `auto_listen = false` on that Hub (switching to manual selection mode) and unlisten that session—so there is **never** a hidden contradiction where `Auto-Listen: On` is lit while an open session sits un-enrolled below!
       - **Visible Inline Feedback When `Auto-Listen: On` (`Question 5`)**:
         - When `Auto-Listen: On` is active on `effectiveTargetHub`:
           - If `visibleAvailable` contains sessions that need `/reload` (`needs_reload == true`), display a clear status caption in `AVAILABLE SESSIONS`: `"Auto-Listen is on — run /reload below to finish connecting"`.
           - When all open sessions are already connected and only historical `recent` threads (`endpoint_pid == nil`) remain: seed `collapsedGroups` once (via `autoCollapsedRecentGroups: Set<String>`) for `recent`-only harnesses when `auto_listen == true` so historical threads collapse to a single **`▸ Codex · 8 recent`** header row by default (while still expanding normally when the user clicks `toggle_group:<harness>`, `Question 7`)!
           - When `visibleAvailable.isEmpty`, show `"Auto-Listen is on — open sessions connect automatically"` (`available_empty_target`).
  2. **Consistent `Pause` / `Resume` Labeling (`Question 3`)**:
     - On `human_paused_card`, label the button **`"Resume"`** (without a `+` prefix, matching `"Pause"`).
     - On every active Human and agent session card, render `stop:<scoped_id>` as the same neutral `"Pause"` pill (`22pt` high, intrinsic text width), without an icon-only `×`. Keep the Human tooltip `"Pause personal notifications"` and agent tooltip `"Pause listening for this session"`. The title and rename control must fit alongside the pill; the canonical `"Release orchestration"` title must remain untruncated in both appearances.

#### 4. `U4` — Intelligent Suppression of `Team / Self` on Personal Networks & Self-Explanatory Hub-Level Visibility (`src/macos/cito-ui.swift`, `crates/cito-native/src/core/daemon.rs`, Questions 8 & 9)
- **Basis**: On `local` or a personal Hub (`tefx-mbp-personal` where only `@tefx`'s own devices are connected, or `auth_mode != "oidc"` with no other users), showing `[ 🔒 Self ]` / `[ 👥 Team ]` on every card is meaningless noise (`Question 8`), and even when shown, `"Self"` vs `"Team"` had zero explanation in the UI (`Question 9`).
- **Contract**:
  1. **Smart Multi-User Detection (`showScopeToggle`, `Question 8`)**:
     - In `cito-ui.swift:2345`, set `let hasOtherUsers = directoryAllPeers.contains(where: { $0.hub == listener.hub && !$0.user.isEmpty && $0.user != user }) || listeners.contains(where: { $0.hub == listener.hub && !$0.user.isEmpty && $0.user != user })` and **`let showScopeToggle = listenerHub?.auth_mode == "oidc" || hasOtherUsers`**.
     - On `local` and personal `none` / `token` Hubs where all connected devices belong to the same user, `[ 🔒 Self ]` / `[ 👥 Team ]` is **hidden** on both Human and agent session cards. New registrations default to `"workspace"` only when `auth_mode == "none"`; token and OIDC Hubs default to `"self"`. Self scope is principal-scoped across all of the user's Macs, allowing their sessions and devices to communicate while remaining private to that principal. Auto-Listen explicitly registers with `"scope": "self"` on every Hub, as specified in §21.1.2. Explicit or previously saved scope remains authoritative.
     - When `showScopeToggle == true` (on OIDC team Hubs or multi-user Hubs), keep the `68pt` uniform `Self` / `Team` button with self-explanatory tooltips:
       - `Self`: `"Visibility: Only Me (@\(user)) — only your own sessions and devices on '\(listener.hub)' can message this session. Click to share with Team."`
       - `Team`: `"Visibility: Team — anyone connected to '\(listener.hub)' can discover and message this session. Click to restrict to Only Me."`
  2. **Explicit Default Visibility Explanation in `Settings → Hubs & Network` (`Question 9`)**:
     - In the `Add / Edit Hub` form (`add_hub_form`, for non-`local` Hubs), display a clear caption explaining Hub visibility: `"Personal Hubs connect your Macs automatically; Team Sign-In (OIDC) Hubs start private to your account (@user) until shared with Team."`

#### 5. `U5` — SQLite `busy_timeout(1000)` & Error-Preserving Codex Discovery (`crates/cito-native/src/plugin_support.rs`, `plugins/codex/native.rs`, Question 4)
- **Basis**: `sqlite_rows()` (`plugin_support.rs:688`) opened `~/.codex/state_5.sqlite` without `sqlite3_busy_timeout`. Whenever Codex wrote to `state_5.sqlite` during a poll, `sqlite3_exec` returned `SQLITE_BUSY` (`rc = 5`), `plugins/codex/native.rs` converted the error into `vec![]` via `.unwrap_or_default()`, and `SessionActivity::reap_discovery` overwrote `provider.cached` with `[]`—causing `Codex · 8 recent` to vanish from `cardMenu`! In addition, `row_caller` spawned `git` subprocesses for every row even when `git_branch` and `git_origin_url` were already present in SQLite.
- **Contract**:
  1. In `crates/cito-native/src/plugin_support.rs` (`sqlite_rows`), declare `fn sqlite3_busy_timeout(db: *mut libc::c_void, ms: libc::c_int) -> libc::c_int;` and call **`sqlite3_busy_timeout(db, 1000)`** immediately after `sqlite3_open_v2` succeeds.
  2. In `plugins/codex/native.rs`:
     - Change `try_rows(req: &Value) -> Result<Vec<Value>>` to **propagate** `sqlite_rows` errors (`?`) instead of swallowing them with `.unwrap_or_default()`, and call `try_rows(req)?` inside `"discover_sessions"` so if SQLite is still locked after `1000ms`, `cito-plugin-codex` returns `Err(...)` and `SessionActivity::reap_discovery` **retains the previous cached Codex sessions** instead of wiping them to `[]`!
     - In `row_caller(req, r)` (`plugins/codex/native.rs:277`), when `r` comes from the `threads` SQLite table (`r.get("git_branch").is_some() || r.get("git_origin_url").is_some()`), construct the session JSON directly from `r["id"]`, `r["cwd"]`, `r["name"]`/`r["title"]`, `r["git_branch"]`, and `r["git_origin_url"]` **without spawning `git` subprocesses** when `git_branch` or `git_origin_url` is already populated in the row!
     - Also include `thread_source = ''` in the SQL predicate (`(thread_source IS NULL OR thread_source = '' OR thread_source = 'user')`) so threads with an empty-string `thread_source` are never excluded.

### 22.2 Verification, Fast Single-Pass Review & Dual-Mac Installation Policy (`MUST`)

- Update existing assertions in `crates/cito-native/tests/support/{ui.rs,daemon.rs,plugins.rs}` in place; do **NOT** add new heavy multi-process E2E test functions.
- Run a **fast single-pass `contract-and-simplicity` review & fix**, verify `cargo test --workspace && cargo build --release`, push to `origin/main` (without creating a release tag), and install on both Macs.










