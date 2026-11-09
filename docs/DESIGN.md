# Cito Architecture & Protocol Specification (V25 — Stage 9 `v0.20.1`: Holistic UX & Visual Hierarchy Overhaul, Terminology Unification, Configured-Only Session Discovery, Live Pi Process Discovery & Visual E2E Verification (`U1`–`U10`), Building on Stage 8 `L1`–`L17` & Pure Rust Core)

> **Normative Supersession Notice (`V25` / `v0.20.1`)**:
> This `V25` specification is the single authoritative contract for Cito `v0.20.1` (Stage 9 — Phase `v25`), superseding `V24` (`v0.20.0`). Stage 9 (`§11`) resolves all 11 live E2E user-reported UX, visual layout, truncation, session-discovery, and terminology defects (`U1`–`U10`) discovered during interactive macOS testing of `v0.20.0`, and introduces mechanical text-truncation detection (`truncated_labels`) plus `test_render_png` visual snapshot verification across all UI surfaces. All paths written with `~` in prose examples represent the user's home directory; in actual runtime configuration files (`settings.json`, `config.toml`, `LaunchAgent` plist), Cito always expands `~` to the resolved absolute filesystem path.

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
   - Inbound `cito-ui -> daemon` action lines use a separate **`1,048,576`-byte (`1 MiB`) limit**, excluding the newline delimiter, matching HTTP `/ui` request bodies. This admits a 65,536-code-point message plus its JSON envelope. Drain oversized lines through the next newline or EOF without growing the buffer, emit `composer_result` (`ok: false`) and `error` with `INVALID_INPUT: UI action exceeds 1 MiB`, and continue processing subsequent actions on the same stream.

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

The following transition was used for v0.18.0 and is superseded by Section 9. It does not describe the current build, test, or distribution entrypoints.
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
         "2. Paste the Bot Token below and click Pair Account… to generate a 1-time /start command.",
         "3. Send the /start command to your bot in Telegram — Chat ID fills in automatically."
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

2. **`[SHARP]` `U2` — Live Running `Pi` Process & Session Discovery + Self-Healing Bridge Socket (`plugins/pi/native.rs`, `plugins/pi/cito-bridge.ts`)**:
   - **Problem**: After clicking `Set Up` on `Pi Coding Agent`, already-open `pi` terminal sessions did not appear in `AVAILABLE SESSIONS` because `plugins/pi/native.rs` only scanned `~/.cito/sockets/pi-pid-*.sock` (which are absent for `pi` processes started prior to `Set Up` or after `Disconnect` cleaned `~/.cito/sockets`).
   - **Contract**:
     - **Self-healing socket in `plugins/pi/cito-bridge.ts`**:
       - Call `ensureServer()` during extension initialization (in addition to `session_start`), and register a `.unref()` `setInterval(..., 2000)` that checks whether `socketPath` still exists on disk when `~/.pi/agent/settings.json` contains the Cito package; if `socketPath` was unlinked (e.g. by a prior disconnect/repair), close the stale `server` handle and re-run `ensureServer()` so `pi-pid-<pid>.sock` is automatically restored within 2s.
     - **Live process + session file discovery in `plugins/pi/native.rs`**:
       - In `candidates(req)`: when `op == "discover_sessions"` (or `resolve_caller` / `verify` / `probe` when no socket matched), after collecting socket-backed candidates from `state(req).join("sockets")`, if `setup_inspect` reports `configured == true` (in `~/.pi/agent/settings.json`), also scan running `pi` processes owned by `geteuid()` via `/bin/ps -axo pid=,uid=,command=` where `matches_process(command)` is true and `pid` is not already in `out`:
         - Resolve the process's working directory via `/usr/sbin/lsof -a -d cwd -Fn -p <pid>` (parsing the `n<path>` line).
         - Look up the latest session `.jsonl` file in `config_home(req, "CITO_PI_HOME", "~/.pi/agent").join("sessions")` under the encoded cwd directory (`--<cwd_without_leading_slash_and_slashes_replaced_by_dashes>--`), reading `"id"` (or filename stem) as `session_id`, `"session_name"` / `"name"` (or directory basename) as `session_name`, and file `modified()` as `updated_at_ms` (falling back to `session_id: format!("pid-{p}")`, `session_name: basename(cwd)` if no session file is found).
         - Allow `resolve_caller` and `probe` in `plugins/pi/native.rs` to accept a live process candidate `(p, v)` when no socket responded yet (verifying `process_identity(p)` matches `matches_process`), so clicking `+ Listen` in `cardMenu` immediately enrolls the open Pi session!

