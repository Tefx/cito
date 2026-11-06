# Cito Architecture & Protocol Specification (V9 — Multi-Hub Session Attachment, Atomic Hub Migration, Ego-Centric Menu Bar Control Plane & Spotlight Network Directory)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for coding agents that wakes or delivers messages to opted-in agent conversations across:
1. **Multiple NATS Hubs** (e.g., a personal Tailscale no-auth hub `personal` and a team OIDC hub `work` simultaneously),
2. **Users** (e.g., `alice`, `bob`, scaling to $U = 50\dots 500+$ team members),
3. **Hosts / Machines** (e.g., `macbook-work`, `macbook-personal`),
4. **Extensible Agent Harnesses** (shipped in code: **Pi Coding Agent**, **ChatGPT / Codex Desktop App** and **Claude Code** (**Section 6**); open extensible adapter interface for further harnesses), and
5. **Concurrent Conversations** within the same repository or working directory (scaling to $N = 100\dots 2,000+$ online agents across a network).

### 1.2 Language, Neutral Naming & Complete-Integration Policy
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, directory panel, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names (such as `rda`, `casdoor`, `rcatwin`, `keppel`, `mingchen`) may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **No Half-Baked Harness Integration in Source**: A harness named in Section 1.1 MUST ship complete: a registered adapter, the host half that binds its endpoint, the delivery effect that actually reaches the model, and tests that exercise the whole path. A harness that cannot meet that bar stays out of `src/**` and `tests/**` entirely and is documented as a design suggestion instead. For Claude Code (Section 6) the complete set is `src/adapters/claude.ts`, `src/adapters/claude-channel.ts`, `src/claude-hook/cli.ts`, `bin/cito-claude-hook`, `registerAdapter(new ClaudeCodeAdapter())` in `src/adapters/index.ts`, `claudeHome` / `enableClaudeChannel` in `src/shared/config.ts`, the channel start in `src/mcp/cli.ts`, `case "claude":` in `src/macos/cito-ui.swift`, and `tests/adapters/claude.test.ts`, `tests/adapters/claude-channel.test.ts`, `tests/claude-hook/cli.test.ts` plus the stdio channel case in `tests/e2e/lifecycle.test.ts`.
- **Opt-In Host Channels**: A host channel that binds a second bridge socket from inside another process (Section 6) MUST be explicitly opt-in (`CITO_CLAUDE_CHANNEL=1`). Auto-detection from host environment variables is forbidden: it would bind an unrelated harness's endpoint and silently capture its caller binding.

---

## 2. Module Boundaries, Pure Adapter Contract & Scalable NATS KV Discovery

### 2.1 Acyclic Module Graph
`src/shared/` $\to$ `src/adapters/` & `src/ui/` $\to$ `src/mcp/`

- **`src/mcp/server.ts` Purity Invariant**: `src/mcp/server.ts` MUST NOT import `codexThreadId` or reference `"x-codex-turn-metadata"`, `"codex"`, or `"pi"`. It passes `{ meta, headers, httpClientPid, httpTransport: true }` into `service.call(name, args, context)`.
- **Protocol Version Negotiation (`src/mcp/server.ts`)**:
  - Registers `'2026-07-28'` in `SUPPORTED_PROTOCOL_VERSIONS` alongside `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`, and `2024-10-07`.
- **Default Registered Adapters (`src/adapters/index.ts`)**:
  - Registers `new CodexDesktopAdapter()`, `new ClaudeCodeAdapter()` and `new SocketBridgeAdapter("pi")` at startup, while `getAdapter(harness)` dynamically instantiates `new SocketBridgeAdapter(harness)` for any valid custom harness ID matching `^[a-z][a-z0-9_-]{0,31}$`.
  - Per-harness candidate ordering in `discoverAllAvailableSessions` ranks `pi` $\to$ `codex` $\to$ `claude` $\to$ custom harnesses alphabetically.

### 2.2 Scalable NATS KV Discovery (`HubDirectoryIndex` in `src/mcp/service.ts`)
To scale `cito_find` and UI discovery to $U = 500+$ users and $N = 2,000+$ online agents without $O(K)$ sequential `broker.kv.get(key)` round-trips or $O(U \cdot N)$ continuous heartbeat broadcast fanout to idle desktops:

1. **Bounded Dual-Termination `broker.kv.watch()` Snapshot + Fallback + Coalesced TTL Cache (`HubDirectoryIndex`)**:
   - Each `HubSlot` owns a `HubDirectoryIndex` caching active `IndexedRegistration` entries (`{ hub, value: Registration, revision, createdIso, createdMs, haystack }`) with a `5,000 ms` (`5s`) freshness window, singleflight promise coalescing (`inflight?: Promise<void>`), and a `directoryGen` counter.
   - **Non-Hanging Snapshot Sync Algorithm (`syncSnapshot`)**:
     1. Increment/capture `const gen = this.directoryGen`. Call `const info = await broker.jsm.streams.info(KV_STREAM)` (`"KV_cito_listeners"`) once to obtain both `brokerTs` (via `info.ts`, anchored to `const sampleMonoMs = performance.now()`) and `info.state.messages` / `const targetSeq = info.state.last_seq`.
     2. If `info.state.messages === 0`, commit an empty `Map` (if `gen === this.directoryGen`) and return immediately without creating a consumer.
     3. **Primary Fast Path (`watch` with `initializedFn` + `targetSeq` + `2,500 ms` deadline)**:
        - Open `broker.kv.watch({ ignoreDeletes: false, initializedFn: () => { initDone = true; watch?.stop(); } })` under a `2,500 ms` timeout timer (which stops a late-resolving watcher and aborts iteration).
        - If `initDone` already fired synchronously before `await broker.kv.watch()` returned, call `watch.stop()` immediately after assignment and drain any already-queued entries.
        - In `for await (const entry of watch)`:
          - If `entry.operation === "PUT"`, parse `registrationSchema.parse(entry.json())` (skipping malformed records) and store in `next` with pre-lowercased `haystack`; if `entry.operation === "DEL" || entry.operation === "PURGE"`, `next.delete(entry.key)`.
          - Outside the `PUT` branch, check termination on every entry: if `initDone || entry.delta === 0 || entry.revision >= targetSeq`, `break` and call `watch.stop()` in `finally`.
     4. **Guaranteed Non-Hanging Fallback (`boundedPointGetSnapshot`, concurrency `= 16`)**:
        - If the `watch` path throws or hits its `2,500 ms` deadline, discard its partial map and execute a `2,500 ms` deadline-bounded `broker.kv.keys()` + **16-worker concurrent `broker.kv.get(key)` pool**. If the fallback also fails, surface the hub error/warning without marking the cache fresh.
     5. **Fenced Commit**:
        - Only if `gen === this.directoryGen`, commit `this.entries = next`, `this.anchorBrokerMs = Date.parse(brokerTs)`, `this.anchorMonoMs = sampleMonoMs`, and `this.lastSyncMonoMs = performance.now()`.
