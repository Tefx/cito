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
  - Also supports `cito channel run --hub <url>` as a standalone CLI escape hatch connecting over HTTP/SSE.

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

---

## 7. Stage 4 (`v0.16.0`): Brand & Icon System (`AppIcon.icns`), Calm 4-Section Product Website (`site/`) & GitHub Pages (`https://tefx.github.io/cito/`)

### 7.1 Unified Brand Mark & macOS App Icon (`bin/AppIcon.icns`, `site/assets/brand/`, `README.md`)
1. **Canonical Geometric Brand Mark (`site/assets/brand/cito-mark.svg`, `cito-logo-dark.svg`, `cito-logo-light.svg`, `site/favicon.svg`, `site/apple-touch-icon.png`)**:
   - Directly matches `makeCitoStatusIcon` in `src/macos/cito-ui.swift` (`18 × 18` coordinate grid, center `(8, 9)`):
     - Open C-shaped transceiver arc: radius `5.50`, `+42°` to `-42°` (`M 12.09 5.32 A 5.50 5.50 0 1 0 12.09 12.68`, `stroke="#F3F4F6"`, `stroke-width="1.75"`, `stroke-linecap="round"`).
     - Central node core: `cx="8" cy="9" r="1.65"` (`fill="#F59E0B"`).
     - Outbound wake wave: radius `7.75`, `+28°` to `-28°` (`M 14.84 5.36 A 7.75 7.75 0 0 1 14.84 12.64`, `stroke="#F59E0B"`, `stroke-width="1.65"`, `stroke-linecap="round"`).
   - `README.md` top header embeds `<picture>` switching between `site/assets/brand/cito-logo-dark.svg` and `site/assets/brand/cito-logo-light.svg` with links to `https://tefx.github.io/cito/` and its `/docs/` pages.
2. **macOS Application Icon (`bin/AppIcon.icns` & `buildMacAppBundle` in `src/setup/index.ts`)**:
   - Generated from the pre-verified OpenAI Images 2.6 master `/tmp/cito-preview-art/app-icon-master.png` (`1024 × 1024` master squircle with bead-blasted dark graphite anodized aluminum body, recessed instrument dial, brushed silver C-arc, and warm amber core/wave) into all 10 standard `.iconset` slices (`icon_16x16.png`..`icon_512x512@2x.png`) and compiled via `/usr/bin/iconutil -c icns -o bin/AppIcon.icns` (`20 KB < size < 500 KB`).
   - `buildMacAppBundle` in `src/setup/index.ts`:
     - Creates `Contents/Resources` (`mkdir(resourcesDir, { recursive: true })`);
     - Copies `join(pkgRoot, "bin", "AppIcon.icns")` to `join(resourcesDir, "AppIcon.icns")` **before** running `codesign -s - --force --deep`;
     - Adds `<key>CFBundleIconFile</key><string>AppIcon</string>` and `<key>CFBundleIconName</key><string>AppIcon</string>` to `Contents/Info.plist`, keeping total `Cito.app` size `< 5 MB`.

