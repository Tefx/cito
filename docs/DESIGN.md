# Cito Architecture & Protocol Specification (V40)

> This document is the single authoritative specification for Cito (`V40`), superseding all earlier stage drafts. All paths written with `~` in prose examples represent the user's home directory; runtime configuration files use expanded absolute paths.

---

## 1. Purpose, Ontology, and Architectural Guardrails

### 1.1 Core Ontology (`Node (1) ↔ Hub ↔ Channels (1..K)`)
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a unified signaling and wake network connecting **Humans** and **Agent Sessions** across machines, hubs, and external messaging channels:
1. **Two Symmetric Network Nodes (`Node`)**:
   - **Human Node (`harness: "human", session_id: "default"`)**: Represents a user's personal inbox (`@alice`, `@bob`) on a Hub. Each user has at most one Human Node per Hub (`session_id` is strictly `"default"`).
   - **Agent Session Node (`harness: "<agent-id>", session_id: "<sid>"`)**: Represents an agent conversation/session on a Hub.
   - **Host-Independent Node Identity (`node_id`)**: `node_id` (`listener_id`) is derived solely from `(principal_id, harness, session_id)` **without** `host_id`.
2. **Multi-Path Attachments (`Channels`, $1 \dots K$ per `Node`)**:
   - Physical devices (`Mac A`, `Mac B`) and Hub-hosted external channels (`Telegram`, etc.) attach to a `Node` as distinct `ChannelAttachment` entries (`<host_id>:human` for devices; `hub:<plugin_id>` for Hub-hosted channels).
   - Attaching multiple devices or external channels never evicts sibling attachments.
3. **100% Hub-Routed Interaction (Zero Floating Popups & Zero Window-Jump Bypass)**:
   - All `Human ⇄ Agent`, `Agent ⇄ Agent`, and `Human ⇄ Human` messaging flows strictly through the Hub's signaling stream (`cito_send` / `signal_ack`).
   - Built-in desktop popups (`BannerWindow`) and out-of-band window-jump URLs (`jump_url`) are absent across the codebase. Local Mac awareness uses the Menubar unread badge and the `Activity` inbox exclusively; active push alerts are delivered by Hub-hosted external channels (such as `Telegram`).