2. **Immediate Local Mutation Coalescing & Read-Time Lease Re-Evaluation**:
   - On `HubSlot.disconnect()`, `close()`, or `forceLogin` account switch, increment `directoryGen` and clear `HubDirectoryIndex`.
   - On local `listen()`, `unlistenById()`, `setScope()`, or `Receiver.revoke()` / `Receiver.sweep()` heartbeat renewal, immediately upsert or remove the affected key in `slot.directoryIndex` so `cito_find` and UI queries observe local mutations with zero TTL lag.
   - On every read from `HubDirectoryIndex`, compute `const currentBrokerIso = new Date(this.anchorBrokerMs + (performance.now() - this.anchorMonoMs)).toISOString()` and filter entries by `evaluateLease(item.value.state, currentBrokerIso, new Date(item.createdMs)) === "ACTIVE"` and caller visibility (`item.value.scope === "workspace" || item.value.principal_id === principal.principal_id`).

### 2.3 Actionable Pi Footer Status Badge (`src/mcp/receiver.ts`)
- **Signal Detection Theory (Green & Swets, 1966 — False Alarm Elimination)**:
  - `refreshSessionBadge()` in `src/mcp/receiver.ts` MUST NOT append `"!"` when `row.replay_gap_detected === 1`.
  - The badge strictly encodes unread notification count:
    ```ts
    const count = unread > 0 ? ` (${unread})` : "";
    await adapter?.updateBadge?.(target, `🔔 ${badgeSlug}${count}`, config, options.signal);
    ```
  - `replay_gap_detected` and `replay_gap_range` remain stored in SQLite (`local_listeners`) and returned by `cito_inbox` for structured diagnostics. (Update the 3 badge strings in `tests/mcp/identity-and-badge.test.ts:112, 114, 117` from `"🔔 abcdefghijkl (!)"` / `"🔔 abcdefghijkl (1!)"` to `"🔔 abcdefghijkl"` / `"🔔 abcdefghijkl (1)"` while asserting `cito_inbox` still reports `replay_gap_detected: true`).

---

## 3. Session Sorting & Candidate Discovery Rules (`src/pi-extension/cito-bridge.ts`, `src/adapters/`)

### 3.1 Pi Interaction Recency Tracking (`lastActiveMs`)
1. **In `src/pi-extension/cito-bridge.ts`**:
   - Maintain `let lastActiveMs = Date.now();` in the extension closure.
   - **Important Polling-Stability Invariant**: Do **NOT** update `lastActiveMs = Date.now()` unconditionally inside `update(ctx)`, because `handle(req)` calls `update(ctx)` on every read-only `{ op: "identify" }` and `{ op: "status" }` probe! Inside `update(ctx)`, update `lastActiveMs = Date.now()` only when `nextSessionId !== sessionId`.
   - Advance `lastActiveMs = Date.now()` in actual lifecycle/user/agent event handlers (`session_start`, `session_info_changed`, `session_tree`), `/cito` command execution, and accepted `deliver` notifications.
   - In `handle(req)` for `req.op === "identify"`, include `updated_at_ms: lastActiveMs` in the response object.
2. **In `SocketBridgeAdapter.discoverAvailableSessions` (`src/adapters/pi.ts`)**:
   - Compute each live session's `updated_at_ms` as:
     `typeof info.updated_at_ms === "number" && Number.isFinite(info.updated_at_ms) && info.updated_at_ms > 0 ? Math.round(info.updated_at_ms) : Math.round(st.mtimeMs)`
   - Sort `Pi` candidates by `updated_at_ms DESC, endpoint_pid DESC` so **the Pi terminal window the user interacted with most recently always appears at the top of the `Pi` group**.

### 3.2 Codex Desktop Candidate Sorting (`src/adapters/codex.ts`)
- When `await (config.codexDesktopRunning ?? codexDesktopRunning)(signal)` is `true`, query `state_5.sqlite` for unarchived user threads (`archived = 0`, `COALESCE(thread_source, 'user') = 'user'` when `thread_source` exists, `COALESCE(source, '') NOT LIKE '{"subagent":%'` when `source` exists).
- Order by `recency_at_ms DESC, updated_at DESC, rowid DESC` (`LIMIT 8`).

### 3.3 Per-Harness Candidate Quota (`discoverAllAvailableSessions` in `src/adapters/index.ts`)
- Exclude any candidate matching an active local listener by `(harness, session_id)` or `(harness, endpoint_pid)`.
- Retain up to **8 candidates per harness** (ordered with `"pi"` first, `"codex"` second, and any other harness alphabetically; max `16` total) so opening multiple Pi windows never starves the Codex group or vice versa.

---

## 4. Two-Surface macOS UI Architecture for Large-Scale Networks (`src/ui/index.ts`, `src/mcp/service.ts`, `src/macos/cito-ui.swift`)

### 4.1 HCI & Systems Rationale: Separating Ego-Centric Control (`NSMenu`) from Allo-Centric Search (`NSPanel`)
1. **Proxemic Locality & Bounded Personal Complexity ($O(M_{\text{personal}})$ vs. $O(N_{\text{network}})$)**:
   - Even in an organization with $U = 500$ users and $N = 2,000$ online agents, a single human user owns only $M_{\text{personal}} \approx 1\dots 6$ sessions across their local Mac and their personal remote hosts (`principal_id === slot.principal.principal_id`).
   - Keeping the `380pt` status-bar `NSMenu` strictly **Ego-Centric** (1. Active on This Mac, 2. My Other Hosts, 3. Available on This Mac, 4. Network Directory Launcher) guarantees that clicking the menu bar icon is always instantaneous, compact, and never steals keyboard focus from the active terminal/IDE.