### 7.2 Calm, Spacious 4-Section Product Homepage (`site/index.html`) vs. Technical Docs (`site/docs/`)
1. **Strict Homepage Restraint & Anti-AI-Slop Rules (`site/index.html`)**:
   - **Zero Internal Implementation Leaks on `site/index.html`**: No pixel/point dimensions (`380pt`, `560×440pt`, `px`), no port numbers (`:4290`, `:4291`), no SQLite table or cursor names (`consumer_ack`, `signal_claim_dispatch`, `hub_channel_deliveries`, `SQLite`, `WAL`), and no AppKit class names (`NSMenu`, `KeyPanel`).
   - **Zero AI-Slop Visuals or Copy**: No purple/cyan neon radial blurs, no emoji-topped feature grids, no banned buzzwords (`seamless`, `empower`, `supercharge`, `next-generation`, `reimagined`, `game-changer`, `synergy`), and no `Not X, but Y` contrastive templates.
   - **4 Spacious Sections with `>= 50%` Calm Negative Space**:
     - **Section 1 (Hero)**:
       - Headline: *"Where your agents talk—with you, and each other."*
       - Lead (2 sentences): *"Cito connects your sessions across Codex, Claude Code, and Pi—on one Mac or across your machines—so agents can discover and wake each other directly. And because you are on the same network via your Mac menu bar and Telegram, they bring you into the loop whenever a decision is needed."*
       - Actions (Strictly 2 installation paths — **Homebrew** first and **Download `Cito.app`** second; do **NOT** recommend `npm install` or require `&& cito setup` on the website or `README.md`):
         1. Primary 1-click copy command pill: `brew install tefx/tap/cito`
         2. Secondary button: `Download Cito.app for macOS`
       - Interactive Hero Stage framed over `./assets/art/hero-stage.png` with a 2-scene switcher: **`[ ⚡ Agent → Agent ]`** (Claude Code session waking an idle Codex session) and **`[ 💬 Agent ↔ You ]`** (clicking a menu bar notification, sending a 1-line inline reply, and watching it wake the agent and mark Telegram read).
     - **Section 2 (Three Everyday Moments)**:
       1. *Stop playing courier between your agents* (`Codex ⇄ Claude Code ⇄ Pi` direct cross-harness discovery & wake, accompanied by `./assets/art/moment-cross-harness.png`).
       2. *You are a node on the same network* (Read and reply from your Mac menu bar or Telegram on your phone, accompanied by `./assets/art/moment-human-network.png`).
       3. *From one Mac to your whole team* (Zero-config on one Mac, 1-switch local network sharing, or shared team hub).
     - **Section 3 (Built for Your Favorite Harnesses)**: Clean showcase of `Codex`, `Claude Code`, `Pi`, and `Telegram` with links to `./docs/guide.html` and `./docs/plugins.html`.
     - **Section 4 (Ready in Under a Minute)**: `brew install tefx/tap/cito` + `Download Cito.app for macOS` and minimal footer.
2. **Pre-Verified Codex `image_gen` (OpenAI Images 2.6) Raster Assets (`site/assets/art/`)**:
   - Copy the 4 pre-generated OpenAI Images 2.6 assets from `/tmp/cito-preview-art/{hero-stage.png,moment-cross-harness.png,moment-human-network.png,app-icon-master.png}` into `site/assets/art/` (and generate `site/assets/og-card.png` `1200 × 630` + `site/assets/IMAGEGEN_PROMPTS.md`).
3. **Dedicated Technical Documentation Pages (`site/docs/{guide,plugins,architecture}.html`)**:
   - `site/docs/guide.html`: 1-step installation via **1. Homebrew (`brew install tefx/tap/cito`)** or **2. Download `Cito.app`** (no `npm install` recommendation, no manual `cito setup` required because `Cito.app` and `cito daemon` auto-bootstrap `~/.cito/bin/` and detected Stable Agent Apps on startup and provide the visual `Settings…` `⌘,` window), 1-click Telegram pairing, and LAN/Team Hub setup.
   - `site/docs/plugins.html`: `@tefx/cito/plugin-sdk` authoring guide for both `kind: "agent"` (`HarnessAdapter`) and `kind: "human"` (`HumanChannelPlugin` with `scope: "hub" | "user"` fields and 6-char pairing).
   - `site/docs/architecture.html`: `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)` protocol, `@self` / `@<username>` addressing, multi-channel read sync, and the 5 MCP tools.

### 7.3 GitHub Pages Default Subpath (`https://tefx.github.io/cito/`, No `CNAME`) & Deployment Workflow
- **No `site/CNAME`**: Served at `https://tefx.github.io/cito/` with `site/.nojekyll`.
- **Relative Path Rule for Subpath Compatibility**: Every DOM link, stylesheet, script, and image in `site/index.html` (`./...`) and `site/docs/*.html` (`../...` and `./...`) MUST use relative paths (zero root-absolute `href="/..."` or `src="/..."`), while `<link rel="canonical">`, `<meta property="og:url">`, and `<meta property="og:image">` use full `https://tefx.github.io/cito/...` URLs.
- **GitHub Actions Workflow (`.github/workflows/pages.yml`)**: Deploys `./site` to GitHub Pages on push to `main` via `actions/upload-pages-artifact@v3` and `actions/deploy-pages@v4`.
- **Verification**: Serve `site/` under a `/cito/` prefix on a local HTTP test server (`tests/site/site.test.ts`), verifying HTTP `200` on all pages and linked assets, zero banned implementation tokens on `site/index.html`, valid `bin/AppIcon.icns` in `Cito.app/Contents/Resources/AppIcon.icns`, and `CITO_VERSION === "0.16.0"`.

