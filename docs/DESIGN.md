# Cito Architecture & Protocol Specification (V8 — Ego-Centric Menu Bar Control Plane, Spotlight Network Directory & Scalable NATS KV Discovery)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for coding agents that wakes or delivers messages to opted-in agent conversations across:
1. **Multiple NATS Hubs** (e.g., a personal Tailscale no-auth hub `personal` and a team OIDC hub `work` simultaneously),
2. **Users** (e.g., `alice`, `bob`, scaling to $U = 50\dots 500+$ team members),
3. **Hosts / Machines** (e.g., `macbook-work`, `macbook-personal`),
4. **Extensible Agent Harnesses** (shipped in code: **Pi Coding Agent** and **ChatGPT / Codex Desktop App**; open extensible adapter interface for future harnesses, with **Claude Code** documented strictly as a design suggestion in **Section 6**), and
5. **Concurrent Conversations** within the same repository or working directory (scaling to $N = 100\dots 2,000+$ online agents across a network).

### 1.2 Language, Neutral Naming & Zero Half-Baked Code Policy
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, directory panel, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names (such as `rda`, `casdoor`, `rcatwin`, `keppel`, `mingchen`) may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **Zero Half-Baked Claude Code Implementation in Source**: Claude Code integration is a **Proposed Design Suggestion Only — Not Yet Implemented** (Section 6). Source files (`src/**`) and test files (`tests/**`) MUST NOT contain half-implemented `claude` adapters, `enableClaudeChannel` config flags, `CITO_CLAUDE_CHANNEL` environment branches, or `"claude"` UI icon special cases:
  - Delete `src/adapters/claude-channel.ts` and `tests/adapters/claude-channel.test.ts`;
  - Delete `tests/e2e/claude-channel.test.ts` after migrating its generic transport-independent regression checks (600-message burst pagination, 256 KiB batch byte-ceiling splitting, 70 KB oversized sender title handling, and plain `cito-mcp` stdio forwarding/shutdown) into `tests/mcp/dispatch.test.ts` / `tests/e2e/lifecycle.test.ts` under `pi`;
  - Remove `registerAdapter(new SocketBridgeAdapter("claude", ...))` from `src/adapters/index.ts`, `enableClaudeChannel` / `CITO_CLAUDE_CHANNEL` from `src/shared/config.ts` and `src/mcp/cli.ts`, `case "claude":` from `src/macos/cito-ui.swift`, the channel config test from `tests/shared/multi-hub.test.ts`, and replace `"claude"` with `"custom"` in `tests/adapters/pi.test.ts` and `tests/ui/lifecycle.test.ts`.

---

## 2. Module Boundaries, Pure Adapter Contract & Scalable NATS KV Discovery

### 2.1 Acyclic Module Graph
`src/shared/` $\to$ `src/adapters/` & `src/ui/` $\to$ `src/mcp/`

- **`src/mcp/server.ts` Purity Invariant**: `src/mcp/server.ts` MUST NOT import `codexThreadId` or reference `"x-codex-turn-metadata"`, `"codex"`, or `"pi"`. It passes `{ meta, headers, httpClientPid, httpTransport: true }` into `service.call(name, args, context)`.
- **Protocol Version Negotiation (`src/mcp/server.ts`)**:
  - Registers `'2026-07-28'` in `SUPPORTED_PROTOCOL_VERSIONS` alongside `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`, and `2024-10-07`.