2. **Information Foraging & AppKit Key-Window Physics for $N = 2,000$ Team Peers**:
   - In a 500-person team, users forage for agents by **task title, repository (`acme/billing`), branch, host, or `@user`**. Navigating 500 collapsed user headers inside a non-key `NSMenu` (`canBecomeKey == false`) fails.
   - Clicking **`[ 🔍 Search Network Directory… ]`** in the menu opens a dedicated, key-capable **Spotlight-style Floating `DirectoryPanel` (`560 × 440 pt` `KeyPanel: NSPanel`, `canBecomeKey == true`)** with an auto-focused `NSSearchField`, facet filter pills, and a cell-reusing `NSTableView` (`rowHeight = 52pt`).

---

### 4.2 Surface 1: Ego-Centric Menu Bar Dropdown (`380 pt` `NSMenu`)

```text
┌──────────────────────────────────────────────────────────────────────┐ ← 380pt wide
│  Cito   1 listening · 2 remote                             [ ● 🌐 ▾ ] │ ← Header (40pt, id="header")
├──────────────────────────────────────────────────────────────────────┤
│ ┌──────────────────────────────────────────────────────────────────┐ │ ← Zone 1: Active on This Mac
│ │ ┌──────┐  dotfiles                     [ 🔒 Self ]  [ ↗ ]  [ ✕ ] │ │   (id="session:<scoped_id>",
│ │ │  >_  │  Local dotfiles/agent Pi main session                   │ │    360×72pt inside 380×78pt)
│ │ └──────┘  [ ⧉ dotfiles#f8k2 ]   ~/dotfiles/agent · main          │ │
│ └──────────────────────────────────────────────────────────────────┘ │
├──────────────────────────────────────────────────────────────────────┤
│  ▾  MY OTHER HOSTS · 2                                 (collapsible) │ ← Zone 2: My Remote Sessions
│     💻 MacBook-Personal · cito · main              [ ⧉ cito#8f3a ]   │   (id="my_remote_header" +
│     💻 MacBook-Personal · larva                    [ ⧉ larv#4d2e ]   │    34pt rows id="remote:<scoped_id>")
├──────────────────────────────────────────────────────────────────────┤
│  AVAILABLE SESSIONS                                                  │ ← Zone 3: Local Candidates
│  ▾  [>_] Pi · 2 open                                   (collapsible) │   (id="group:pi", 28pt)
│     cito · ~/Projects/cito · main                       [ + Listen ] │   (id="candidate:pi:...", 36pt,
│     larva · ~/Projects/larva · feat/ui                  [ + Listen ] │    x:24..298 icon-free text)
│  ▸  [✦] Codex · 5 recent                                 (collapsed) │   (id="group:codex", 28pt)
├──────────────────────────────────────────────────────────────────────┤
│  [ 🔍 Search Network Directory…                        412 online ]  │ ← Zone 4: Directory Launcher (34pt)
├──────────────────────────────────────────────────────────────────────┤
│  [ ■ Stop All ]                                     [ Quit Cito ⌘Q ] │ ← Footer (36pt, id="footer")
└──────────────────────────────────────────────────────────────────────┘
```

#### Appearance, Contrast, and Hover Behavior

Controls use four Apple HIG-oriented styles with distinct contrast and action hierarchy. Borders are `1 pt` unless omitted:

| `MenuControl.Style` | Resting | Hovered | Uses |
| --- | --- | --- | --- |
| `.neutral` | `labelColor` fill at `0.09`, border at `0.14`; `normalColor` label and primary symbol, `secondaryLabelColor` accessory | `labelColor` fill at `0.18`, border at `0.28`; label and owned symbols use `labelColor` | Hub pills, secondary drawer actions, Copy, Self, Open, Listen, Directory launcher, Stop All, Quit, inactive facets, Close |
| `.prominent` | `systemBlue` fill at `0.88`, white border at `0.22`, white label and symbols | Solid `systemBlue`, white border at `0.36`, white label and symbols | Primary Sign In / Connect action, Team scope, active Directory facets |
| `.subtleHeader` | Clear fill, no border; `normalColor` label, `secondaryLabelColor` chevrons | `labelColor` fill at `0.10`, no border; label and chevrons use `labelColor` | My Other Hosts and collapsible harness-group headers |
| `.destructiveHover` | `labelColor` fill at `0.09`, border at `0.14`, `secondaryLabelColor` symbol | `systemRed` fill at `0.88`, white border at `0.26`, white symbol | Individual Stop listening controls |

- Neutral hover stays achromatic in Light and Dark Mode. Header labels rest in `secondaryLabelColor` for My Other Hosts and `labelColor` for harness groups. Owned leading, center, and accessory symbols receive control styling; nested harness badges and status dots retain their colors.
- Copy feedback synchronously shows `✓ Copied` in `systemGreen`, with green fill at `0.16` and border at `0.36`. Its `1.2 s` timer runs in common and event-tracking modes, then restores the original label, geometry, and current hover style.
- `SurfaceView` retains semantic fill/stroke colors and resolves their `CGColor` values under `effectiveAppearance` at initialization, layer updates, and appearance changes. `withAppearanceAlpha` defers alpha application through an `NSColor` dynamic provider: AppKit's eager `withAlphaComponent` can freeze semantic RGB values before a later appearance switch. Corner radius is available immediately for headless inspection.
- Harness badges use `systemIndigo` / `terminal.fill` for Pi, `systemTeal` / `sparkles` for Codex, and `systemBlue` / `bolt.horizontal.fill` for other harnesses. All have tint fill at `0.88`, a white border at `0.18`, and a white icon. Zone 1 uses `34 × 34 pt`, radius `8`, inset `8`; group headers use `18 × 18 pt`, radius `5`, inset `3`.
- Each control retains one visible-rect tracking area. A weak single-active-hover coordinator clears the previous control when another enters. Screen-coordinate reconciliation checks window visibility, hidden ancestors/menu items, and the control's visible rect on local pointer/drag/scroll/button events and a `0.08 s` timer in common and event-tracking modes. The timer stops when no active hover remains.
- Menu rebuild, open, and close clear hover only on menu-owned controls or controls outside the Directory panel. Toggling a drawer reconciles visibility immediately. Closing the Directory clears its own hover; an unrelated menu rebuild or close preserves hover on unchanged Directory controls.
- Headless hooks exercise these logical lifecycle transitions without a physical pointer or visible window. Appearance tests switch real AppKit views between Aqua and Dark Aqua and inspect resolved layer colors; they do not claim physical cursor-event coverage.