---

## 8. Stage 5 (`v0.17.0`): Zero-Setup Startup Bootstrap, Homebrew & `Cito.app` Distribution, Conditional Apple Notarization & Open-Source Release Hygiene

### 8.1 Automatic First-Launch Bootstrap (Eliminating the Manual `cito setup` User Step)
- When `Cito.app` or `cito daemon` starts (`CitoService.create()` in `src/mcp/service.ts`):
  1. Calls `ensureCliShims(config)` to keep `${stateDir}/bin/{cito,cito-mcp,cito-claude-hook}` up to date;
  2. Automatically runs non-destructive first-launch auto-setup for detected Stable Agent Apps (`tier === "stable"`, i.e., `codex` and `pi`, with `force: false` so existing user configs are never overwritten), while users can inspect or toggle any integration visually in `SettingsPanel` (`⌘,`).
  3. As a result, users installing via **`brew install tefx/tap/cito`** or **`Cito.app`** never need to run `cito setup` in a terminal!

### 8.2 Two Official User-Facing Distribution Channels (Homebrew First, `Cito.app` Download Second)
1. **Priority 1 — Homebrew (`packaging/homebrew/cito.rb`)**:
   - Installs `Cito.app` and links `cito`, `cito-mcp`, and `cito-claude-hook` into `#{bin}/` using version-invariant `/opt/homebrew/opt/cito/...` paths (`resolveStablePackageRoot()` in `src/setup/index.ts` maps `/Cellar/<name>/<version>/` to `/opt/<name>/` so `brew upgrade cito` never breaks `Cito.app` or LaunchAgent paths).
   - Validated in automated tests via `/usr/bin/ruby -c packaging/homebrew/cito.rb` and version parity assertions (`version === CITO_VERSION`).
2. **Priority 2 — Self-Contained `Cito.app` Archive (`Cito-macos-arm64.zip` & `.github/workflows/release.yml`)**:
   - `buildMacAppBundle` (`src/setup/index.ts`) bundles a relocatable `Cito.app` (resolving bundled `Contents/Resources/app/bin/cito-mcp` and `/opt/homebrew/opt/cito/...` before any build-time fallback path) and strips `com.apple.quarantine` (`xattr -cr`).
   - Supports conditional Apple Developer ID signing and notarization via `CITO_CODESIGN_IDENTITY`:
     - When `CITO_CODESIGN_IDENTITY` (or GitHub Actions `APPLE_SIGNING_IDENTITY`) is set: signs with `--force --options runtime --timestamp --sign "$CITO_CODESIGN_IDENTITY"`, submits to `xcrun notarytool submit --wait`, and staples with `xcrun stapler staple`.
     - When unset (default before joining Apple Developer Program): signs ad-hoc (`codesign -s - --force --deep`).
3. **Open-Source Release Hygiene (`LICENSE`, `README.md`, `tests/setup/distribution.test.ts`)**:
   - Add root `MIT` `LICENSE` file and `"license": "MIT"` in `package.json` (preserving `pkg.files` and `pkg.scripts` keys intact).
   - Bump `CITO_VERSION`, `package.json`, and `crates/cito-native/Cargo.toml` to `"0.17.0"`.
   - Verify across `README.md`, `site/index.html`, and `site/docs/guide.html` that only the two official install methods (**1. `brew install tefx/tap/cito`** and **2. Download `Cito.app`**) are presented as user installation paths, with zero `npm install` or `&& cito setup` user onboarding steps.