### 1.2 Language, Neutral Naming & Zero-Hardcoding Guardrails
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces, error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names may appear anywhere in source code, UI, configuration defaults, tests, or documentation; use vendor-neutral examples (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **Zero Concrete Agent or Channel Strings in Core (`crates/cito-native/src/core/` & `src/macos/cito-ui.swift`)**:
  - Core Rust modules (`crates/cito-native/src/core/`) and `src/macos/cito-ui.swift` MUST NOT hardcode identifiers, config keys, or display names for specific Agent Apps or external Human Channels.
  - Verification: inspect any case-insensitive lexical matches on known agent/channel names (`pi`, `codex`, `claude`, `telegram`, `webhook`, `slack`, `teams`, `email`, `discord`, `feishu`) across `crates/cito-native/src/core/` and `src/macos/` semantically to verify zero concrete integration branches or string literals (legitimate domain/geometry identifiers such as `let slack = ...` are allowed without code deformation).
  - UI copy for dynamic channels uses manifest fields (`ch.display_name`, e.g., `"Pause \(ch.display_name)"` / `"Resume \(ch.display_name)"`).
- **Mechanism, Not Policy (`cito_send.message`)**: Recommend Cito as a lightweight control plane in MCP tool descriptions without enforcing a hard 500-character policy cap (`sanitize_signal_message` permits `1..65,536` Unicode code points after stripping non-whitespace control characters).

---

## 2. Three-Layer Logical Architecture & Physical Co-Location

### 2.1 Logical Modules (`crates/cito-native/src/core/`)
1. **`HubEngine` (`hub.rs`)**: Owns the SQLite-backed `Node ↔ Channels` registry (`hub_kv`), signal stream & inline exclusive wake claims (`hub_signals`), consumer state (`hub_consumers`, `hub_consumer_acks`), channel delivery ledger (`hub_channel_deliveries` & `hub_channel_delivery_msgs`), plugin service configs (`hub_plugin_services`), user channel bindings (`hub_human_channels`), pairing codes (`hub_pairing_codes`), processed reply dedup (`hub_processed_replies`), and audit log (`hub_audit_log`).
2. **`AgentChannelHost` (`daemon.rs`, `receiver.rs`, `plugins.rs`)**: Bridges local coding sessions (`plugins/*` and `${CITO_STATE_DIR}/plugins/*`) to Hubs via `HubClient` and out-of-process `agent-harness` plugin subprocesses.
3. **`HumanChannelHost` (`hub.rs`, `hub_plugins.rs`, `daemon.rs`)**:
   - **Native Desktop Companion (`cito-ui`)**: Resolved directly as `Cito.app/Contents/Helpers/cito-ui` (dev `bin/cito-ui-darwin-arm64`, `CITO_UI_BINARY` override), spawned by `cito daemon` on the user's Mac over the UI NDJSON stream (`stdin`/`stdout`). It is **not** a Hub plugin or pseudo-plugin manifest.
   - **Hub-Hosted External Channels (`scope: "hub"`)**: Managed by `HubEngine` (embedded in `cito daemon` for `local://` or standalone `cito hub`) over the persistent `human-channel` NDJSON subprocess protocol (`plugins/telegram` and external `${CITO_STATE_DIR}/plugins/<id>/`).

### 2.2 Physical Co-Location
- **Embedded Local & Shared LAN/Tailscale Hub (`cito daemon`)**: Runs one local `HubEngine` persisted at `${stateDir}/hub.sqlite`. When `Share This Mac as a Hub` (`host_hub.enabled`) is active, `cito daemon` binds an HTTP/SSE server (`0.0.0.0:4291` default) onto that same `HubEngine`.
- **Standalone Hub (`cito hub`)**: Runs `HubEngine` persisted at `${dataDir}/hub.sqlite` and hosts `scope: "hub"` `human-channel` subprocesses continuously so team/cloud external channels stay online 24/7.

---

## 3. Symmetric `Node ↔ Channels` Protocol, Human Inbox RPCs & Delivery Guarantees

### 3.1 Node Identity & Addressing (`identity.rs`, `daemon.rs`)
- **Identity Derivation**: `NodeKey = { principal_id, harness, session_id }`. `listener_id(key)` hashes `"cito-v2|" + principal_id + "|" + harness + "|" + session_id` (`c_` + 16 hex chars; validators accept legacy `cito-v1|` IDs). `public_listener_id(owner_user, canonical_id)` normalizes `owner_user` through `normalize_slug` (`[a-z0-9-]`, max 32 chars).
- **Human Addressing Shorthands (`to`)**: Accepts `[<hub>:]@self`, `[<hub>:]@<username>`, or `[<hub>:]<listener_id_or_public_id>`.
  - `@self` resolves to the caller's `(principal_id, "human", "default")` on the target Hub.
  - `@<other_username>` matches active `harness == "human"` nodes by case-insensitive `owner_user` or `normalize_slug`: `0` matches → `LISTENER_NOT_FOUND`; `> 1` distinct `principal_id`s → `INVALID_INPUT` ("Ambiguous @username").
- **Self-Send Guard (`SELF_SEND_FORBIDDEN`)**: Rejects sends where `from.node_id == Some(canonical_to)`; allows `Human ⇄ Agent` and unbound CLI sends (`from.node_id = None`).

### 3.2 Explicit Hub-Authoritative Human Operations & CAS (`hub.rs`, `registration.rs`, `daemon.rs`, `receiver.rs`)
Ambiguous client-side whole-record `kv_save` Human mutations are rejected; all Human state transitions flow through four explicit Hub-owned RPC operations scoped to the caller's authenticated principal:

| RPC Op | Allowed Inputs & Ownership | Write Set & `human_control_revision` Rule | Conflict / Error Behavior |
|---|---|---|---|
| `human_device_attach` | `(channel_id: "<host_id>:human", host_id, host_name, owner_token, binding_generation)` | Writes **only** `channels[channel_id]` (`state: "ACTIVE"`, `updated_at_ms: hub_now_ms`). **Ownership fencing**: permitted for an absent `channel_id`, a same-owner retry matching `(binding_generation, owner_token)`, or a newer replacement (`binding_generation > existing.binding_generation`); rejects stale/superseded `(binding_generation, owner_token)` without mutating the attachment or sibling devices. Initializes defaults (`human_paused: false`, `about: "Human"`, `scope: "workspace"` on `local`/`none` trusted Hubs or `"self"` on `token`/`oidc` remote Hubs, `human_control_revision: 1`) **only** when creating a new Human record; for legacy existing records missing `human_control_revision`, Hub initializes `human_control_revision = 1` atomically on first read/mutation while preserving stored `human_paused`, `about`, `scope`, and attachments. **Never overwrites** existing `human_paused`, `about`, `scope`, `human_control_revision`, or sibling attachments. | Rejects stale/superseded generation/token without mutation; runs `reconcile_registration_state` and returns updated Human record + `server_now_ms`. |
| `human_device_heartbeat` | `(node_id, channel_id, binding_generation, owner_token)` | Verifies matching `(binding_generation, owner_token)` on `channels[channel_id]`, updates **only** `updated_at_ms = hub_now_ms`, and runs `reconcile_registration_state`. **Never** advances `human_control_revision` and never touches `human_paused`, `about`, or `scope`. | Returns ownership/stale-generation error if fenced; never overwrites sibling channels. |
| `human_device_detach` | `(node_id, channel_id, binding_generation, owner_token)` | Verifies captured `(binding_generation, owner_token)` and sets **only** `channels[channel_id].state = "UNLISTENED"`, then runs `reconcile_registration_state`. **Never** sets `human_paused = true`, never advances `human_control_revision`, and never alters `about`, `scope`, or sibling attachments. | Propagates auth/transport failures; retries only true storage CAS conflicts. |
| `human_inbox_update` | `(node_id, expected_control_revision, paused?, scope?, about?)` | Protected by `expected_control_revision` CAS against `human_control_revision` (after Hub-side initialization to `1` for any legacy revision-less record). Patches **only** fields explicitly present (`Resume Inbox` omits `about` so stored descriptions survive!). Increments `human_control_revision += 1` on success, runs `reconcile_registration_state`, and preserves all attachments/tokens. | Returns `CONTROL_REVISION_CONFLICT` with current Hub inbox state on mismatch (**never** silently replays a stale Pause over a newer Resume). |

- **Atomic `publish_payload()` Pause Enforcement**: `hub.rs:publish_payload()` enforces `human_paused != true` inside the Hub publish transaction, rejecting sends to a paused Human inbox with **`LISTENER_NOT_FOUND`** (`HTTP 404`), matching `daemon.rs` and `hub.rs`. Deliveries already handed to an external channel plugin before Pause commits may complete; subsequent reservations stop immediately.
- **No Local Detach on `human_paused`**: `daemon.rs:poll` NEVER marks `local_listeners.status = 'UNLISTENED'` when observing `human_paused == true`—device heartbeats and projections stay independent of inbox pause. `set_scope` on `human` routes through `human_inbox_update` (`expected_control_revision` CAS). `unlisten_session` filters `status != 'FENCED_INACTIVE'` and resolves the current profile's principal.

### 3.3 Unified Lease Reconciliation & Structured Projections (`lease.rs`, `daemon.rs`, `hub.rs`)
- **Single Lease Rule (Hub Clock)**:
  - Host attachments (`host_id != "hub"`): `age < 60_000 ms` → `ACTIVE`; `60_000..=1_800_000 ms` → `DISCONNECTED_RETAINED`; `> 1_800_000 ms` → `EXPIRED` (comparing `hub_now_ms` / `server_now_ms` supplied by the Hub, never skewed client clocks).
  - Hub-hosted attachments (`host_id == "hub"`): `ACTIVE` whenever plugin is available, service config is complete, user binding is enabled, and destination is complete; never expires on Mac disconnect.
  - Node availability: `human_paused == true` → `UNLISTENED`; else any live attachment → `ACTIVE`; else any retained attachment → `DISCONNECTED_RETAINED`; else any expired attached device → `EXPIRED`; else `UNLISTENED`. Top-level `state` is maintained strictly as a compatibility summary (`ACTIVE` for `ACTIVE | DISCONNECTED_RETAINED`, else `UNLISTENED`). TTL expiry never sets `human_paused = true`, stale top-level `state` never vetoes fresh attachments, and the shared Human profile (`human_paused`, `about`, `scope`, `human_control_revision`) persists even if all devices expire.
- **Structured `attachments` & Single-Truth Projections**:
  - `snapshot()`, `peer_dto()`, and `settings_for_hub()` project structured `attachments` (`{ channel_id, location: "device" | "hub", host_id?, plugin_id?, display_name, availability, is_local }`, stripped of owner tokens), `human_listener_id`, `human_paused`, `human_about`, `human_control_revision`, `human_availability`, `human_scope`, and `human_state_status` (`"current" | "stale" | "unknown"`), even when this Mac has no local Human listener. `snapshot().hubs[*]` and `settings.hubs[*]` share this single projection without synchronous blocking RPCs.
  - `channel_list()` adds `inbox_paused: bool` and `delivery_enabled: bool` (`enabled && configured && !inbox_paused`) while preserving pairing/credentials across global Pause/Resume. Toggling `human_inbox_update` immediately invalidates/overlays cached `ChannelInventory`.
- **No Cross-Hub Coalescing & Unified Directory Pipeline**:
  - `directory_identity` keys on `(hub_identity, canonical_node_id)`: multiple attachments within one Hub merge into one Node row, while separate Hubs (`local:@alice` vs `work:@alice`) remain distinct rows. Cross-Hub suppression (`shadowKey` / `sharedKeys` in Swift) is removed completely.
  - One Rust pipeline evaluates visibility/principal → Hub-clock lease eligibility → within-Hub deduplication → search/scope/harness filters (`host:` searches active `attachments` as well as session host metadata) → stable sort → `matched_count == online_count` → limit. Swift renders returned Directory rows directly (retaining only initial `seedDirectoryPeersIfEmpty` fallback before the first response, without a second `matchesDirectoryPeer` query parser).

### 3.4 CLI & MCP Authority Matrix (`cli.rs`, `daemon.rs`)

| Entrypoint | Supported Human Operations | Exact Effect & Authority |
|---|---|---|
| `cito listen --harness human` / `cito unlisten --harness human` | Device-scoped attach / detach | Calls `human_device_attach` / `human_device_detach` for `<host_id>:human` only; never sets `human_paused`. |
| `cito inbox pause [--hub <name>]` / `cito inbox resume [--hub <name>]` | Shared inbox policy control | Calls `human_inbox_update` (`expected_control_revision` CAS); `resume` omits `about` to preserve stored description. Optional `--scope <self\|workspace>` (plus `--team` alias, matching `cito listen`) and `--about <text>`. |
| `cito unlisten --all [--hub <name>]` | Agent-only bulk pause | Pauses active non-human sessions (`harness != "human"`). Explicit `--hub` scopes to one Hub; omitted `--hub` cleans up all Hubs. |
| `cito status` | Read-only snapshot | Reads local/Hub snapshot (`human_paused`, `human_attachments`, sessions). |
| `cito setup` | Administrative app/hub setup | CLI/GUI only; excluded from MCP. |
| MCP (`cito_listen`, `cito_unlisten`) | Agent session self-determination | Rejects `harness == "human"` / `_meta.expected_harness == "human"` with **`FORBIDDEN`** (`Session Self-Determination` boundary prevents agents from pausing/resuming the user's shared inbox). |
| MCP (`cito_send`, `cito_inbox`, `cito_find`, `cito_status`) | Agent messaging & read-only status | Agents may address `@self` / `@<user>` (subject to `human_paused` enforcement) and inspect sanitized session status. |

### 3.5 Delivery Guarantees, Activity Scope & Channel Lifecycle
- **Transport vs. Business Ack & Wake Claims**: `consumer_ack` advances stream cursor; `signal_ack` marks `hub_signals.acked_at_ms` and broadcasts `node_ack` + `on_acked`. Offline acks queue in `pending_hub_acks` and flush on reconnect. Agent wakes unconditionally acquire `signal_claim_dispatch` (`30_000 ms` lease, renewed every `10_000 ms`, all-or-nothing transaction, fail-closed on RPC error):
  - **`already_claimed`**: losing channels store `STORED_PASSIVE` with `acknowledged_at_ms = NULL` and schedule a `30_500 ms` fallback check so an unacknowledged batch retries automatically if the winner crashes before `signal_ack`.
  - **`already_acked`**: if only part of a requested batch is already acknowledged/missing, the caller marks those `acked_ids` read, removes them from the batch, and rebatches the remaining unread signals immediately.
  - **`LocalState::admit` duplicates**: still schedule pending `RECEIVED` rows if a prior `consumer_ack` flush failed.
- **Activity Scope Conservation**: `ack_signals` (including UI optimistic `Read All`) honors `filter` (`"me"` affects only `target_harness == "human"`). `clear_activity_history` accepts both `filter` and optional `query` (using the exact `query_activity` search predicate so `"Clear N matching messages?"` clears strictly the `N` search-matched rows in `<filter>`). Clearing Activity hides cleared rows via `activity_hidden` while preserving pending agent delivery work, checkpoints, and in-flight completion; `snapshot()` `unread_count` aligns with `state.activity()?.1` (excluding `activity_hidden` rows) while `cito_inbox` / `cito_status` / `poll_listener` read all unacknowledged signals.
- **External Channel Delivery, Inbound Reply Atomicity & Destination Isolation**: `hub_channel_deliveries` reserves signals before I/O (`in_flight` 60s timeout) with hole retry after `initial_bind_seq` and 4,096-char multipart chunking recorded in `hub_channel_delivery_msgs`. In `hub.rs:inbound_reply`, deduplication (`hub_processed_replies`), reply publication (`hub_signals`), and source signal acknowledgment (`acked_at_ms` + `on_acked`) commit atomically in a single SQLite transaction before `reply_ack` returns; the plugin advances `cursor_save` **only after** confirmed `reply_ack` (or isolated per-update error handling), rolling back publication if source acknowledgment fails. When destination changes (`A → B`), confirmed chunk prefixes and reply mappings are isolated by destination identity (`already_sent_ids` / `inbound_reply` never cross from A to B), and late in-flight results from retired destination A are fenced while preserving `last_delivered_seq` and terminal `effect_unknown` safety.
- **1:1 External Ownership & Unthreaded Disambiguation**: `(hub, plugin_id, _external_chat_id, _external_user_id)` ownership is enforced 1:1 across both `redeem` and manual `human_channels_configure` (including paused `enabled == 0` rows, honoring OIDC principal equivalence). Unthreaded replies (`reply_to_message` absent) auto-route only when exactly one distinct sender exists in the 7-day window and no legacy duplicate ambiguity exists; otherwise the plugin asks the user to reply to a specific message.

---

## 4. Subprocess Plugin Contracts & Built-in Plugins

### 4.1 Discovery, Cleanup & `plugin.json` Schema
- Priority: `CITO_PLUGIN_PATH` > `${CITO_STATE_DIR}/plugins/<id>/` > bundled `Cito.app/Contents/Resources/plugins/<id>/` (CLI staging under `${CITO_STATE_DIR}/runtime/plugins/<id>/`).
- **Retired Pseudo-Plugin Cleanup**: `setup::ensure_plugins_staged` and plugin discovery unconditionally delete `${stateDir}/plugins/human` and `${stateDir}/runtime/plugins/human` without requiring any bundled `plugins/human/plugin.json`, and `plugins.rs` / `hub_plugins.rs` ignore/reject any leftover `id == "human"` manifest.
- **Manifest Schema**: `id` (`^[a-z0-9][a-z0-9_-]{0,31}$`), `displayName`, `kind` (`"agent-harness"` | `"human-channel"`), `tier` (`"stable"` | `"experimental"`), `scope` (`"hub"` | `"user"`), `sfSymbol`, `tint`, `cliFlag`, `command` (`[executable, ...args]`, falling back to `index.js`/`index.ts` for external plugins), `channelEnv`, `fields` (`PluginFieldDescriptor[]`), `pairing` (`{ supported, actionLabel?, manualDisclosure?, privateIdentityField?, openUrlLabel? }`).
- **Secret Field & Non-Mutating Test Semantics**: Redacted `secret` fields arrive blank on `channel_list` with `configured: true`. Omitting or submitting an empty/unchanged secret preserves the existing stored secret; submitting a new non-empty value replaces it; clearing requires explicit reset authorization (`reset_hub` / `reset_user` — `Clear Token` sends `reset_hub: true, reset_user: false` so personal bindings survive). `Send Test` (`human_channels_test`) evaluates unsaved candidate form values without mutating stored credentials or bindings.

### 4.2 `agent-harness` Protocol (`stdin` JSON line → `stdout` JSON line)
Supports `setup_inspect`, `setup_apply`, `setup_remove`, `discover_sessions`, `resolve_caller`, `probe`, `update_badge`, `wake` (including two-step `checkpoint_only` / `queue_written` / `queue_confirmed` / `continue_wake`), and `stdio_sidecar` (`channelEnv` streaming). One-shot calls enforce a `10_000 ms` timeout (`120s` for `wake`, `1.5s` for `discover_sessions`/`update_badge`). Built-ins: `plugins/pi` (standard Pi Package + live bridge socket), `plugins/codex` (`sqlite_rows` with `busy_timeout(1000)`), `plugins/claude` (`experimental`, `cito-claude-hook` + `CITO_CLAUDE_CHANNEL` sidecar).

### 4.3 `human-channel` Persistent Protocol (`stdin`/`stdout` NDJSON)
Spawned by `HubEngine` when required `hub_config` is configured or on `human_channel_pair_start` / `human_channels_test`; exits within `< 100 ms` on `stdin EOF`.
- Host → Plugin: `configure`, `deliver` (with `already_sent_ids`), `on_acked`, `format_pair` (returns `pairing_command`, `instruction`, `bot_username`, `pairing_url`), `pair_ack`, `test`, `reply_ack`, `shutdown`.
- Plugin → Host: `delivered` (reporting `retryable`, `effect_unknown`, `external_msg_id`, `external_msg_ids`), `pair_redeem` (private chat verified), `inbound_reply`, `cursor_save`, `health` (reporting `getUpdates` polling failures such as HTTP 409 webhook conflicts or invalid token without auto-deleting webhooks), `rpc_result`.
- Built-in channel: **`plugins/telegram`** (only bundled external Human Channel).

---

## 5. Four-Surface macOS UI Architecture & Bounded IPC (`src/macos/cito-ui.swift`, `daemon.rs`)

### 5.1 Bounded UI IPC Protocol & Asynchronous Settlement
- **Outbound Daemon → UI (`<= 262,144` bytes / `256 KiB` UTF-8 budget)**: Frames `state`, `directory`, `settings`, `hub_invite`, `pair_code` (`request_id`, `pairing_url`, `bot_username`), `channel_test_result`, `composer_result`, `activity_message`, `error`. **No `"banner"` or `"hide_banner"` frames.** Field caps (`512B` message, `256B` about, `128B` title/cwd) and collection caps (`80` activity, `100` peers, `64` sessions/remote, `32` available) plus a final shedding loop enforce the `256 KiB` limit.
- **Inbound UI → Daemon (`<= 1,048,576` bytes / `1 MiB` line limit)**: `listen_session`, `unlisten_session`, `unlisten`, `unlisten_all`, `human_inbox_update`, `set_scope`, `rename_session`, `refresh_available`, `switch_hub`, `hub_action`, `set_host_hub`, `setup_save_hub`, `setup_toggle_hub`, `setup_remove_hub`, `paste_hub_invite`, `open_directory`, `close_directory`, `query_directory`, `send_signal`, `ack_signals`, `clear_activity_history`, `copy_activity_message`, `open_settings`, `close_settings`, `setup_apply`, `setup_remove_app`, `setup_disconnect_all`, `set_launch_at_login`, `save_hub_service`, `channel_save`, `channel_toggle`, `channel_unpair`, `channel_test`, `channel_pair`, `channel_cancel_pair`, `disconnect_human_channel`, `quit`. (`close_settings` and Hub removal/disable clear `active_settings_hub`.)
- **Asynchronous UI Settlement & Draft Preservation**: `UiMailbox` never lets passive `state`/`settings`/`directory` snapshot coalescing drop correlated command receipts (`request_id` on `settings`/`state`/`error`/`composer_result`/`hub_invite`/`pair_code`). UI panels reject stale responses whose `request_id` (or draft generation) does not match the active in-flight submission, clear pending spinners only on the matching receipt, and preserve user-edited form/composer drafts changed after submission.

### 5.2 Persistent Window Chrome & Four Coordinated Surfaces
1. **Persistent AppKit Root (`Directory` `560×440`, `Settings` `520×460`)**:
   - `ensureDirectoryPanel()` and `ensureSettingsPanel()` create one persistent `SurfaceView` (`cornerRadius: 14`, `cornerCurve: .continuous`) and `NSVisualEffectView(material: .popover, blendingMode: .behindWindow, state: .active, masksToBounds: true)` on `window.contentView`. `rebuildSettingsPanel()` mounts `settings_header`, `scroll`, and `settings_footer` directly on the persistent `SurfaceView` root (clearing only non-`NSVisualEffectView` subviews so direct `NSScrollView` traversal stays intact) and calls `window.invalidateShadow()` on presentation and size/opacity changes.
   - `MenuControl` synchronizes `accessibilityLabel` whenever `label.stringValue` / title mutates, supports `!isEnabled` dimming and `leadingDotView` (`6×6` drawn `SurfaceView` dots/rings replacing Unicode `"● "` / `"○ "`), and `inspectMenu()` exports `button_tooltip` and `button_accessibility_label` on the status item.
2. **Surface 1 — Menubar Status Item**:
   - Renders `.active` when `agentCount > 0 || unreadCount > 0` (`.warning` on Hub connection/error; `.idle` only when `agentCount == 0 && unreadCount == 0`), caps display text at `" 99+"`, and sets dynamic `status.button.toolTip` (`Cito · <N> session(s) listening [· <U> unread]` / `Personal inbox active` / `Inbox paused` / `Hub connection needs attention`).
3. **Surface 2 — Menu Popover (`380pt`)**:
   - **Header (`40pt`) & Hub Drawer (`32pt`)**: Subtitle priority: `hub.status != "connected"` (`Sign in required` / `Hub unavailable` / `Connecting…`) → `agentCount > 0` (`N session(s) listening`) → `human_paused == true` (`Inbox paused`) → `humanListening` (`Personal inbox active`) → `Not listening`. Hub drawer action `"Disconnect"`/`"Cancel"` renamed to **`Hub Settings…`** (`open_hub_settings:<hub>`).
   - **Zone 1A (`YOUR INBOX`)**: Visible even when Hub is offline/degraded/`unknown` (disabling `Pause Inbox`/`Resume Inbox` unless `hub.status == "connected"`). Active card shows `You (@user)` + `[Self/Team]` (OIDC/multi-user only) + **`[ Pause Inbox ]`**, and Line 2 structured summary (`Telegram · +1 Mac`, `Telegram · Activity`, `Activity · +1 Mac`, `Activity inbox` — never raw `.local` hostnames; full cleaned host list in `toolTip`). Paused card (`58pt`) shows `You (@user) · Paused`, `"Inbox paused · Activity & <channels> paused"` (or `"Activity alerts paused"`), and **`[ Resume Inbox ]`**.
   - **Zone 1B (`LISTENING SESSIONS`)**: Separate section header + Agent cards (`+ Listen` in `AVAILABLE SESSIONS` → `Pause` in Zone 1B, inline rename with `menuDidClose` reset, `Auto-Listen` toggle & shared `x = 46–48` parent-child keyline `width <= 244pt`), plus explicit empty-agent card when `agentListeners.isEmpty`.
   - **Zone 4 & Footer**: `Activity…` highlights with `infoText` tint + `bell.badge.fill` when `unreadCount > 0` (`clock.arrow.circlepath` + `History` when `0`). Footer contains **`[ Pause Agents ]`** (when `targetHub` `agentListeners.count > 0`, scoped via explicit `hub` on `unlisten_all`), `[ Settings… ]`, and `Quit Cito ⌘Q` (**no bell icon**).
4. **Surface 3 — Directory & Activity (`560 × 440pt`)**:
   - **Directory**: Scope pills **`All Nodes`** | **`Mine`** inside a **`129pt`** (`x: 16..145`) `directory_scope_track`, shifting `directory_facet_separator` to **`x: 151`** and `directory_harness_scroll` to **`(157, 3, 387, 26)`** (`r = 13`, symmetric `2pt` pill-track padding); Human rows read `Inbox · <attachments>` (e.g. `Inbox · laptop · Telegram` or `Inbox · Telegram`); **`⇧↵ Message`** shortcut supported in `settingsKeyMonitor` and `doCommandBy:` (`directory_shortcuts`: `"↵ Copy Address   ⇧↵ Message   ⎋ Close"`).
   - **Activity**: `✓ Read All` disabled when `unreadInCurrentFilter == 0`; clicking `Reply` marks unread signals read automatically; **`Clear…`** disabled when `activityFilteredItems.isEmpty`, transitioning the `32pt` facet bar into an inline confirm (`"Clear N matching messages?"` when searching else `"Clear N <filter> messages?"`, `[ Cancel ]` + solid red `.destructive` `[ Clear ]` anchored via `layoutBar(..., insetRight: 16)`, `Esc` cancels confirm before closing window, reset on mode/filter switch/`closeDirectoryPanel`/`openComposer`).
   - **Activity Rows & Composer**: Line 1 displays distinct sender session titles with protected right-aligned `relativeTime` and compact amber **`[Held: ...]`** pill mapping all 7 `held_reason` values (`stale_generation`, `driver_unavailable`, `session_switched`, `unlistened`, `unlistened_before_dispatch`, `fenced_before_dispatch`, `sender_ack_timeout`, hidden on cell reuse); `activityRowHeight = 44 + ceil(wrappedRect.height)` at `492pt`; Composer (`44pt` clean / `62pt` error) lays out input row via `layoutBar` at `y: 10` (or `y: 26` with error at `y: 5, x: 16, width: 528`), showing `"Sending…"` on `~76pt` `composer_send`, non-button `SurfaceView` `To:` badge, and **`Cancel`** (`⎋ Cancel Reply`) vs **`Close`** (`⎋ Close Draft`). Empty Activity search shows `"No matching messages"` + `Reset Search`; empty `For Me` with `activity_has_any` shows `"No messages addressed to you"` + `Show All Activity`.
5. **Surface 4 — Settings (`520 × 460pt`, 3 Tabs)**:
   - **`Agent Apps`**: Clean 2-line `settings_app:<id>` card (`Line 1`: badge + title + `[Experimental]` `minWidth: 0` + action buttons **`Set Up` / `Check & Repair` / `Repair` / `Disconnect`**; `Line 2`: `6×6` semantic status dot `successText`/`warningText`/`dangerText` + status text & detail) eliminating all `418pt` horizontal crowding, plus 1-line section subtitle.
   - **`Human Channels`**: Pure external channels (no desktop banner card), fitting all states without scrolling; top orientation note; `human_paused_banner` with `Resume Inbox` when `inbox_paused == true`; `ChannelSetupItem` projects `inbox_paused` / `delivery_enabled` with status precedence `error` > `paused` > `pairing_pending` > `inbox_paused` > `ready` > `hub_ready` > `not_configured` (so `inbox_paused` only masks `ready` without hiding `not_configured`, `hub_ready`, `pairing_pending`, or `paused`); dynamic `toggleWidth` for `Pause/Resume \(ch.display_name)`; `Pair Account…` primary `.prominent` CTA → `request_id`-correlated auto-copy (`/start <code>` with `pairCopiedUntil`) + manifest `openUrlLabel` (`ch.pairing_open_url_label ?? "Open Pairing Link ↗"`, opening `pairing_url`) + `Cancel` + collapsed `▸ Enter Chat ID manually` disclosure (symmetric `_external_*` population for positive private IDs); remote `Re-pair…` 1Hz polling; persistent recovery controls (`Retry` / `Change Token…` / `Clear Token` with `reset_user: false` / `Send Test` / `Unpair`) on all error states including `fields.isEmpty`; `[ Pause/Resume \(ch.display_name) ]` & `[ Unpair ]` on `userStratum` header bar during `Edit…` (`Cancel` + `Escape`).
   - **`Hubs & Network`**: `layoutBar` on `host_hub_card` top row (`flexible` title + passive `6×6` status indicator + **`Start Sharing`** / **`Stop Sharing`**, `toggleWidth` updated accordingly); `hostHeight` `+12pt` (`host_hub_addr_track` `y = 82`); `[ Copy Token ]` on `Port:` bar + disabled `Apply` when unchanged; `layoutBar` on `CONNECTED HUBS` header (**`Join from Clipboard`** + `+ Add Hub…`); `auto-scroll` `add_hub_form`; `[Default]` badge (`minWidth: 0`) on default Hub row; `effectiveTargetHub` click & last-Hub disable guard; `editingOriginalHub` & `NSNull` `user` clear; canonical OIDC keys (`issuer`/`clientId`) & `request_id`/`client_id` stripping.
   - **Footer & Modal**: **`Disconnect Agent Apps…`**; `uninstall_modal_overlay` (`0..460`) dimming `settings_header`, `scroll`, and `settings_footer` (`0.35` alpha), preserving `launch_at_login` when `purge_data == false`, and updating modal primary button dynamically between **`Disconnect Apps`** and **`Disconnect & Delete Data`**.
6. **Complete Tooltips & Unshadowing**: Remove internal `label.toolTip` shadowing on `header_hub_strip`, `toggle_group`, and `Directory…`/`Activity…` badges, and supply accurate `toolTip` + `accessibilityLabel` strings across every interactive control in all 4 surfaces.

---

## 6. SQLite Schemas & Security Authority

### 6.1 `${stateDir}/state.sqlite` (`user_version = 4`)
- `local_listeners (hub, listener_id, public_id, tag, principal_id, owner_user, host_id, host_name, harness, session_id, endpoint_pid, cwd, repo, branch, title, about, scope, status ['ACTIVE','REBINDING','UNLISTENED','FENCED_INACTIVE'], binding_generation, owner_token, persisted_seq, consecutive_probe_failures, replay_gap_detected, replay_gap_range, dispatcher_token, lease_until_ms, hub_url, updated_at, PRIMARY KEY (hub, listener_id))`
- `local_signals (hub, signal_id, listener_id, seq, binding_generation, from_user, from_host_name, from_harness, from_listener_id, from_title, from_about, message, held_reason, in_reply_to, created_at, expires_at_ms, state ['RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE','EXPIRED_UNSENT'], batch_id, batch_expires_at_ms, dispatcher_token, queue_written, queue_confirmed, acknowledged_at_ms, batch_card, activity_hidden, PRIMARY KEY (hub, signal_id))`
- `local_sent_idempotency`, `daemon_owner`, `pending_hub_acks`, `pending_hub_detaches`, `session_custom_titles`, `wake_retries`.

### 6.2 `${dataDir}/hub.sqlite` & OIDC Authority
- `hub_meta`, `hub_kv (key, revision, created_ms, value, principal_id, scope)`, `hub_signals` (with inline `claimed_by_channel`, `claim_until_ms`, `acked_at_ms`), `hub_consumers`, `hub_consumer_acks`, `hub_channel_deliveries`, `hub_channel_delivery_msgs`, `hub_plugin_services`, `hub_human_channels`, `hub_pairing_codes`, `hub_processed_replies`, `hub_audit_log`.
- **OIDC Verification & RLS**: Verifies JWTs against `.well-known/openid-configuration` / `jwks_uri`, enforces `principal_id` Row-Level Security on `self` nodes/events, and restricts `scope: "hub"` mutations to `CITO_OIDC_ADMIN_USERS` (or non-OIDC hosts).

---

## 7. Visual System, Brand Identity & Documentation Contract

### 7.1 Lightweight macOS Signal Glass & Precision Vector Palette (WCAG AA Verified)
- **Filled Controls, AppIcon & Brand Squircle (`#FFFFFF` foreground, `>= 4.5:1`)**:
  - Brand Cobalt Gradient: `#1B6EF3` (top, `4.59:1` vs `#FFFFFF`) → `#0057BF` (`6.74:1`) → `#0048A8` (bottom, `8.43:1`).
  - Solid Destructive Fill (`MenuControl.Style.destructive`, e.g. inline `[ Clear ]` and `[ Disconnect & Delete Data ]`): `CitoPalette.destructiveFill` (`#BD1924` / `sRGB(0.74, 0.10, 0.14)`) with `#FFFFFF` text (`5.9:1` on both Light and Dark surfaces).
- **Text & Link Accents**:
  - Dark canvas (`#0C0E12` / `#171B24` / Dark AppKit): `#38BDF8` / `#7DD3FC` (`8.04:1–11.59:1`) and AppKit `CitoPalette.infoText` (`sRGB(0.40, 0.74, 1.00)`).
  - Light surface (`#ECEFF3` / `#FFFFFF`): `#0057BF` (`5.84:1`).
- **Status & Warning**:
  - Ready / Human: `#34D399` (Dark) / `#0A6E29` (Light), with dedicated saturated `CitoPalette.badgeFill(_)` fills (`>= 3.0:1` vs white SF Symbols in both appearances).
  - Warning / Experimental / Held: adaptive `CitoPalette.warningText` (`#9C4200` Light `5.17:1` / `#FFB340` Dark `8.03:1`); `#F59E0B` is reserved strictly for warnings/experimental tags on dark web canvases, never as primary brand chrome.

### 7.2 Brand Assets, Icon & Website / Docs Synchronization
1. **AppIcon & Vector Marks**:
   - Regenerate `site/assets/art/app-icon-master.png` (`1024×1024` clean satin cobalt squircle `#1B6EF3 → #0048A8`, centered `#FFFFFF` C-arc & core + `#E0F2FE`/`#FFFFFF` outbound wave arc — zero metal bezel, screws, or amber LED) and compile all 10 `.iconset` slices into `bin/AppIcon.icns` + `site/apple-touch-icon.png`.
   - Update `site/assets/brand/{cito-mark,cito-logo-dark,cito-logo-light}.svg` and `site/favicon.svg` (foreground marks `#FFFFFF` / `#E0F2FE` on `#0048A8–#0057BF` so all SVG tokens pass `> 4.5:1` in `distribution.rs`; remove `paint-order` dark/light stroke outlines from wordmarks in **both** `cito-logo-dark.svg` and `cito-logo-light.svg`).
   - Delete the 4 unused heavy hardware rasters (`site/assets/art/{hero-stage,moment-cross-harness,moment-human-network,architecture-topology}.png`) and the 2 retired popup screenshots (`site/assets/screenshots/{banner-dark,wake-card-dark}.png`).
   - Regenerate `site/assets/og-card.png` (`1200×630` neutral slate dot-grid, new cobalt lockup, `brew install --cask tefx/tap/cito`, and refreshed `menu-dark.png`/`activity-dark.png` preview) and refresh `site/assets/art/architecture-topology.svg` (Cito Blue vector cards, no `#F59E0B` on Pi/Hub, `"Unread badge + 1-line reply"`).
2. **Website, README & Repo Docs Sync**:
   - Update `site/styles/{tokens,main}.css` (Cito Blue dark text accents `#38BDF8`/`#7DD3FC`, cobalt buttons `#0057BF`/`#1B6EF3` + `#FFFFFF`, remove `.docs-figure-stage--compact` & `Banner (388pt)` comment), `site/index.html` (Moment 02 → `Menubar Unread Badge & Inbox` + `1-Line Reply in Activity`, updated labels across Moments 01–03), `site/scripts/hero-preview.js`, `site/docs/{guide,architecture,plugins}.html` (drop `BannerWindow`, `Mac Desktop Banner`, `plugins/human`, and legacy labels), `site/assets/IMAGEGEN_PROMPTS.md`, `README.md` (removing all obsolete sections/banner/human-plugin prose and `docs/ROADMAP.md` link), and delete obsolete `docs/ROADMAP.md` (keeping `docs/DESIGN.md` and `docs/RELEASING.md` as the only repository specs).
   - Re-capture all 6 remaining Retina screenshots (`menu-dark.png`, `directory-dark.png`, `activity-dark.png`, `settings-apps-dark.png`, `settings-channels-dark.png`, `settings-hubs-dark.png`).

---

## 8. Verification Policy & Implementation Hazard Map

### 8.1 Verification (`MUST`)
- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release` (0 warnings), `node crates/cito-native/tests/support/site-clipboard.mjs`, and `python3 packaging/release.py check` + `python3 -m unittest discover -s packaging`.
- Visually inspect `AppIcon.icns`, `og-card.png`, `architecture-topology.svg`, and all 6 refreshed `site/assets/screenshots/*.png` via `read`, plus verify `§1.2` zero-hardcoding semantic inspection across `crates/cito-native/src/core/` and `src/macos/` (and keep `state_tests.rs` test fixtures vendor-neutral).
- Include discriminating tests for: two-Mac + Telegram detach vs `human_inbox_update` CAS conflict, Hub-clock TTL liveness, remote/Telegram-only `YOUR INBOX` projection, cross-Hub Directory retention (`matched_count == online_count`), `FENCED_INACTIVE` exclusion, CLI `cito inbox pause/resume` vs MCP `FORBIDDEN` on Human mutations, upgrade cleanup of retired `${stateDir}/{,runtime/}plugins/human`, inline `Clear…` confirm + `Cancel` + `Esc`, dynamic `MenuControl` `accessibilityLabel` & unshadowed `toolTip`, and relocated `Cito.app/Contents/Helpers/cito-ui` bundle execution.

### 8.2 Implementation Notes & Hazard Map (`ADVISORY`)
- Retarget legacy tests in `registration.rs`, `hub/cases.rs`, `tests/support/{daemon,ui,distribution,mod}.rs`, `bin/cito-test-runner`, `packaging/release.py`, `hub.rs:2235` (`GET /`), and `site-clipboard.mjs` to match the clean-slate `V40` contracts directly without adding compatibility wrappers.
- Update `inspectMenu()` in `cito-ui.swift` to drop banner/popup fields (`banner_items`, `wake_action_text`, `banner_hovered`, `banner_visible`, `CITO_BANNER_HEADLESS`) and export `button_tooltip` and `button_accessibility_label`.
