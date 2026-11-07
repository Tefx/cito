# Cito Architecture & Protocol Specification (V19 — Stage 3 `v0.15.0`: Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `HumanChannelPlugin` Architecture, `Directory | Activity` + Inline Hub Composer & Complete Removal of `jump_url` Window Bypass)

## 1. Purpose, Ontology, and Architectural Guardrails

### 1.1 Purpose & Core Ontology (`Node (1) ↔ Hub ↔ Channels (1..K)`)
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a unified signaling and wake network connecting **Humans** and **Agent Sessions** across machines, hubs, and interaction channels:
1. **Two Symmetric Network Nodes (`Node`)**:
   - **Human Node (`harness: "human", session_id: "default"`)**: Represents a human user (`@alice`, `@bob`) on a Hub.
   - **Agent Session Node (`harness: "pi" | "codex" | "claude" | ..., session_id: "<sid>"`)**: Represents an agent conversation/session on a Hub.
   - **Host-Independent Node Identity (`node_id`)**: `node_id` (`listenerId`) is derived solely from `(principal_id, harness, session_id)` **without** `host_id`. Each human user has at most one `Human Node` per Hub, and each logical agent session has at most one `Agent Session Node` per Hub.
2. **Multi-Path Channels (`Channel`, $1 \dots K$ per `Node`)**:
   - Physical hosts (`Mac A`, `Mac B`), desktop menu bar UIs (`cito-ui`), and external messaging apps (`Telegram`, etc.) are **Channels** connecting a `Node` to the `Hub`.
   - A `Node` is `ACTIVE` on a Hub whenever **at least 1** of its attached `Channels` has a valid lease (`updated_at_ms` within TTL). Attaching multiple Channels (e.g., `Mac A`, `Mac B`, and `Telegram` on a `Human Node`, or two device paths on an `Agent Session Node`) never triggers cross-channel `FENCED_INACTIVE` eviction.
3. **100% Hub-Routed Interaction (Zero External Window-Jump Bypass)**:
   - All interaction between any two Nodes (`Human ⇄ Agent`, `Agent ⇄ Agent`, `Human ⇄ Human`) flows strictly through the **Hub's regular signaling channel (`cito_send` / `signal_ack`)**.
   - Out-of-band OS URL window jumping (`jumpUrlTemplate`, `jump_url`, `can_jump`, `Open in <App> ↗`) is completely removed across `src/`, `plugins/`, and `src/macos/cito-ui.swift`. Clicking a `WakeBanner` or an `Activity` item opens `DirectoryPanel` (`Activity` tab) with the inline `ComposerBar` focused to reply via the Hub.