#### Exact Menu Bar Zones & Geometry (`src/macos/cito-ui.swift`):
1. **Header Bar (`380 × 40 pt`, `id = "header"`)**:
   - Hub drawer detail uses `labelColor` for errors or authentication-required status and `secondaryLabelColor` for other status text. Primary drawer actions use `.prominent`; other drawer actions and hub pills use `.neutral`.
   - Left: **`Cito`** (`13pt .bold`) + dynamic subtitle (`"1 listening"`, `"1 listening · 2 remote"`, `"Idle · 2 remote"`, or `"Idle"`). Compute `subWidth = ceil(measureTextWidth(subtitleText, font: .systemFont(ofSize: 11.5))) + 8`, set subtitle width to `subWidth`, and place `hubStrip` at `x = max(138, 54 + subWidth), width = 368 - max(138, 54 + subWidth)` so the subtitle is never clipped.
2. **Zone 1 — Active on This Mac (`listeners`)**:
   - `360 × 58/72 pt` rounded cards (`id = "session:\(listener.scoped_id)"`), with radius `10`, `labelColor` fill at `0.07`, and border at `0.11`. Path/branch text uses `secondaryLabelColor`. Capped inside `active_scroll` (`260pt` `FlippedView` `NSScrollView`) when `listeners.count > 4`.
   - **Multi-Hub Card Switch Pill (`switch_hub:<scoped_id>`, rendered only when `hubs.count > 1`)**:
     - When `hubs.count <= 1`, no hub switch pill is rendered, keeping `actionWidth` (`114pt` when `can_jump` else `88pt`) and `scope:<scoped_id>` origin (`x = 236` when `can_jump` else `x = 262`) unchanged.
     - When `hubs.count > 1`, `title` displays `listener.title` (without a static `" · \(listener.hub)"` suffix), and an interactive `MenuControl` pill (`id = "switch_hub:\(listener.scoped_id)"`, `style = .neutral`, `height = 22 pt`, `radius = 6 pt`, `width = min(76, max(46, ceil(measureTextWidth(listener.hub, font: btnFont)) + 22))`) is placed `4 pt` to the left of `scope:\(listener.scoped_id)`. The pill contains a `6 × 6 pt` status dot (`x: 7, y: 8, radius: 3`) reflecting that hub's connection status and `listener.hub` text starting at `x: 16`. `actionWidth` includes `hubPillWidth + 4` so `title` width (`350 - 54 - actionWidth - 6`) never overlaps the right-aligned controls.
     - Clicking `switch_hub:<scoped_id>` selects the next target hub in `hubs` ring order (`(currentIndex + 1) % hubs.count`, preferring connected hubs where the same `(harness, session_id)` is not already listening, falling back to the next hub with `name != listener.hub`), sets `pendingMutation = true`, and emits `["op": "switch_hub", "from_hub": listener.hub, "to_hub": nextHub, "listener_id": listener.listener_id]`.
   - When `listeners.isEmpty && !available.isEmpty`, renders the `380 × 34 pt` compact strip (`id = "empty"`, `"No active listeners — click + Listen below"`).
3. **Zone 2 — My Other Hosts (`my_remote_listeners`, shown when `!myRemote.isEmpty`)**:
   - Contains only active registrations from connected hubs where `reg.principal_id === principal.principal_id && reg.host_id !== this.hostId`.
   - **Collapsible Header (`380 × 26 pt`, `id = "my_remote_header"`)**: Top `1pt` hairline divider + `MenuControl` (`id = "toggle_my_remote"`) with `chevron.down` / `chevron.right` and `"MY OTHER HOSTS · \(myRemote.count)"` (`10pt .semibold`, `.secondaryLabelColor`). Clicking toggles `myRemoteCollapsed` in-place.
   - **My Remote Rows (`380 × 34 pt`, `id = "remote:\(peer.scoped_id)"`, shown when `!myRemoteCollapsed`)**:
     - Left (`NSRect(x: 16, y: 10, width: 14, height: 14)`): SF Symbol `"desktopcomputer"` (`.secondaryLabelColor`).
     - Right (`NSRect(x: 368 - copyWidth, y: 7, width: copyWidth, height: 20)`): `MenuControl(short, ..., leadingSymbol: "doc.on.doc")` (`id = "copy_remote:\(peer.scoped_id)"`), copying `peer.scoped_id` with `"✓ Copied"` feedback.
     - Center (`NSRect(x: 36, y: 8, width: 368 - copyWidth - 44, height: 18)`): `"\(cleanHost) · \(peer.title)" + (branchSuffix)` (`12pt .regular`, `.labelColor`, `toolTip = "\(peer.scoped_id) — \(peer.about)"`).
     - Capped inside `my_remote_scroll` (`102pt` = 3 rows) when `myRemote.count > 3`.