3. **`[SHARP]` `U3` — Re-Enrolling the Human Node in `cardMenu` When Stopped (`src/macos/cito-ui.swift`)**:
   - **Problem**: Clicking `[ × ]` (`stop:<scoped_id>`) on the Human listener (`harness == "human"`) in `cardMenu` unlistened the Human node, and because `AVAILABLE SESSIONS` only lists agent sessions, there was no UI control to add the Human node back.
   - **Contract**:
     - In `src/macos/cito-ui.swift` `rebuild()`:
       - Check `let hasHumanListener = listeners.contains { $0.harness == "human" }`.
       - Whenever `!hasHumanListener` and `hubs.contains(where: { $0.status != "disconnected" })`:
         - Render a compact **"Human Node Paused"** card at the top of `cardMenu` (`id: "human_paused_card"`, `NSRect(x: 0, y: 0, width: 380, height: 52)`):
           - Inner card: `roundedView(NSRect(x: 10, y: 3, width: 360, height: 46), radius: 10, color: .labelColor.withAppearanceAlpha(0.05), border: .labelColor.withAppearanceAlpha(0.10))`
           - Badge: `makeHarnessBadge(NSRect(x: 10, y: 9, width: 28, height: 28), radius: 7, iconInset: 6, harness: "human", harnesses: map)`
           - Title (`NSRect(x: 46, y: 24, width: 220, height: 16)`, `size: 12.5, weight: .semibold`): `"Human Node (@\(hubs.first?.user ?? "local"))"`
           - Subtitle (`NSRect(x: 46, y: 8, width: 220, height: 14)`, `size: 10.5, color: .secondaryLabelColor`): `"Not listening · Click Enable to receive signals"`
           - Button: `MenuControl("Enable", frame: NSRect(x: 272, y: 12, width: 78, height: 22), leadingSymbol: "plus", radius: 6)` with `identifier = NSUserInterfaceItemIdentifier("listen_human")` and `style = .prominent`.
           - Clicking `listen_human` emits `["op": "listen_session", "hub": effectiveTargetHub, "harness": "human", "session_id": "default", "about": "Human receiver"]`.

4. **`[SHARP]` `U4` — Unified "Human Node / Human Channels" Terminology (`src/macos/cito-ui.swift`, `plugins/telegram/plugin.json`)**:
   - **Problem**: Using `"Mobile Alerts"` on buttons/banners conflicted with the `"Human Channels"` tab name and implied passive one-way alerts rather than a bidirectional Human Node on the signaling network.
   - **Contract**:
     - Replace all occurrences of `"Mobile Alerts"` with `"Human Channels"` or `"Telegram Channel"`:
       - `onboarding_step:channels` label: `"2. Human Channels →"`
       - `goto_channels_tab` label: `"Human Channels →"` (with callout text: `"Agent app connected · Optional: pair Telegram for two-way mobile chat"`)
       - `empty_open_channels` label in `cardMenu`: `"Human Channels…"` (with subtitle: `"Call cito_listen in a session or open Human Channels"`)