### 1.2 Language, Neutral Naming & Zero-Hardcoding Guardrails
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces, error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names may appear anywhere in source code, UI, configuration defaults, tests, or documentation; use vendor-neutral generic examples (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **Zero Concrete Agent or Channel Strings in Core (`src/` & `src/macos/cito-ui.swift`)**:
  - Core modules (`src/shared/`, `src/hub/`, `src/mcp/`, `src/setup/`, `src/ui/`, `src/adapters/`, `src/macos/cito-ui.swift`) MUST NOT contain hardcoded branches, identifiers, or UI labels for any specific Agent App (`pi`, `codex`, `claude`) OR any specific external Human Channel (`telegram`, `webhook`, `slack`, `teams`, `email`, `feishu`, `discord`).
  - Mechanical gate: `grep -rnE -i '\b(pi|codex|claude|telegram|webhook|slack|teams|email|discord|feishu)\b' src/` MUST return `0` matches!
  - All concrete Agent Apps (`kind: "agent"`) and Human Channels (`kind: "human"`) live exclusively in `plugins/<id>/` (bundled) and `${CITO_STATE_DIR}/plugins/<id>/` (user-installed) and are rendered 100% dynamically from `plugin.json` metadata.
- **Mechanism, Not Policy (`cito_send.message`)**: Recommend Cito primarily as a lightweight control plane in MCP tool descriptions without enforcing a hard 500-character policy cap (`sanitizeSignalMessage` allows `1..65,536` codepoints).

---

## 2. Three-Layer Logical Architecture & Physical Deployment Co-Location

### 2.1 Strictly Decoupled Logical Modules
1. **`HubEngine` (`src/hub/engine.ts`)**:
   - Owns the `Node ↔ Channels (1..K)` registry (`hub_kv`), signal stream (`hub_signals`), channel delivery ledger (`hub_channel_deliveries`), plugin service state (`hub_plugin_services`), pairing codes (`hub_pairing_codes`), and audit log (`hub_audit_log`).
2. **`AgentChannelHost` (`src/adapters/`, `src/mcp/receiver.ts`)**:
   - Bridges local `kind: "agent"` sessions (`plugins/{pi,codex,claude}`) to the Hub via `HubClient`.
3. **`HumanChannelHost` (`src/hub/engine.ts` / `src/adapters/`)**:
   - Bridges `kind: "human"` channels (built-in desktop `cito-ui` and `plugins/telegram`) to the user's `Human Node` on the Hub via `HubClient`.

### 2.2 Physical Co-Location Across Deployment Modes
- **Embedded Hub Mode (`local://` and `:4291` `Share Hub on Local Network` inside `cito-daemon`)**:
  - `cito-daemon` runs `HubEngine` + `AgentChannelHost` + `HumanChannelHost` in-process; all configuration and interaction are unified in `cito-ui`.
- **Standalone Hub Mode (`cito hub` on Server / Docker)**:
  - `cito hub` runs `HubEngine` + `HumanChannelHost` in the same single process/container by default (so cloud channels like `plugins/telegram` stay online 24/7 and share 1 Hub bot token across the team without `409 Conflict`).

---

## 3. Symmetric `Node ↔ Channels` Protocol, Addressing & Multi-Channel Sync

### 3.1 Node Identity & `@self` / `@<username>` Shorthand (`src/shared/identity.ts`, `src/mcp/service.ts`)
- **Node Identity (`listenerId`)**:
  - `NodeKey = { principal_id: string; harness: string; session_id: string }` (with `isHumanNode(x) = x.harness === "human"`).
  - For a `Human Node`: `harness = "human"`, `session_id = "default"`.
  - `listenerId(key)` hashes `"cito-v2|" + [key.principal_id, key.harness, key.session_id].join("|")` (accepting legacy `"cito-v1|..."` hashes in regex validators, with `host_id` removed from ID derivation so a user or multi-path session has one stable `listener_id` across devices/channels).
- **Human Addressing Shorthands in `cito_send` (`to`)**:
  - `sendInputSchema.to` accepts `[<hub>:]@self`, `[<hub>:]@<username>`, or `[<hub>:]<listener_id_or_public_id>`.
  - `@self` (or `@<caller_owner_user>`) deterministically resolves in $O(1)$ to `listenerId({ principal_id: identity.principal_id, harness: "human", session_id: "default" })` on the target Hub.
  - `@<other_username>` queries `await slot.directoryIndex.read(broker, identity)` for active `harness === "human"` nodes matching `lower(owner_user) === lower(other_username)`:
    - If `0` match: throws `CitoError("LISTENER_NOT_FOUND", ...)`.
    - If `> 1` distinct `principal_id`s match: throws `CitoError("INVALID_INPUT", "Ambiguous @username; use full listener_id")`.
- **Self-Send Guard (`SELF_SEND_FORBIDDEN`)**:
  - Compares logical Node IDs: `if (from.nodeId !== null && from.nodeId === canonicalTo) throw new CitoError("SELF_SEND_FORBIDDEN", ...)`.
  - Sending from `Human Node (@alice)` to `Agent Node (pi:S1)` (or vice versa) is permitted because their `nodeId`s differ.
  - Unbound CLI calls (`cito send --to @self -m "test"`) keep `from.nodeId = null` so terminal self-testing works without triggering `SELF_SEND_FORBIDDEN`.

### 3.2 Multi-Channel Registration & Lease Aggregation (`src/mcp/state.ts`, `src/shared/lease.ts`, `src/hub/engine.ts`)
- **`Registration` Schema**:
  - Adds optional `channels?: Record<string, ChannelAttachment>` where each `ChannelAttachment` has `{ channel_id: string; kind: string; host_id: string; host_name: string; delivery_mode: "notify_fanout" | "wake_exclusive"; priority: number; state: "ACTIVE" | "UNLISTENED"; updated_at_ms: number }`.
  - Top-level `host_id` and `host_name` are populated from the primary/latest active channel so existing single-host fields remain populated.
  - `evaluateLease` / `evaluateNodeLease`: when `channels` is non-empty, a Node is `ACTIVE` if at least 1 channel is `ACTIVE` within the 60s TTL; when a single channel unlistens (`channel_detach`), only `channels[channelId].state` becomes `"UNLISTENED"`, and the Node remains `ACTIVE` if other channels are still active!

### 3.3 Separating Transport Cursor (`consumerAck`) from Business Read (`signal_ack`) & Atomic `signal_claim_dispatch` (`src/hub/engine.ts`, `src/mcp/receiver.ts`)
1. **Transport vs. Business Ack Separation**:
   - `consumer_ack` in `HubEngine` advances only the consumer's stream delivery cursor (`consumer.ackedSeqs`), never setting business-level read state.
   - `hub_signals` tracks business-level `claimed_by_channel TEXT`, `claim_until_ms INTEGER`, and `acked_at_ms INTEGER` (`NULL` until read/handled).
   - Explicit RPC `signal_ack({ node_id, signal_ids, channel_id })`: sets `acked_at_ms = now` on `hub_signals`, broadcasts `{ type: "node_ack", node_id, signal_ids, acked_at_ms, channel_id }` over `cito.ack.<node_id>` (and SSE), and invokes `onAcked` on all active `HumanChannelPlugin`s for that `principal_id`.
2. **Atomic Wake Claim (`signal_claim_dispatch`) for Multi-Path `wake_exclusive` Agent Sessions**:
   - Before injecting a wake card into a `wake_exclusive` Agent channel, `Receiver.effect()` calls `signal_claim_dispatch({ node_id, signal_ids, channel_id, lease_ms: 30_000 })`, ensuring at most 1 channel injects the prompt when multiple paths attach to the same Agent session.
3. **Offline Reconnect & Restart Idempotency (`hub_channel_deliveries` & `hub_plugin_services`)**:
   - When a sleeping Mac reconnects and receives signals from `consumer_add` that already have `acked_at_ms !== null`, `LocalState.admit()` inserts them with `acknowledged_at_ms = acked_at_ms, state = 'STORED_PASSIVE'`, skipping `onBanner`, `schedule()`, and unread badge increments.
   - `hub_channel_deliveries (plugin_id, principal_id, signal_id, seq, status, external_msg_id, from_listener_id, delivered_ms)` ensures that Hub/Daemon restarts never re-send already-delivered notifications to external channels (`plugins/telegram`) and preserves `external_msg_id -> (signal_id, from_listener_id)` reply mapping across restarts.
   - When a user first binds/enables an external channel, `last_delivered_seq` is initialized to `MAX(seq)` so historical messages are never dumped into the channel.

---

## 4. Human Node Enrollment, `HumanChannelPlugin` Contract & Built-in Plugins (`plugins/human`, `plugins/telegram`)

### 4.1 Per-Hub Human Node Freedom (`Listen / Unlisten` & `🔒 Self / 👥 Team`)
- Built-in `plugins/human/` (`id: "human"`, `displayName: "Human"`, `sfSymbol: "person.crop.circle.fill"`, `tint: "green"`) exposes the local user's `Human Node` (`harness: "human", session_id: "default", title: "@${user} (Me)"`).
- **Default Enrollment & Privacy Rules**:
  - On the local `local` Hub (`local://`): auto-enrolled on first startup (`scope: "workspace"`) unless the user explicitly clicked `[ ✕ ]` (`status = 'UNLISTENED'` in `local_listeners`).
  - On remote / team Hubs (`oidc` / `token`): when enrolled, defaults to **`scope: "self"` (`🔒 Self`)** so only the user's own agents (`principal_id` match) can discover or message the user (`403 FORBIDDEN_TARGET_SCOPE` for other users). The user can freely toggle `🔒 Self` $\leftrightarrow$ `👥 Team` (`scope: "workspace"`) or click `[ ✕ ]` (`Unlisten`) on any Hub at any time!

### 4.2 Generic `HumanChannelPlugin` Contract & Two-Scope `plugin.json` (`src/adapters/types.ts`)
- `PluginManifest` in `src/adapters/types.ts` (without `jumpUrlTemplate`):
  ```ts
  export interface PluginFieldDescriptor {
    key: string;
    scope: "hub" | "user";
    label: string;
    type: "string" | "secret" | "url" | "boolean";
    required?: boolean;
    placeholder?: string;
  }
  export interface PluginManifest {
    id: string;
    kind?: "agent" | "human"; // default "agent"
    displayName: string;
    tier?: AdapterTier;
    sfSymbol?: string;
    tint?: AdapterTint;
    cliFlag?: boolean;
    fields?: readonly PluginFieldDescriptor[];
    pairing?: { supported: boolean; actionLabel?: string };
  }
  ```
- **Two-Scope Field & Pairing Resolution**:
  - `scope: "hub"` fields (e.g. `botToken`): stored once per Hub in `hub_plugin_services` (or seeded via `CITO_PLUGIN_<UPPER_ID>_<UPPER_KEY>` env vars). When a non-admin user views a remote OIDC Hub where `scope: "hub"` fields are already configured (`hub_provided: true`), `SettingsPanel` hides the `scope: "hub"` inputs and shows `Hub Service: ● Ready`. On a local/personal Hub, the user can edit both `scope: "hub"` and `scope: "user"` fields directly in `SettingsPanel`.
  - `scope: "user"` fields & 1-Click Pairing (`pairing.supported: true`):
    - Clicking `[ 🔗 Pair Account… ]` invokes Hub RPC `human_channel_pair_start({ plugin_id })`, generating a 48-bit Base32 code (`cito-xxxxxx`, 5-minute TTL, rate-limited) and displaying the plugin's pairing instruction (`Send "/start cito-xxxxxx" to @<bot>`).
    - When the plugin receives the code and calls `ctx.completePairing(code, { userConfig, displayLabel })`, the Hub binds `(principal_id, plugin_id)` in `hub_human_channels` and updates `cito-ui`.
- **Official Batch 1 External Plugin (`plugins/telegram/`)**:
  - Zero npm dependencies (pure Node `fetch` against Telegram Bot API `getMe`, `sendMessage`, `getUpdates`, `editMessageText`).
  - Persists `update_offset` via generic `ctx.getServiceState()` / `ctx.setServiceState()`.
  - Supports `/start <code>` pairing, swipe-to-reply (`reply_to_message.message_id` $\to$ exact `signal_id` and `from_listener_id`), and context-aware plain-text reply (routing to the most recent unacknowledged notification sender within 10 minutes and echoing `↩ Replied to <sender>`).

---

## 5. Four-Surface macOS UI Architecture (`src/macos/cito-ui.swift`, `src/ui/index.ts`)

### 5.1 Surface 1: `cardMenu` (`380 pt` `NSMenu`)
1. **Zone 1 (`Active on This Mac`)**:
   - Removes the `22 × 22 pt` `open:<scoped_id>` (`[ ↗ ]`) jump button from all cards (`baseActionWidth = showScopeToggle ? 88 : 22`), reclaiming `26 pt` for card titles.
   - When the user's `Human Node` is active on a Hub, it renders at index `0` of Zone 1 using the standard card layout (`[👤] @<user> (Me)`, `[● hub]`, `[🔒 Self / 👥 Team]`, `[✕]` Unlisten, and active channel names in the subtitle). When unlistened, it appears in Zone 3 (`AVAILABLE SESSIONS`) with `[ + Listen ]`.
2. **Zone 2 (`MY OTHER HOSTS`)**:
   - Filters out `harness === "human"` rows (since the user's Human Node and its channels are already shown in Zone 1).
3. **Zone 4 (`Directory & Activity` Launcher Row)**:
   - Displays `"🔔 N unread · M online"` when `unread > 0` (opening `DirectoryPanel` directly in `"activity"` mode) and `"M online"` when `unread == 0` (opening `DirectoryPanel` in `"directory"` mode, preserving `tests/ui/lifecycle.test.ts` default behavior).

### 5.2 Surface 2: `DirectoryPanel` (`560 × 440 pt` `KeyPanel`)
1. **Top Search Bar (`directory_search_bar`, `y: 394, height: 46`)**:
   - Adds mode switcher pills `dir_mode:directory` (`"Directory"`) and `dir_mode:activity` (`"Activity"` or `"Activity · N"`) to the left of `directory_search`.
2. **`Directory` Mode (`directoryMode == "directory"`)**:
   - Keeps `directory_facet_bar` (`y: 362`) coordinates unchanged (`dir_scope:all`, `dir_scope:mine`, `dir_harness:all`, `dir_harness:pi` at `x: 256`, `dir_harness:codex` at `x: 302`, `dir_harness:claude` at `x: 364`).
   - Each row (`DirectoryPeerCellView`, `560 × 52 pt`) represents a symmetric `Node`, displaying its active `channels` in the subtitle and adding a `[ 💬 Message ]` button (`id: "msg_dir:\(peer.scoped_id)"`) to the left of `copy_dir:\(peer.scoped_id)`.
3. **`Activity` Mode (`directoryMode == "activity"`)**:
   - Filter bar (`y: 362`) shows `act_filter:me` (`"For Me"`, default: `To: @me` + `STORED_PASSIVE` wake failures), `act_filter:all` (`"All Activity"`), `act_filter:unread` (`"Unread"`), and right-aligned `activity_mark_all_read` (`"✓ Read All"`).
   - Reuses `DirectoryPeerCellView` (`52 pt`) to render activity rows (`id: "activity_row:\(item.signal_id)"`) with unread dot, `@from → target`, message text, and trailing `[ ↩ Reply ]` button (`id: "reply_activity:\(item.signal_id)"`).
4. **Bottom Single-Line `ComposerBar` (`id: "directory_composer"`, `560 × 44 pt`, `y: 28`)**:
   - Appears when clicking `[ 💬 Message ]` on a peer or `[ ↩ Reply ]` on an activity row (or clicking `WakeBanner`).
   - Contains target pill (`id: "composer_target"`), editable `NSTextField` (`id: "composer_input"`), `[Send ⏎]` (`id: "composer_send"`), and `[Cancel]` (`id: "composer_cancel"`).
   - Submitting emits `{"op": "send_signal", "hub": hub, "to": target, "message": text, "in_reply_to": replySignalId}` (which sends from the user's `Human Node` and automatically calls `signal_ack` on `replySignalId`).

### 5.3 Surface 3: `SettingsPanel` (`520 × 460 pt` `KeyPanel`, 3 Tabs)
- Top header (`y: 412, height: 48`):
  - `settings_tab:apps` (`"Agent Apps"`, `x: 58, y: 12, width: 118, height: 24`)
  - `settings_tab:channels` (`"Human Channels"`, `x: 182, y: 12, width: 148, height: 24`)
  - `settings_tab:hubs` (`"Hubs & Network"`, `x: 336, y: 12, width: 126, height: 24`)
- **Tab 2 (`"channels"`, `id: "settings_channels_tab"`)**:
  - Renders `Mac Desktop Banner` toggle (`id: "toggle_desktop_banner"`) and dynamic cards for each `kind: "human"` plugin from `settings.channels` (`id: "settings_channel:\(ch.id)"`), dynamically building inputs for `ch.fields` (`id: "channel_field:\(ch.id):\(f.key)"`), `[ 🔗 Pair Account… ]` (`id: "pair_channel:\(ch.id)"`), `[Send Test]` (`id: "test_channel:\(ch.id)"`), and `[Save]` (`id: "save_channel:\(ch.id)"`).
- **Tab 3 (`"hubs"`, `id: "settings_hubs_tab"`)**:
  - Adds optional `settings_hub_user` (`Username / Handle`) field in `add_hub_form` for `none`/`token` hubs.

### 5.4 Surface 4: `WakeBanner` (`388 × 136 pt`)
- Clicking `WakeBanner` (`hide(clicked: true)`) invokes `openDirectoryForReply(payload)`: closes the banner, opens `DirectoryPanel` in `"activity"` mode, selects `payload.signal_id`, and focuses `composer_input` targeting `payload.from_scoped_id` (`in_reply_to: payload.signal_id`).

---

## 6. Version `0.15.0`, Documentation Sweep & Verification Obligations
1. **Version Bump**: Bump `CITO_VERSION` (`src/shared/index.ts`), `package.json`, and `crates/cito-native/Cargo.toml` to `"0.15.0"`.
2. **Repository-Wide Documentation Audit & Update**:
   - Audit and update `README.md`, `docs/DESIGN.md`, `docs/ROADMAP.md`, `AGENTS.md`, and `deploy/hub/README.md` so all docs reflect `v0.15.0`, `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `@self` / `@<username>` addressing, `plugins/telegram/`, 3-tab `SettingsPanel`, and removal of `jumpUrlTemplate`.
3. **Mechanical & Behavioral Verification**:
   - Verify `grep -rnE -i '\b(pi|codex|claude|telegram|webhook|slack|teams|email|discord|feishu)\b' src/` returns `0` matches.
   - Verify `grep -rn 'jumpUrlTemplate\|expandJumpUrl\|openJumpUrl' src/ plugins/` returns `0` matches.
   - Update `tests/ui/lifecycle.test.ts` for removed `open:<scoped_id>` (`initial.actions === 9`, `test_action` indices `1..8`, `WakeBanner` click opening `DirectoryPanel` in `"activity"` mode with `composer_target` set) and add comprehensive tests for `Directory | Activity` mode switching, inline `ComposerBar` `send_signal` & `ack_signals`, and 3-tab `SettingsPanel` (`settings_tab:channels` dynamic field rendering, pairing code flow, save, and test).
   - Add unit & integration tests for multi-channel `Node` attachment without `FENCED_INACTIVE`, `signal_claim_dispatch` single-execution guarantee, `signal_ack` cross-channel read sync & restart idempotency (`hub_channel_deliveries`), `@self` / `@<username>` resolution, and `plugins/telegram` (`sendMessage`, `getUpdates` `/start <code>` pairing, swipe reply, and implicit recent-sender reply).