4. **Zone 3 — Available Sessions with Per-Hub Target Selector & Collapsible Harness Groups (`!available.isEmpty`)**:
   - **Multi-Hub Candidate Retention (`listening_hubs`)**:
     - `AvailableSessionSummary` (`src/adapters/types.ts`, re-exported from `src/ui/index.ts`) includes optional `listening_hubs?: string[]`.
     - Both `discoverAllAvailableSessions` (`src/adapters/index.ts`) and `CitoService.pushUiState()` (`src/mcp/service.ts`) compute `listening_hubs` for each local candidate session from matching active listeners (`r.harness === c.harness && (r.session_id === c.session_id || (c.endpoint_pid !== undefined && r.endpoint_pid != null && r.endpoint_pid === c.endpoint_pid))`). A candidate is excluded only when it matches an active listener without a `hub` field (legacy unit-test callers) or when `listening_hubs` covers all configured hubs (`Object.keys(config.hubs)` / `this.slots.size`).
   - **Section Header (`380 × 24 pt`, `id = "available_header"`)**:
     - Top `1pt` divider (`NSRect(x: 12, y: 21, width: 356, height: 1)`) + `"AVAILABLE SESSIONS"` (`10pt .semibold`, `width: 220` when `hubs.count <= 1`, `width: 124` when `hubs.count > 1`).
     - **Target Hub Selector Pills (`target_hub:<hub>`, rendered when `hubs.count > 1`)**:
       - `Companion` tracks `selectedTargetHub: String = ""`, with `effectiveTargetHub` resolving `selectedTargetHub` if present in `hubs`, else the first connected hub with at least one unmounted candidate (`!($0.listening_hubs ?? []).contains(hub.name)`), else the first connected hub or `hubs.first?.name ?? ""`.
       - Right side of `available_header` hosts a horizontal `NSScrollView` (`NSRect(x: 140, y: 2, width: 228, height: 18)`, `drawsBackground = false`, hidden scrollers) containing `18 pt`-tall (`radius: 9`) `MenuControl` pills (`id = "target_hub:\(hub.name)"`, `style = hub.name == effectiveTargetHub ? .prominent : .neutral`). Clicking a pill sets `selectedTargetHub = hub.name` and calls `rebuild()`.
   - **Per-Target-Hub Candidate Filtering**:
     - Candidates displayed under Zone 3 are filtered by `visibleAvailable = available.filter { !($0.listening_hubs ?? []).contains(effectiveTargetHub) }`.
     - When `!available.isEmpty && visibleAvailable.isEmpty` (all discovered local sessions are already listening on `effectiveTargetHub`, while another configured hub still has unmounted sessions), `available_header` remains visible and a compact `380 × 28 pt` status row (`id = "available_empty_target"`, `"All sessions listening on \(effectiveTargetHub)"`) is rendered below it so the user can switch `target_hub:<hub>`.
   - **Collapsible Harness Group Header (`380 × 28 pt`, `id = "group:\(harness)"`)**:
     - Partition `visibleAvailable` by `harness` in deterministic order (`"pi"`, `"codex"`, then any other harness alphabetically).
     - Interactive `MenuControl` (`id = "toggle_group:\(harness)"`, `NSRect(x: 8, y: 2, width: 364, height: 24)`, `radius: 6`):
       - Disclosure chevron (`x: 6, y: 7, width: 10, height: 10`): `collapsedGroups.contains(harness) ? "chevron.right" : "chevron.down"`.
       - Filled harness icon badge (`18 × 18 pt` at `x: 20, y: 3, radius: 5`, tint fill at `0.88`, white border at `0.18`, white icon): `"terminal.fill"` (`.systemIndigo`) for `pi`, `"sparkles"` (`.systemTeal`) for `codex`, `"bolt.horizontal.fill"` (`.systemBlue`) for others.
       - Group label (`NSRect(x: 44, y: 4, width: 310, height: 16)`, `11.5pt .semibold`): `"Pi · \(items.count) open"` for `pi`, `"Codex · \(items.count) recent"` for `codex`, `"\(harness.capitalized) · \(items.count) available"` for others.
       - Clicking toggles `collapsedGroups` membership for `harness` and calls `rebuild()` in-place.
   - **Icon-Free Candidate Rows (`380 × 36 pt`, `id = "candidate:\(c.harness):\(c.session_id)"`, shown when `!collapsedGroups.contains(harness)`)**:
     - Text label (`NSRect(x: 24, y: 9, width: 274, height: 18)`): `"\(c.title) · \(shortCwd)\(branchSuffix)"` (`12pt .regular`, `.byTruncatingMiddle`, `toolTip = "\(c.title) — \(c.cwd)"`).
     - Right button (`NSRect(x: 304, y: 7, width: 64, height: 22)`): `MenuControl("Listen", ..., leadingSymbol: "plus")` (`id = "listen:\(c.harness):\(c.session_id)"`). When `hubs.count > 1 && !effectiveTargetHub.isEmpty`, clicking emits `"hub": effectiveTargetHub` in the `listen_session` action payload; when `hubs.count <= 1`, `"hub"` is omitted for single-hub backward compatibility.
     - **Scroll Container ID Rule**: When `items.count > 4` within an expanded group, host its candidate rows inside an `NSScrollView` (`width: 380, height: 144 pt`, `FlippedView` document height `items.count * 36 pt`) with `id = groupedCandidates.count == 1 ? "available_scroll" : "available_scroll:\(harness)"` (preserving `"available_scroll"` when a single group is present and disambiguating `"available_scroll:\(harness)"` when multiple groups exist).
5. **Zone 4 — Network Directory Launcher Row (`380 × 34 pt`, `id = "directory_row"`, rendered when `!hubs.isEmpty`)**:
   - Top `1pt` hairline divider (`x: 12, y: 32, width: 356`).
   - Full-width `MenuControl` (`id = "open_directory"`, `NSRect(x: 10, y: 4, width: 360, height: 26)`, `leadingSymbol: "magnifyingglass"`, `radius: 7`):
     - Label: `"Search Network Directory…"` (`11.5pt .medium`) + right-aligned count badge (when `directoryOnlineCount > 0`): `"\(directoryOnlineCount) online"` (`11pt .regular`, `.secondaryLabelColor`).
     - Clicking calls `cardMenu.cancelTracking()`, opens `DirectoryPanel` (`openDirectoryPanel()`), and emits `["op": "query_directory", "query": directoryQuery, "scope": directoryScopeFilter, "harness": directoryHarnessFilter]`.
6. **`menuActions` Index Stability**:
   - Do **NOT** append `switch_hub:*`, `target_hub:*`, `toggle_my_remote`, `copy_remote:*`, `toggle_group:*`, or `open_directory` to `menuActions` (they are invoked via `test_control` by `id`). Only existing listener/footer/hub actions (`0..9`) plus `candidateActions` (appended after `quit`) are registered in `menuActions`.

---

### 4.3 Surface 2: Spotlight-Style Network Directory Panel (`560 × 440 pt` `KeyPanel: NSPanel`)

1. **Window & Focus Lifecycle**:
   - `final class KeyPanel: NSPanel { override var canBecomeKey: Bool { true }; override var canBecomeMain: Bool { false } }`
   - Separate from the notification HUD `panel` (`directoryPanel` is centered at `NSRect(x: 0, y: 0, width: 560, height: 440)`). Its clipped `SurfaceView` root has radius `14`, clear fill, and an appearance-aware `labelColor` border at `0.15`.
   - Menu lifecycle hover clearing excludes unchanged Directory controls. Closing this panel clears only its active hover before hiding it.
   - In non-headless mode, opening `directoryPanel` calls `NSApp.activate(ignoringOtherApps: true)`, `directoryPanel.makeKeyAndOrderFront(nil)`, and focuses `directorySearchField`; in `headless` mode, `openDirectoryPanel()` constructs the real `KeyPanel` and subviews and marks `directoryVisible = true` without calling `NSApp.activate` or `makeKeyAndOrderFront`.
   - Pressing `Escape` (when `!directorySearchField.currentEditor()?.hasMarkedText()` so active IME composition is never aborted) or clicking `id = "close_directory"` sets `directoryVisible = false`, hides `directoryPanel`, and emits `["op": "close_directory"]`.