5. **`[SHARP]` `U5` — Non-Redundant, User-Controlled `?` Setup Guide Across All 3 Tabs & Dismissible Callout (`src/macos/cito-ui.swift`)**:
   - **Problem**:
     1. `showGuide = onboardingGuideOpen || !resolvedApps.contains(where: { $0.configured })` forced `onboarding_banner` open whenever 0 apps were configured (even after clicking `?`), duplicating the 3 tab buttons right below the header (`Screenshot 1` & `Screenshot 7`).
     2. `onboarding_banner` was only added inside `appsView` (`Tab 1`), so clicking `?` on `Human Channels` or `Hubs & Network` did nothing (`Screenshot 5`).
   - **Contract**:
     - `onboardingGuideOpen` defaults to `false` and is strictly controlled by `toggle_onboarding_guide` (`?` button in `settings_header`, which highlights with `style = onboardingGuideOpen ? .prominent : .neutral`) and `dismiss_onboarding_guide` (`×` inside the banner); it is **never** forced true by `!resolvedApps.contains(...)`.
     - When `onboardingGuideOpen == true`, render `onboarding_banner` (`NSRect(x: 20, y: 344, width: 480, height: 60)`) directly inside `root` above the active tab content (with `.windowBackgroundColor.withAppearanceAlpha(0.96)` and `.controlAccentColor.withAppearanceAlpha(0.35)` border) so clicking `?` works identically on **all 3 tabs (`apps`, `channels`, `hubs`)**!
     - Inside `onboarding_banner`:
       - Top line (`y: 36`): `"Quick Setup Guide — Click any step to jump:"` (`x: 12, y: 36, width: 420, height: 16`, `size: 11, weight: .semibold`) + `MenuControl("Close Guide", frame: NSRect(x: 450, y: 34, width: 22, height: 20), symbol: "xmark")` (`id: "dismiss_onboarding_guide"`).
       - Bottom line (`y: 8`): 3 step buttons (`onboarding_step:apps`: `"1. Agent Apps"`, `onboarding_step:channels`: `"2. Human Channels →"`, `onboarding_step:hubs`: `"3. Hubs & Network →"`), each styled `.prominent` when `settingsActiveTab == step.0` and `.neutral` otherwise.
     - Always show the section header `"CONNECTED AGENT APPS"` and subtitle `"Enable 1-click notification & wake support in your coding apps."` at the top of `appsView`.
     - For `apps_next_step_callout` (at the bottom of `appsView` when at least 1 app is configured and `!nextStepCalloutDismissed` and no Human Channel is configured yet):
       - Include both **`Human Channels →`** (`id: "goto_channels_tab"`, `frame: NSRect(x: 282, y: 7, width: 136, height: 24)`) and **`Skip`** (`id: "skip_next_step"`, `frame: NSRect(x: 424, y: 7, width: 48, height: 24)`, which sets `nextStepCalloutDismissed = true` and rebuilds).