- **Default Registered Adapters (`src/adapters/index.ts`)**:
  - Registers only `new CodexDesktopAdapter()` and `new SocketBridgeAdapter("pi")` at startup, while `getAdapter(harness)` dynamically instantiates `new SocketBridgeAdapter(harness)` for any valid custom harness ID matching `^[a-z][a-z0-9_-]{0,31}$`.

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
│ │ └──────┘  [ ⧉ dotfiles#rdap ]   ~/dotfiles/agent · main          │ │
│ └──────────────────────────────────────────────────────────────────┘ │
├──────────────────────────────────────────────────────────────────────┤
│  ▾  MY OTHER HOSTS · 2                                      (可折叠) │ ← Zone 2: My Remote Sessions
│     💻 Tefx-MBP-Personal · cito · main             [ ⧉ cito#8f3a ]   │   (id="my_remote_header" +
│     💻 Tefx-MBP-Personal · larva                   [ ⧉ larv#4d2e ]   │    34pt rows id="remote:<scoped_id>")
├──────────────────────────────────────────────────────────────────────┤
│  AVAILABLE SESSIONS                                                  │ ← Zone 3: Local Candidates
│  ▾  [>_] Pi · 2 open                                        (可折叠) │   (id="group:pi", 28pt)
│     cito · ~/Projects/cito · main                       [ + Listen ] │   (id="candidate:pi:...", 36pt,
│     larva · ~/Projects/larva · feat/ui                  [ + Listen ] │    x:24..298 icon-free text)
│  ▸  [✦] Codex · 5 recent                                    (已折叠) │   (id="group:codex", 28pt)
├──────────────────────────────────────────────────────────────────────┤
│  [ 🔍 Search Network Directory…                        412 online ]  │ ← Zone 4: Directory Launcher (34pt)
├──────────────────────────────────────────────────────────────────────┤
│  [ ■ Stop All ]                                     [ Quit Cito ⌘Q ] │ ← Footer (36pt, id="footer")
└──────────────────────────────────────────────────────────────────────┘
```

#### Exact Menu Bar Zones & Geometry (`src/macos/cito-ui.swift`):
1. **Header Bar (`380 × 40 pt`, `id = "header"`)**:
   - Left: **`Cito`** (`13pt .bold`) + dynamic subtitle (`"1 listening"`, `"1 listening · 2 remote"`, `"Idle · 2 remote"`, or `"Idle"`). Compute `subWidth = ceil(measureTextWidth(subtitleText, font: .systemFont(ofSize: 11.5))) + 8`, set subtitle width to `subWidth`, and place `hubStrip` at `x = max(138, 54 + subWidth), width = 368 - max(138, 54 + subWidth)` so the subtitle is never clipped.
2. **Zone 1 — Active on This Mac (`listeners`)**:
   - `360 × 58/72 pt` rounded cards (`id = "session:\(listener.scoped_id)"`). Capped inside `active_scroll` (`260pt` `FlippedView` `NSScrollView`) when `listeners.count > 4`.
   - When `listeners.isEmpty && !available.isEmpty`, renders the `380 × 34 pt` compact strip (`id = "empty"`, `"No active listeners — click + Listen below"`).
3. **Zone 2 — My Other Hosts (`my_remote_listeners`, shown when `!myRemote.isEmpty`)**:
   - Contains only active registrations from connected hubs where `reg.principal_id === principal.principal_id && reg.host_id !== this.hostId`.
   - **Collapsible Header (`380 × 26 pt`, `id = "my_remote_header"`)**: Top `1pt` hairline divider + `MenuControl` (`id = "toggle_my_remote"`) with `chevron.down` / `chevron.right` and `"MY OTHER HOSTS · \(myRemote.count)"` (`10pt .semibold`, `.secondaryLabelColor`). Clicking toggles `myRemoteCollapsed` in-place.
   - **My Remote Rows (`380 × 34 pt`, `id = "remote:\(peer.scoped_id)"`, shown when `!myRemoteCollapsed`)**:
     - Left (`NSRect(x: 16, y: 10, width: 14, height: 14)`): SF Symbol `"desktopcomputer"` (`.secondaryLabelColor`).
     - Right (`NSRect(x: 368 - copyWidth, y: 7, width: copyWidth, height: 20)`): `MenuControl(short, ..., leadingSymbol: "doc.on.doc")` (`id = "copy_remote:\(peer.scoped_id)"`), copying `peer.scoped_id` with `"✓ Copied"` feedback.
     - Center (`NSRect(x: 36, y: 8, width: 368 - copyWidth - 44, height: 18)`): `"\(cleanHost) · \(peer.title)" + (branchSuffix)` (`12pt .regular`, `.labelColor`, `toolTip = "\(peer.scoped_id) — \(peer.about)"`).
     - Capped inside `my_remote_scroll` (`102pt` = 3 rows) when `myRemote.count > 3`.
4. **Zone 3 — Available Sessions with Collapsible Harness Groups (`!available.isEmpty`)**:
   - **Section Header (`380 × 24 pt`, `id = "available_header"`)**: `1pt` divider + `"AVAILABLE SESSIONS"` (`10pt .semibold`).
   - **Collapsible Harness Group Header (`380 × 28 pt`, `id = "group:\(harness)"`)**:
     - Partition `available` by `harness` in deterministic order (`"pi"`, `"codex"`, then any other harness alphabetically).
     - Interactive `MenuControl` (`id = "toggle_group:\(harness)"`, `NSRect(x: 8, y: 2, width: 364, height: 24)`, `radius: 6`):
       - Disclosure chevron (`x: 6, y: 7, width: 10, height: 10`): `collapsedGroups.contains(harness) ? "chevron.right" : "chevron.down"`.
       - Harness icon badge (`18 × 18 pt` at `x: 20, y: 3, radius: 5`, `tint.withAlphaComponent(0.12)`): `"terminal.fill"` (`.systemIndigo`) for `pi`, `"sparkles"` (`.systemTeal`) for `codex`, `"bolt.horizontal.fill"` for others.
       - Group label (`NSRect(x: 44, y: 4, width: 310, height: 16)`, `11.5pt .semibold`): `"Pi · \(items.count) open"` for `pi`, `"Codex · \(items.count) recent"` for `codex`, `"\(harness.capitalized) · \(items.count) available"` for others.
       - Clicking toggles `collapsedGroups` membership for `harness` and calls `rebuild()` in-place.
   - **Icon-Free Candidate Rows (`380 × 36 pt`, `id = "candidate:\(c.harness):\(c.session_id)"`, shown when `!collapsedGroups.contains(harness)`)**:
     - Text label (`NSRect(x: 24, y: 9, width: 274, height: 18)`): `"\(c.title) · \(shortCwd)\(branchSuffix)"` (`12pt .regular`, `.byTruncatingMiddle`, `toolTip = "\(c.title) — \(c.cwd)"`).
     - Right button (`NSRect(x: 304, y: 7, width: 64, height: 22)`): `MenuControl("Listen", ..., leadingSymbol: "plus")` (`id = "listen:\(c.harness):\(c.session_id)"`).
     - **Scroll Container ID Rule**: When `items.count > 4` within an expanded group, host its candidate rows inside an `NSScrollView` (`width: 380, height: 144 pt`, `FlippedView` document height `items.count * 36 pt`) with `id = groupedCandidates.count == 1 ? "available_scroll" : "available_scroll:\(harness)"` (preserving `"available_scroll"` when a single group is present and disambiguating `"available_scroll:\(harness)"` when multiple groups exist).
5. **Zone 4 — Network Directory Launcher Row (`380 × 34 pt`, `id = "directory_row"`, rendered when `!hubs.isEmpty`)**:
   - Top `1pt` hairline divider (`x: 12, y: 32, width: 356`).
   - Full-width `MenuControl` (`id = "open_directory"`, `NSRect(x: 10, y: 4, width: 360, height: 26)`, `leadingSymbol: "magnifyingglass"`, `radius: 7`):
     - Label: `"Search Network Directory…"` (`11.5pt .medium`) + right-aligned count badge (when `directoryOnlineCount > 0`): `"\(directoryOnlineCount) online"` (`11pt .regular`, `.secondaryLabelColor`).
     - Clicking calls `cardMenu.cancelTracking()`, opens `DirectoryPanel` (`openDirectoryPanel()`), and emits `["op": "query_directory", "query": directoryQuery, "scope": directoryScopeFilter, "harness": directoryHarnessFilter]`.
6. **`menuActions` Index Stability**:
   - Do **NOT** append `toggle_my_remote`, `copy_remote:*`, `toggle_group:*`, or `open_directory` to `menuActions` (they are invoked via `test_control` by `id`). Only existing listener/footer/hub actions (`0..9`) plus `candidateActions` (appended after `quit`) are registered in `menuActions`.

---

### 4.3 Surface 2: Spotlight-Style Network Directory Panel (`560 × 440 pt` `KeyPanel: NSPanel`)

1. **Window & Focus Lifecycle**:
   - `final class KeyPanel: NSPanel { override var canBecomeKey: Bool { true }; override var canBecomeMain: Bool { false } }`
   - Separate from the notification HUD `panel` (`directoryPanel` is centered at `NSRect(x: 0, y: 0, width: 560, height: 440)`).
   - In non-headless mode, opening `directoryPanel` calls `NSApp.activate(ignoringOtherApps: true)`, `directoryPanel.makeKeyAndOrderFront(nil)`, and focuses `directorySearchField`; in `headless` mode, `openDirectoryPanel()` constructs the real `KeyPanel` and subviews and marks `directoryVisible = true` without calling `NSApp.activate` or `makeKeyAndOrderFront`.
   - Pressing `Escape` (when `!directorySearchField.currentEditor()?.hasMarkedText()` so active IME composition is never aborted) or clicking `id = "close_directory"` sets `directoryVisible = false`, hides `directoryPanel`, and emits `["op": "close_directory"]`.
2. **Search Field, Scope/Harness Facet Pills & Daemon-Backed Full-Index Search**:
   - **Top Search Bar (`560 × 46 pt`)**: `NSSearchField` (`id = "directory_search"`, `placeholderString = "Search by user, repo, title, host, or #id…"`), plus a close button (`id = "close_directory"`).
   - **Facet Bar (`560 × 32 pt`)**:
     - Scope pills: `All` (`id = "dir_scope:all"`), `My Hosts` (`id = "dir_scope:mine"`).
     - Harness pills: `All Harnesses` (`id = "dir_harness:all"`), `Pi` (`id = "dir_harness:pi"`), `Codex` (`id = "dir_harness:codex"`).
   - **Full-Index Query Execution (`query_directory`)**:
     - Whenever `openDirectoryPanel()` runs, or the search query / scope pill / harness pill changes, `cito-ui` immediately filters its currently loaded `directoryPeers` in-memory for instant `0ms` UI feedback AND emits `["op": "query_directory", "query": query, "scope": scope, "harness": harness]` to `cito-daemon`.
     - `CitoService` evaluates the query (supporting space-separated free-text tokens AND prefix tokens `@<user>`, `user:<user>`, `repo:<repo>`, `host:<host>`, `is:<harness>`, `#<id>`) across the **entire in-memory `HubDirectoryIndex` (all 2,000+ peers, zero pre-filter truncation!)**, sorts matches (`is_self_principal` first, exact/prefix `title`/`user`/`repo` matches second, `last_seen_at` descending third, `scoped_id` ascending fourth), and sends back `{ op: "directory", peers: matched.slice(0, 100), matched_count: matched.length, online_count: totalOnline }`.
     - Because each `op: "directory"` response carries at most `100` matched rows (with `about`/`title`/`cwd` truncated to bounded display lengths if needed so serialized UTF-8 is always `< 128 KiB`), every single peer in a 2,000-peer network is 100% searchable while every stdio frame stays far below the 1 MiB limit!
3. **Virtualized Result List (`NSTableView` inside `NSScrollView`, `560 × 334 pt`)**:
   - Uses a view-based `NSTableView` (`rowHeight = 52`) with `makeView(withIdentifier:owner:)` cell reuse.
   - Each row (`height: 52 pt`, `id = "dir_peer:\(peer.scoped_id)"`) displays:
     - Left: `28 × 28 pt` tinted harness badge (`terminal.fill` for `pi`, `sparkles` for `codex`).
     - Line 1 (`y: 26`): **`<peer.title>`** (`13pt .semibold`) + `" · \(peer.repo ?? "")\(branchSuffix)"` (`11.5pt .secondaryLabelColor`).
     - Line 2 (`y: 7`): **`@\(peer.user)`** (`11.5pt .medium .labelColor`) + `"@\(cleanHost) · \(peer.about)"` (`11.5pt .secondaryLabelColor`).
     - Right (`x: 544 - copyWidth, y: 16, height: 22 pt`): `MenuControl(short, ..., leadingSymbol: "doc.on.doc")` (`id = "copy_dir:\(peer.scoped_id)"`) that copies `peer.scoped_id` to `NSPasteboard.general` and shows `"✓ Copied"` in `.systemGreen`.
   - **Footer Shortcut Bar (`560 × 28 pt`)**: Displays `"↵ Copy Selected ID    ⎋ Close"` and count summary (`"\(directoryMatchedCount) of \(directoryOnlineCount) online"`).

---

### 4.4 Wire Protocol & Headless Inspection Contract (`src/ui/index.ts`, `src/macos/cito-ui.swift`)

1. **`UiBridge` & `DaemonToUiMessage` (`src/ui/index.ts`)**:
   - `UiBridge.state(hubs, listeners, available = [], myRemoteListeners = [], directoryOnlineCount = 0)` defaults optional parameters so existing 2- or 3-argument callers in `tests/ui/lifecycle.test.ts` remain 100% compatible.
   - `UiBridge.directory(peers: UiRemotePeerSummary[], matchedCount = peers.length, onlineCount = peers.length)` sends `{ op: "directory", peers, matched_count: matchedCount, online_count: onlineCount }` and caches the latest directory frame for replay on child restart.
   - `validAction()` accepts `{ op: "open_directory" }`, `{ op: "close_directory" }`, and `{ op: "query_directory", query?: string, scope?: "all" | "mine", harness?: string }` before the `a.hub` check.
2. **Headless Inspection & Control Hooks (`src/macos/cito-ui.swift`)**:
   - `test_control` searches `cardMenu.items` first, and if not found, searches `directoryPanel?.contentView` (including visible `NSTableView` row views).
   - Headless command `{"op": "test_directory_query", "query": "..."}` updates `directorySearchField.stringValue`, triggers filtering, and emits `query_directory`.
   - Headless command `{"op": "test_directory_key", "key": "down" | "up" | "return" | "escape"}` invokes the production keyboard navigation / Return-copy / Escape-close handlers on `DirectoryPanel`.
   - `inspectMenu()` keeps `items` strictly for `cardMenu.items` (preserving `initial.items.every(node => node.width === 380)`) and adds top-level fields `"directory_open"`, `"directory_online_count"`, `"directory_matched_count"`, `"directory_query"`, `"directory_scope"`, `"directory_harness"`, `"directory_selected_id"`, and `"directory_items"` to the `cito-ui: menu {...}` JSON snapshot.

---

## 5. Direct Session Control Commands (`Pi /cito` & CLI `cito listen` / `cito unlisten`)

- **Pi Slash Command (`/cito` in `src/pi-extension/cito-bridge.ts`)**:
  - Standalone zero-dependency extension registering `/cito` (`["on", "team", "off"]` completions, no-arg toggle via `activeBadge`, `/cito on [about...]`, `/cito team [about...]`, `/cito off`), calling stateless `POST http://127.0.0.1:${port}/mcp` with `_meta: { endpoint_pid: process.pid }`.
- **CLI Direct Control (`cito listen` & `cito unlisten` in `src/mcp/cli.ts`)**:
  - Supports `cito listen` and `cito unlisten` for terminal/SSH and `--codex[=<thread_id>]`.

---

## 6. Proposed Design for Claude Code Integration (Design Suggestion Only — Not Yet Implemented)

> **Status Notice (Proposed Design Suggestion Only — Not Yet Implemented)**: This section records the architectural design suggestion for future Claude Code integration. No Claude Code-specific adapter registration, CLI flag, or UI code is implemented in `src/`.

1. **Session Identity & Transport Topology**:
   - Claude Code sessions can connect either to `cito-daemon`'s stateless Streamable HTTP endpoint (`http://127.0.0.1:4290/mcp`) for pull-based `cito_inbox()` workflows, or through a future local socket bridge (`claude-pid-<pid>.sock`) implementing the standard `SocketBridgeAdapter` protocol (`identify`, `deliver`, `status`).
2. **Dual-Mode Notification Delivery (Channel Push + Turn-Start Inbox Drain)**:
   - Future integration can combine MCP channel notifications with turn-start `cito_inbox()` draining while reusing `SocketBridgeAdapter` without changing `src/mcp/` core logic.