2. **Search Field, Scope/Harness Facet Pills & Daemon-Backed Full-Index Search**:
   - **Top Search Bar (`560 × 46 pt`)**: `NSSearchField` (`id = "directory_search"`, `placeholderString = "Search by user, repo, title, host, or #id…"`), plus a close button (`id = "close_directory"`).
   - **Facet Bar (`560 × 32 pt`)**:
     - Scope pills: `All` (`id = "dir_scope:all"`), `My Hosts` (`id = "dir_scope:mine"`).
     - Harness pills: `All Harnesses` (`id = "dir_harness:all"`), `Pi` (`id = "dir_harness:pi"`), `Codex` (`id = "dir_harness:codex"`).
     - Active scope/harness pills use `.prominent`; inactive pills and Close use `.neutral`.
   - **Full-Index Query Execution (`query_directory`)**:
     - Whenever `openDirectoryPanel()` runs, or the search query / scope pill / harness pill changes, `cito-ui` immediately filters its currently loaded `directoryPeers` in-memory for instant `0ms` UI feedback AND emits `["op": "query_directory", "query": query, "scope": scope, "harness": harness]` to `cito-daemon`.
     - `CitoService` evaluates the query (supporting space-separated free-text tokens AND prefix tokens `@<user>`, `user:<user>`, `repo:<repo>`, `host:<host>`, `is:<harness>`, `#<id>`) across the **entire in-memory `HubDirectoryIndex` (all 2,000+ peers, zero pre-filter truncation!)**, sorts matches (`is_self_principal` first, exact/prefix `title`/`user`/`repo` matches second, `last_seen_at` descending third, `scoped_id` ascending fourth), and sends back `{ op: "directory", peers: matched.slice(0, 100), matched_count: matched.length, online_count: totalOnline }`.
     - Because each `op: "directory"` response carries at most `100` matched rows (with `about`/`title`/`cwd` truncated to bounded display lengths if needed so serialized UTF-8 is always `< 128 KiB`), every single peer in a 2,000-peer network is 100% searchable while every stdio frame stays far below the 1 MiB limit!
3. **Virtualized Result List (`NSTableView` inside `NSScrollView`, `560 × 334 pt`)**:
   - Uses a view-based `NSTableView` (`rowHeight = 52`) with `makeView(withIdentifier:owner:)` cell reuse. Each cell keeps the constant reuse identifier `DirectoryPeerCell`; its separate `inspectionId` exposes `dir_peer:<scoped_id>` to headless inspection. Reconfiguration invalidates copy-feedback timers and clears stale copied/hover state.
   - Selected cells resolve `controlAccentColor` fill at `0.22` on layer and appearance updates; other cells remain clear.
   - Each row (`height: 52 pt`, `id = "dir_peer:\(peer.scoped_id)"`) displays:
     - Left: `28 × 28 pt` filled harness badge (radius `7`, icon inset `6`, tint fill at `0.88`, white border at `0.18`, white icon; `terminal.fill` for `pi`, `sparkles` for `codex`, `bolt.horizontal.fill` for others).
     - Line 1 (`y: 26`): **`<peer.title>`** (`13pt .semibold`) + `" · \(peer.repo ?? "")\(branchSuffix)"` (`11.5pt .secondaryLabelColor`).
     - Line 2 (`y: 7`): **`@\(peer.user)`** (`11.5pt .medium .labelColor`) + `"@\(cleanHost) · \(peer.about)"` (`11.5pt .secondaryLabelColor`).
     - Right (`x: 544 - copyWidth, y: 16, height: 22 pt`): `MenuControl(short, ..., leadingSymbol: "doc.on.doc")` (`id = "copy_dir:\(peer.scoped_id)"`) that copies `peer.scoped_id` to `NSPasteboard.general` and shows `"✓ Copied"` in `.systemGreen`.
   - **Footer Shortcut Bar (`560 × 28 pt`)**: Displays `"↵ Copy Selected ID    ⎋ Close"` and count summary (`"\(directoryMatchedCount) of \(directoryOnlineCount) online"`).

---

### 4.4 Wire Protocol & Headless Inspection Contract (`src/ui/index.ts`, `src/macos/cito-ui.swift`)

1. **`UiBridge` & `DaemonToUiMessage` (`src/ui/index.ts`)**:
   - `UiBridge.state(hubs, listeners, available = [], myRemoteListeners = [], directoryOnlineCount = 0)` defaults optional parameters so existing 2- or 3-argument callers in `tests/ui/lifecycle.test.ts` remain 100% compatible.
   - `UiBridge.directory(peers: UiRemotePeerSummary[], matchedCount = peers.length, onlineCount = peers.length)` sends `{ op: "directory", peers, matched_count: matchedCount, online_count: onlineCount }` and caches the latest directory frame for replay on child restart.
   - `validAction()` accepts `{ op: "open_directory" }`, `{ op: "close_directory" }`, `{ op: "query_directory", query?: string, scope?: "all" | "mine", harness?: string }`, and `{ op: "switch_hub", from_hub: string, to_hub: string, listener_id: string }` (validating `from_hub` and `to_hub` with `hubNameSchema`, requiring `from_hub !== to_hub`, and validating `listener_id` with `canonicalListenerIdSchema`) before the generic `a.hub` check. `listen_session` additionally accepts optional `hub?: string` (`hubNameSchema`) and `scope?: "self" | "workspace"`.
   - `CitoService.handleUiAction` handles `switch_hub` inside a dedicated per-session `switchMutex` (`${harness}:${session_id}`): it verifies the source listener is still live in `liveActiveRows()`, checks whether the target hub already has a live listener for that session in `liveActiveRows()`, otherwise enrolls on `to_hub` via `this.listen(...)` inheriting `about`, `scope`, and `tag`, and only calls `unlistenById(from_hub, row.listener_id)` after `to_hub` enrollment succeeds. If `to_hub` enrollment fails, it records the error on `toSlot.error` and preserves the `from_hub` listener intact.