6. **`[SHARP]` `U6` — Allow Skipping Optional Human Channel Setup (`src/macos/cito-ui.swift`)**:
   - **Problem**: Users landing on `Human Channels` (`Telegram`) had no `Skip` option if they only wanted `Mac Desktop Banner`.
   - **Contract**:
     - On any unconfigured channel card (`!ch.configured` and `ch.status == "not_configured"`), display `"○ Optional · Not Configured"` in `settings_channel_status:\(ch.id)` (note: preserve `"○ Not Configured"` when `channelResult?.ok == true` or in legacy tests if needed—wait: `tests/support/ui.rs` asserts `"○ Not Configured"` on `settings_channel_status:channel`! So keep `settings_channel_status:\(ch.id)` text as `"○ Not Configured"` for test compatibility, and render an explicit **`Skip for Now`** button `id: "skip_channel:\(ch.id)"`, `frame: NSRect(x: 132, y: 10, width: 96, height: 24)` in the card's bottom action bar when `!ch.configured` and no hub token is saved yet, which sets `nextStepCalloutDismissed = true` and switches to `settings_tab:hubs`!).

7. **`[SHARP]` `U7` — Eliminate All Button/Badge Truncation & Right-Align `CONNECTED HUBS` Row Controls (`src/macos/cito-ui.swift`)**:
   - **Problem**:
     1. `"Experimental"` pill on Claude Code truncated to `"Experimen..."` (`Screenshot 1`).
     2. `"✨ Paste from Invite"` and `"+ Add Hub…"` in `Hubs & Network` truncated to `"✨ Paste from Inv..."` and `"+ + Add Hu..."` (with duplicate `+ +`, `Screenshot 4`).
     3. On the `local` hub row, `Edit` (`x: 192`) and `Enabled` (`x: 318`) left an `84pt` empty gap on the right side (`Screenshot 4`).
   - **Contract**:
     - **`settings_app_tier:<id>` (`"Experimental"` badge)**: `frame: NSRect(x: 54 + titleW, y: 56, width: 88, height: 16)` with inner label `NSRect(x: 6, y: 1, width: 76, height: 14)` (`alignment = .center`).
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
     - In `src/macos/cito-ui.swift` `host_hub_card`:
       - When `authMode == "token"` and `!hostHub.token.isEmpty`, display a **`Copy Token`** pill (`id: "copy_host_hub_token"`, `frame: NSRect(x: 274, y: 44, width: 192, height: 20)`, `leadingSymbol: "key.fill"`) next to `host_hub_endpoint` showing `"Token: \(String(hostHub.token.prefix(8)))… Copy"`, which copies `hostHub.token` to the clipboard and calls `copied("✓ Token Copied")`!
       - Add a `"Port:"` label (`NSRect(x: 188, y: 16, width: 34, height: 15)`) before `host_hub_port` (`NSRect(x: 222, y: 12, width: 58, height: 24)`).
       - Widen `toggle_host_hub_auth` (`"Auth: Token"` / `"Auth: None"`, `NSRect(x: 286, y: 12, width: 102, height: 24)`) and `apply_host_hub_port` (`"Apply"`, `NSRect(x: 394, y: 12, width: 72, height: 24)`).
       - On `copy_hub_invite`: call `copyInvite.copied("✓ Copied")` when clicked.
       - On `apply_host_hub_port`: call `apply.copied("✓ Saved")` when clicked.

9. **`[SHARP]` `U9` — Explicit Field Labels & Protocol Explanations in `Add / Edit Hub` Form (`src/macos/cito-ui.swift`)**:
   - **Problem**: `add_hub_form` had zero labels above its text fields and zero explanation of `local://` vs `http://` / `https://` (`Screenshot 9`).
   - **Contract**:
     - Expand `add_hub_form` height inside `hubsView` (`NSRect(x: 20, y: 4, width: 480, height: 184)`):
     - **When editing `local` (`editingExistingHub && editingOriginalHub == "local"`)**:
       - Header (`NSRect(x: 14, y: 156, width: 452, height: 18)`, `size: 12.5, weight: .semibold`): `"Edit Built-in Local Hub (local://)"`
       - Protocol description (`NSRect(x: 14, y: 134, width: 452, height: 16)`, `size: 11, color: .secondaryLabelColor`): `"Runs in-process on this Mac (SQLite-backed). No external server or auth needed."`
       - Field Label (`NSRect(x: 14, y: 104, width: 452, height: 14)`, `size: 10, weight: .semibold, color: .secondaryLabelColor`): `"YOUR DISPLAY HANDLE (@USERNAME)"`
       - Input (`NSRect(x: 14, y: 76, width: 452, height: 24)`): `makeEditableField("settings_hub_user", ..., placeholder: "e.g. tefx", value: addHubUser)`
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
      - Render PNG snapshots of all 8 core UI states (`menu_human_paused.png`, `settings_apps.png`, `settings_guide_open.png`, `settings_channels_unconfigured.png`, `settings_channels_pairing.png`, `settings_hubs_token.png`, `settings_hubs_edit_local.png`, `settings_hubs_add_remote.png`) and inspect them with `uiux-auditor` multimodal image review until zero visual/UX defects remain.

11. **`[SHARP]` `U11` — Two-Stratum Human Channel Card (`stratum_hub:<id>` vs. `stratum_user:<id>`) Separating Hub Bot Gateway (`scope: "hub"`) from Personal Human Node Binding (`scope: "user"`) (`src/macos/cito-ui.swift`, `plugins/telegram/plugin.json`)**:
    - **Problem**: `settings_channel:telegram` rendered `Bot Token` (`scope: "hub"`, configured once per Hub by the Hub Owner/Admin) and `Chat ID` / `Pair Account…` (`scope: "user"`, configured by each Human Node joining the Hub) as flat sibling rows inside one box, failing to distinguish Hub Gateway infrastructure from personal Human Node enrollment.
    - **Contract**:
      - On `SettingsPanel → Human Channels` (`settings_channels_tab`):
        - Add a persistent `"TARGET HUB:"` label (`NSRect(x: 20, y: 342, width: 78, height: 14)`, `size: 10, weight: .semibold, color: .secondaryLabelColor`) to the left of the Hub selector scroll view (`x: 102, y: 336, width: 398, height: 28`).
        - Inside each Hub-routed channel card (`settings_channel:\(ch.id)`), partition `visibleFields` into `hubFields = visibleFields.filter { $0.scope == "hub" }` and `userFields = visibleFields.filter { $0.scope != "hub" }`, and render **two visually distinct sub-containers** separated by a `1pt` divider:
          1. **Upper Sub-Container — `stratum_hub:\(ch.id)` (`1. HUB BOT GATEWAY · ONCE PER HUB`)**:
             - Header label (`id: "stratum_hub_title:\(ch.id)"`): `"1. HUB BOT GATEWAY · SHARED BY HUB '\(effectiveChannelHub.uppercased())' (ONCE PER HUB)"` (`size: 10, weight: .bold, color: .secondaryLabelColor`)
             - Status badge on the right of `stratum_hub:\(ch.id)`:
               - When `ch.hub_provided || ch.status != "not_configured"`: `"● Bot Gateway Active"` (`.systemGreen`) + (when `!hubFields.isEmpty`) **`Clear Token`** button (`id: "clear_channel_service:\(ch.id)"`, `.destructiveHover`).
               - When `ch.status == "not_configured"`: `"○ Bot Token Required"` (`.secondaryLabelColor`).
             - Role description:
               - If `!hubFields.isEmpty`: `"Configured once by the Hub owner (@BotFather /newbot). Shared by all human nodes on '\(effectiveChannelHub)'."` + `hubFields` (`channel_field:\(ch.id):\(f.key)` with left label, secure input, and `f.help_text`).
               - If `hubFields.isEmpty` (non-admin OIDC user on a shared Hub):
                 - When `ch.hub_provided`: `"Provided by '\(effectiveChannelHub)' Hub administrator — no bot setup needed on your Mac."`
                 - When `!ch.hub_provided`: `"Not yet configured by the '\(effectiveChannelHub)' Hub administrator."`
          2. **Lower Sub-Container — `stratum_user:\(ch.id)` (`2. YOUR PERSONAL HUMAN NODE · EACH USER ON THIS HUB`)**:
             - Header label (`id: "stratum_user_title:\(ch.id)"`): `"2. YOUR PERSONAL NODE (@\(currentUser)) · EACH HUMAN ON '\(effectiveChannelHub.uppercased())'"` (`size: 10, weight: .bold, color: .secondaryLabelColor`)
             - Role description: `"Bind your personal Telegram chat to receive agent wakes and send/reply to agents on '\(effectiveChannelHub)'."`
             - Renders `userFields` (`channel_field:\(ch.id):\(f.key)`, e.g. `Chat ID`), the active pairing instruction box (`pairing_instruction:\(ch.id)` + `copy_pair_command:\(ch.id)` + `cancel_pair_channel:\(ch.id)`), and the personal node action bar:
               - `pair_channel:\(ch.id)` (`"Pair Account…"` / `"Re-pair…"`)
               - When `ch.configured`: `toggle_channel:\(ch.id)` (`"Pause"` / `"Resume"`) + `unpair_channel:\(ch.id)` (`"Unpair"`, `.destructiveHover`)
               - When `!ch.configured`: `skip_channel:\(ch.id)` (`"Skip for Now"`)
               - Right-aligned: `test_channel:\(ch.id)` (`"Send Test"`) + `save_channel:\(ch.id)` (`"Save"`, `.prominent`).