2. **Headless Inspection & Control Hooks (`src/macos/cito-ui.swift`)**:
   - `test_control` searches `cardMenu.items` first, and if not found, searches `directoryPanel?.contentView` (including visible `NSTableView` row views).
   - Headless command `{"op": "test_directory_query", "query": "..."}` updates `directorySearchField.stringValue`, triggers filtering, and emits `query_directory`.
   - Headless command `{"op": "test_directory_key", "key": "down" | "up" | "return" | "escape"}` invokes the production keyboard navigation / Return-copy / Escape-close handlers on `DirectoryPanel`.
   - `inspectMenu()` keeps `items` strictly for `cardMenu.items` (preserving `initial.items.every(node => node.width === 380)`) and adds top-level fields `"directory_open"`, `"directory_online_count"`, `"directory_matched_count"`, `"directory_query"`, `"directory_scope"`, `"directory_harness"`, `"directory_selected_id"`, and `"directory_items"` to the `cito-ui: menu {...}` JSON snapshot.

---

## 5. Direct Session Control Commands & Zero-Script Packaging (`Pi /cito`, CLI `cito`, `package.json`)

### 5.1 Direct Session Control Commands
- **Pi Slash Command (`/cito` in `src/pi-extension/cito-bridge.ts`)**:
  - Standalone zero-dependency extension registering `/cito` (`["on", "team", "off"]` and `--hub ` completions, `Usage: /cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]`, no-arg toggle via `activeBadge`, `/cito on [--hub <hub> | --hub=<hub>] [about...]`, `/cito team [--hub <hub> | --hub=<hub>] [about...]`, `/cito off [--hub <hub> | --hub=<hub>]`), calling stateless `POST http://127.0.0.1:${port}/mcp` with `_meta: { endpoint_pid: process.pid }`. When `--hub <hub>` is passed to `/cito off`, `activeBadge` is not blindly cleared after the RPC returns so the daemon's synchronous socket `status` update for any remaining active hubs remains authoritative, and the notification states `Cito stopped listening on <hub>`.
- **CLI Direct Control (`cito listen` & `cito unlisten` in `src/mcp/cli.ts`)**:
  - Supports `cito listen` and `cito unlisten` for terminal/SSH and `--codex[=<thread_id>]`.

### 5.2 Git & Global `npm install` Packaging Contract (`package.json`, `tsconfig.json`, `bin/`)
1. **Avoiding `pacote` Git-Preparation Global Symlink Corruption**:
   - In npm's `pacote/lib/git.js` (`GitFetcher.#prepareDir`), defining any of the six script keys `prepare`, `build`, `prepack`, `preinstall`, `install`, or `postinstall` in `package.json` causes `npm install -g github:<owner>/<repo>` to spawn a nested `npm install` inside `~/.npm/_cacache/tmp/git-clone*` that inherits `npm_config_global=true`, creating a broken global symlink or failing with `sh: tsc: command not found` (`code 127`).
   - Therefore, `package.json` `scripts` MUST NOT define `prepare`, `build`, `prepack`, `preinstall`, `install`, or `postinstall`. TypeScript compilation is defined under `"compile": "rm -rf dist && tsc"` (with `"prepublishOnly": "npm run typecheck && npm run compile"`, `"typecheck": "tsc --noEmit"`, and `"test": "vitest run --dir tests"`).
   - `package.json` `"files"` MUST be `["bin", "dist/src", "src", "README.md"]` so both Git installs (where `dist/` is gitignored) and prebuilt registry tarballs contain the complete `src/` tree alongside `bin/`.
2. **Native Node TypeScript Execution & Zero-Install-Script Production Dependencies**:
   - `tsconfig.json` enables `"erasableSyntaxOnly": true`, and all classes in `src/` use explicit field declarations and constructor assignments instead of TypeScript parameter properties (`constructor(readonly ...)`), allowing Node.js (`^22.13.0 || ^24.0.0 || >=26.0.0`) to execute `src/**/*.ts` directly via built-in TypeScript type-stripping.
   - `bin/cito-mcp` and `bin/cito-auth-callout` resolve `../dist/src/.../cli.js` first when present; when `dist/` is absent, they register a built-in `node:module.register` hook that maps relative `.js` specifiers (`./*.js`, `../*.js`) to `.ts` on `ERR_MODULE_NOT_FOUND` and import `../src/mcp/cli.ts` / `../src/auth-callout/cli.ts` directly without `tsx`.
   - `"tsx"` is kept only in `"devDependencies"` (never in `"dependencies"`), eliminating `esbuild` and `fsevents` install scripts from production and global installs.
   - `bin/cito-claude-hook` (Section 6) is a third entrypoint under the same rule and the same `"bin"` map; it resolves `../dist/src/claude-hook/cli.js` first and must never print anything but a hook result object on stdout.

### 5.3 Versioning, `/health` Handshake & Automatic Stale-Daemon Handover (`src/shared/index.ts`, `src/mcp/server.ts`, `src/mcp/cli.ts`)
1. **Single Source of Truth (`CITO_VERSION`)**:
   - `CITO_VERSION` (exported from `src/shared/index.ts`, set to `"0.10.0"` matching `package.json` `"version"`) is the canonical product SemVer string. Automated tests verify `CITO_VERSION === packageJson.version`.
   - `cito --version` and `cito -v` (in `src/mcp/cli.ts`) print `cito v${CITO_VERSION}\n` to `stdout` and exit `0` without starting `cito-daemon`.
   - `createMcpServer` (`src/mcp/server.ts`) advertises `version: CITO_VERSION`, and `GET /health` returns `{ ok: true, pid: process.pid, stateDir: config.stateDir, version: CITO_VERSION }`.
2. **Automatic Stale-Daemon Replacement (`ensureDaemon` in `src/mcp/cli.ts`)**:
   - `daemonHealth(config)` returns `{ pid: number; stateDir: string; version?: string } | undefined`.
   - In `ensureDaemon(config)`:
     - If `const health = await daemonHealth(config)`:
       - When `health.version === CITO_VERSION`, return immediately.
       - When `health.version !== CITO_VERSION` (e.g. a pre-upgrade daemon is still running on `config.daemonPort`), send `SIGTERM` to `health.pid` (`try { process.kill(health.pid, "SIGTERM"); } catch {}`) and poll until `!alive(health.pid)` (up to `5000ms`), then proceed to spawn the new `cito-daemon` and wait until `(await daemonHealth(config))?.version === CITO_VERSION`.

---

## 6. Claude Code Integration (`claude`)

Claude Code exposes no queue CLI and no wake URL scheme, so the delivery effect cannot be a pair of process invocations the way `codex queue` + `codex://threads/wake/<id>` is (Section 3.2). The only surface that can restart an idle Claude Code turn is a **hook**, which runs out-of-band from the MCP server. Delivery is therefore split into a durable queue write the daemon can retry and fence like any other effect, and an injection performed later by the hook at the next turn boundary.

### 6.1 Session Identity & Transport Topology
- **Host half (`startClaudeChannelBridge` in `src/adapters/claude-channel.ts`)**: started by `src/mcp/cli.ts` in stdio mode when `config.enableClaudeChannel` (`CITO_CLAUDE_CHANNEL=1`, Section 1.2). It binds `${stateDir}/sockets/claude-pid-${process.ppid}.sock` (`0700` directory, `0600` socket) and answers the standard socket bridge protocol (`identify`, `deliver`, `status`), so `ClaudeCodeAdapter extends SocketBridgeAdapter` inherits candidate discovery, probing and badge updates without a second protocol. `identify` answers with `session_name` resolved by `resolveClaudeSessionTitle`: the last `custom-title` record in `${claudeHome}/projects/<slugged-cwd>/<session>.jsonl`, scanned over the final 128 KiB only and cached by size and mtime, because transcripts reach hundreds of megabytes while `identify` answers under a sub-second budget. A session with no title in that window reports `basename(cwd)`, so discovery degrades to the workspace name instead of stalling. A failed bind is logged and the MCP forwarder keeps working.
- **Implicit binding**: the MCP server is a child of Claude Code, so `resolveCaller`'s parent-PID walk reaches `claude-pid-<claude-pid>.sock` within depth 5 and `cito_listen` needs no caller metadata.
- **Session identifier**, re-resolved on every request so `/clear` reads as `switched` rather than silently redirecting a listener:
  1. `CLAUDE_SESSION_ID` when the host exports it;
  2. the newest `${claudeHome}/projects/<slug>/<uuid>.jsonl`, where `<slug>` is the absolute cwd with every non-alphanumeric character replaced by a hyphen, by mtime (`CITO_CLAUDE_HOME`, default `~/.claude`);
  3. `claude-<endpoint_pid>`.
  Identifiers become queue file names and are constrained to `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
- **Explicit claims (`ClaudeCodeAdapter.detectExplicitCaller`)** for direct Streamable HTTP callers with no Cito child process: `_meta.claudeSessionId`, `_meta.claude_session_id`, the `x-claude-session-id` HTTP header, and `expected_session_id` when `expected_harness === "claude"`. Multiple distinct claims, or a claim combined with a top-level bridge PID (`endpoint_pid` / `caller_pid` / `pi_pid`), return `INVALID_INPUT`. Workspace resolution is `_meta.cwd` $\to$ the HTTP client's OS working directory $\to$ retained session cwd $\to$ the daemon's cwd, with Git metadata enrichment.
- **No disk-derived closure**: a transcript outlives the process that wrote it, so a missing or stale transcript MUST NOT produce `closed_definitive`. Liveness belongs to the socket bridge and the endpoint PID alone.
- **Capability advertisement**: `forward()` adds `experimental: { "claude/channel": {} }` to the initialize result only; every other JSON-RPC message passes through unchanged.
- **UI**: `harnessVisual` in `src/macos/cito-ui.swift` maps `"claude"` to `("cpu.fill", .systemOrange, "Claude")`.

### 6.2 Two-Phase Delivery (Durable Queue Write + Hook Injection)
1. **Queue write (synchronous, fenced, retriable)**: `SocketBridgeAdapter.deliver` sends `{op: "deliver", session_id, batch_id, wake, content}`; the bridge appends `{batch_id, content, wake, created_at_ms}` to `${stateDir}/claude/wakes/<session>.jsonl` (`0600`) and `fsync`s **before** replying, because the hook may only run minutes later. Batches are deduplicated over the last 200 `batch_id`s. The adapter is constructed with `confirmDelivery: false`, so a queued card yields `delivered_unconfirmed`: it stays unacknowledged and `cito_inbox()` remains the fallback.
2. **Injection (`bin/cito-claude-hook`, `src/claude-hook/cli.ts`)**: reads the hook payload on stdin and emits exactly one JSON object on stdout.
   - `Stop` $\to$ `{"decision": "block", "reason": "<cards joined by a blank line>"}` — the only output that restarts a finished turn.
   - `SessionStart` / `UserPromptSubmit` $\to$ `{"hookSpecificOutput": {"hookEventName": "<event>", "additionalContext": "<cards>"}}`.
   - Any other event emits `{}` and MUST NOT drain the queue.
3. **Atomic drain**: `rename(queue, queue + ".draining-" + randomUUID())`, then read every `*.draining-*` staging file for that session in mtime order, deduplicate by `batch_id`, and unlink. An appender racing a drain recreates the queue instead of losing a card, and a drain that died before unlinking leaves its staging file for the next run to recover. A torn trailing line is skipped, never replayed.
4. **Neutral failure**: any hook error writes to stderr and still emits `{}` — a notification bus must never break the user's session. A card lost after its drain remains an unacknowledged signal in SQLite and reappears through `cito_inbox()`.
5. **Badge**: `updateBadge` writes `🔔 <slug> (<unread>)` to `${stateDir}/claude/badges/<session>` (`0600`) and removes it when the badge clears or the bridge closes, so an optional `statusLine` command can render it. Badges are never pushed into the transcript.

### 6.3 Operator-Owned Host Configuration
Cito installs no hooks and never edits Claude Code settings, matching its LaunchAgent policy. The operator adds `cito-mcp` with `CITO_CLAUDE_CHANNEL=1` to `mcpServers` and `bin/cito-claude-hook` to the `Stop`, `SessionStart` and `UserPromptSubmit` hooks; `README.md` carries the exact configuration. Each half degrades alone: with only the MCP server, cards queue and arrive through `cito_inbox()`; with only the hook, nothing is ever queued to drain.
