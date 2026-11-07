# Cito Architecture & Protocol Specification (V16 — Harness-Agnostic Core, Plugin Host SDK, Bundled & Out-of-Tree Plugins, Data-Driven macOS UI & Scalable NATS KV Discovery)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for coding agents that wakes or delivers messages to opted-in agent conversations across:
1. **Multiple NATS Hubs** (e.g., a personal Tailscale no-auth hub `personal` and a team OIDC hub `work` simultaneously),
2. **Users** (e.g., `alice`, `bob`, scaling to $U = 50\dots 500+$ team members),
3. **Hosts / Machines** (e.g., `macbook-work`, `macbook-personal`),
4. **Extensible Agent Harnesses via Unified Plugin Architecture** (bundled in `plugins/`: **Pi Coding Agent** (`tier: "stable"`), **ChatGPT / Codex Desktop App** (`tier: "stable"`), and **Claude Code** (`tier: "experimental"`); open out-of-tree plugin standard in `${CITO_STATE_DIR}/plugins/<id>/` via `@tefx/cito/plugin-sdk`), and
5. **Concurrent Conversations** within the same repository or working directory (scaling to $N = 100\dots 2,000+$ online agents across a network).

### 1.2 Language, Neutral Naming & Harness-Agnostic Core Policy
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, directory panel, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names (such as `rda`, `casdoor`, `rcatwin`, `keppel`, `mingchen`) may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **Harness-Agnostic Core (`src/` & `src/macos/cito-ui.swift`)**: Core modules (`src/shared/`, `src/mcp/`, `src/ui/`, `src/macos/cito-ui.swift`, and the Plugin Host SDK in `src/adapters/`) MUST NOT contain hardcoded branches, configuration fields, CLI flag names, or visual tables for specific agent harnesses (`pi`, `codex`, `claude`). All harness-specific discovery, caller detection, delivery, stdio channel wrapping, CLI shorthand flags, and UI presentation metadata live inside plugin directories (`plugins/{pi,codex,claude}/` for bundled plugins and `${CITO_STATE_DIR}/plugins/<id>/` for external user plugins).
- **Opt-In Host Channels**: A host channel that binds a second bridge socket from inside another process (Section 6) MUST be explicitly opt-in (`CITO_CLAUDE_CHANNEL=1`). Auto-detection from host environment variables is forbidden: it would bind an unrelated harness's endpoint and silently capture its caller binding.

---

## 2. Module Boundaries, Plugin Host SDK & Scalable NATS KV Discovery

### 2.1 Acyclic Module Graph & Unified Plugin Architecture
`src/shared/` $\to$ `src/adapters/` (Plugin Host SDK) & `src/ui/` $\to$ `src/mcp/`, with `plugins/{pi,codex,claude}/` importing only `src/shared/` and `src/adapters/` (`@tefx/cito/plugin-sdk`).

#### 1. Explicit File Extraction & Package Export Map
- **Core Plugin Host SDK (`src/adapters/`, exported as `@tefx/cito/plugin-sdk`)**:
  - `src/adapters/types.ts`: `HarnessAdapter`, `PluginManifest`, `UiHarnessMetadata`, `HarnessCliDescriptor`, `StdioChannelBridge`, `Caller`, `CallerDetectionContext`, `AvailableSessionSummary`, `DeliverContext`, `DeliverOutcome`, `ProbeOutcome`.
  - `src/adapters/socket-bridge.ts` (renamed from `src/adapters/pi.ts`): generic `SocketBridgeAdapter`, `SocketBridgeError` (keeping `PI_BRIDGE_UNAVAILABLE` as the public error code string for wire compatibility), `bridgeSocketPath`, `requestBridge`, `identifyBridge`.
  - `src/adapters/git.ts`: `BASE_PID_KEYS = ["endpoint_pid", "caller_pid"] as const`, `hasExplicitPid`, `gitMetadata`, `normalizeRepo`, `resolveExplicitCaller`.
  - `src/adapters/wake-card.ts`: `formatWakeCard`.
  - `src/adapters/index.ts`: `ADAPTER_REGISTRY`, `registerAdapter`, `loadPlugins`, `ensurePluginsLoaded`, `getAdapter`, `discoverAllAvailableSessions`, `filterAvailableSessions`, `matchAdapterCliFlag`, `resolveAdapterCliMeta`, `startOptionalStdioChannel`, `resolveCaller`.
- **Bundled Plugins (`plugins/{pi,codex,claude}/`)**:
  - `plugins/index.ts`: loads and registers the 3 bundled plugins (`plugins/pi`, `plugins/codex`, `plugins/claude`) in deterministic order (`pi` $\to$ `codex` $\to$ `claude`).
  - `plugins/pi/`:
    - `plugins/pi/plugin.json`: `{ "id": "pi", "displayName": "Pi", "tier": "stable", "sfSymbol": "terminal.fill", "tint": "indigo" }`.
    - `plugins/pi/index.ts`: exports `SocketBridgeAdapter("pi", { manifest, pidKeys: ["pi_pid"] })`.
    - `plugins/pi/cito-bridge.ts` (moved from `src/pi-extension/cito-bridge.ts`): standalone zero-dependency Pi 1.5.0 extension. `package.json` `"pi.extensions"` is updated to `["./plugins/pi/cito-bridge.ts"]`.
  - `plugins/codex/`:
    - `plugins/codex/plugin.json`: `{ "id": "codex", "displayName": "Codex", "tier": "stable", "sfSymbol": "sparkles", "tint": "teal", "jumpUrlTemplate": "codex://threads/{session_id}", "cliFlag": true }`.
    - `plugins/codex/index.ts` (moved from `src/adapters/codex.ts`): `CodexDesktopAdapter`, `loadCodexConfig(config)`, `codexThreadId`, `readCodexThreadOptional`, `resolveCodexThreadId`, `codexDesktopRunning`, and `cli` descriptor for `--codex[=<thread_id>]`.
  - `plugins/claude/`:
    - `plugins/claude/plugin.json`: `{ "id": "claude", "displayName": "Claude", "tier": "experimental", "sfSymbol": "cpu.fill", "tint": "orange", "cliFlag": true }`.
    - `plugins/claude/index.ts` (moved from `src/adapters/claude.ts`): `ClaudeCodeAdapter`, `loadClaudeConfig(config)`, `claudeSessionClaim`, `resolveClaudeSessionId`, `resolveClaudeSessionTitle`, `validateClaudeSessionId`, `appendClaudeWake`, `drainClaudeWakes`, `writeClaudeBadge`, `claudeProjectDir`, `claudeWakeQueuePath`, `claudeBadgePath`, `cli` descriptor for `--claude[=<session_id>]`, and `startStdioChannel`.
    - `plugins/claude/channel.ts` (moved from `src/adapters/claude-channel.ts`): `startClaudeChannelBridge`.
    - `plugins/claude/hook-cli.ts` (moved from `src/claude-hook/cli.ts`): `main()` for `bin/cito-claude-hook` (`runEntrypoint("../dist/plugins/claude/hook-cli.js", "../plugins/claude/hook-cli.ts")`).
- **Core Configuration (`src/shared/config.ts`)**:
  - Remove `codexHome`, `codexBin`, `codexDesktopRunning`, `claudeHome`, and `enableClaudeChannel` from `CitoConfig`.
  - Add `env: NodeJS.ProcessEnv` (preserving the exact `env` object passed to `loadConfig(env)`), `pluginDirs: string[]` (defaulting to `[builtinPluginsDir, join(stateDir, "plugins")]`, with `CITO_PLUGIN_PATH` entries appended), and `plugins?: Record<string, Record<string, unknown>>` on `CitoConfig`.
  - `loadConfig(env)` continues to reject empty/whitespace-only `CITO_*` path environment variables (`CITO_STATE_DIR`, `CITO_CONFIG_PATH`, `CITO_OPEN_BIN`, `CITO_CODEX_HOME`, `CITO_CODEX_BIN`, `CITO_CLAUDE_HOME`) and exports `expandHomeDir(value: string, name?: string): string` so `loadCodexConfig(config)` and `loadClaudeConfig(config)` validate and expand `CITO_CODEX_HOME`, `CITO_CODEX_BIN`, `CITO_CLAUDE_HOME`, and `CITO_CLAUDE_CHANNEL` from `config.plugins?.[id]` (highest precedence, parsed from optional `"plugins"` in `config.json` or supplied programmatically in tests) and `config.env` (fallback):
    - `config.plugins?.codex` keys: `{ codexHome?: string; codexBin?: string; codexDesktopRunning?: (signal: AbortSignal) => Promise<boolean> }`.
    - `config.plugins?.claude` keys: `{ claudeHome?: string; enableClaudeChannel?: boolean }`.
  - Persisted SQLite rows (`local_listeners`, `local_signals`, `local_sent_idempotency`), socket paths (`${stateDir}/sockets/<id>-pid-<pid>.sock`), and Claude state paths (`${stateDir}/claude/{wakes,badges,aliases}`) remain 100% unchanged.

#### 2. Flat `plugin.json` Manifest & Plugin Loader Contract (`src/adapters/index.ts`, `bin/loader.js`)
- **Manifest & Public `@tefx/cito/plugin-sdk` Interfaces (`src/adapters/types.ts`)**:
  ```ts
  export type AdapterTier = "stable" | "experimental" | "custom";
  export type AdapterTint = "indigo" | "teal" | "orange" | "blue" | "purple" | "green" | "pink" | "gray";
  export interface PluginManifest {
    id: string;                // ^[a-z][a-z0-9_-]{0,31}$, must match directory name
    displayName: string;       // 1..64 printable chars
    tier?: AdapterTier;        // defaults to "custom"; forced to "custom" for external plugins
    sfSymbol?: string;         // defaults to "bolt.horizontal.fill"
    tint?: AdapterTint;        // defaults to "blue"
    jumpUrlTemplate?: string;  // optional URL template with {session_id} / {cwd}, safe custom scheme only
    cliFlag?: boolean;         // defaults to false
  }
  export interface HarnessCliDescriptor {
    consumesNextArg?(cmd: "listen" | "unlisten", nextArg: string, config: CitoConfig): boolean;
    resolveMeta(cmd: "listen" | "unlisten", explicitId: string | undefined, cwd: string, config: CitoConfig): Promise<Record<string, unknown>>;
  }
  export interface StdioChannelBridge {
    readonly socketPath?: string;
    sessionId?(): Promise<string>;
    forward(message: JSONRPCMessage): JSONRPCMessage;
    close(): Promise<void>;
  }
  export interface HarnessAdapter {
    readonly id: string;
    readonly manifest?: PluginManifest;
    readonly pidKeys?: readonly string[];
    readonly canJump?: boolean;
    readonly cli?: HarnessCliDescriptor;
    startStdioChannel?(
      config: CitoConfig,
      transport: { send(message: JSONRPCMessage): Promise<void> },
      options?: { endpointPid?: number; cwd?: string; env?: NodeJS.ProcessEnv },
    ): Promise<StdioChannelBridge | undefined>;
    detectExplicitCaller?(ctx: CallerDetectionContext, config: CitoConfig): Promise<Caller | null>;
    discoverAvailableSessions?(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]>;
    probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome>;
    deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome>;
    updateBadge?(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void>;
  }
  export type PluginFactory = HarnessAdapter | ((manifest: PluginManifest) => HarnessAdapter | Promise<HarnessAdapter>);
  ```
- **Entrypoint & SDK Resolution across Compiled (`dist/`) and Source-Only Layouts**:
  - From `src/adapters/index.ts` (`<root>/src/adapters/index.ts`) and `dist/src/adapters/index.js` (`<root>/dist/src/adapters/index.js`), relative specifier `"../../plugins/index.js"` resolves to `<root>/plugins/index.js` (retried as `.ts` by `loader.js`) in source mode and `<root>/dist/plugins/index.js` in compiled mode.
  - `bin/loader.js` always registers a lightweight `node:module.register` hook that:
    1. Resolves `"@tefx/cito/plugin-sdk"` to the active `src/adapters/index.ts` or `dist/src/adapters/index.js` URL so external plugins in `${CITO_STATE_DIR}/plugins/<id>/` can `import ... from "@tefx/cito/plugin-sdk"` without a local `node_modules` directory;
    2. Retries relative `.js` specifiers as `.ts` on `ERR_MODULE_NOT_FOUND` and strips TypeScript types for any `.ts` file (both in repo source mode and for external `~/.cito/plugins/<id>/index.ts` plugins even when Cito itself runs from `dist/`).
  - Synchronous built-in registration: `plugins/index.ts` registers the 3 built-in plugins (`pi`, `codex`, `claude`) so `ADAPTER_REGISTRY` is populated immediately upon importing `src/adapters/index.ts` (preserving synchronous `ADAPTER_REGISTRY.get(...)` and `getAdapter(...)` behavior in all existing unit tests).
  - External user plugins in `config.pluginDirs`: `loadUserPlugins(config)` scans `${stateDir}/plugins/<id>/` (and `CITO_PLUGIN_PATH`) during `CitoService.create()`, `discoverAllAvailableSessions()`, and CLI `runDirectCommand()`. For each `<dir>`, it validates `<dir>/plugin.json`, imports `<dir>/index.js` or `<dir>/index.ts` (`index.js` preferred over `index.ts`) if present, reads `const exported = mod.default as PluginFactory` (awaiting `exported(manifest)` when `typeof exported === "function"` and verifying `adapter.id === manifest.id`, or instantiating `new SocketBridgeAdapter(manifest.id, { manifest })` with a default `cli` descriptor returning `{ caller_pid: process.ppid, expected_harness: manifest.id, ...(explicitId ? { expected_session_id: explicitId } : {}), cwd }` when no entry script exists), and caches loaded plugin directories for the process lifetime.
  - Uninstalled socket fallback: `getAdapter(harness)` continues to auto-instantiate `new SocketBridgeAdapter(harness)` with default metadata (`displayName: harness.charAt(0).toUpperCase() + harness.slice(1)`, `sfSymbol: "bolt.horizontal.fill"`, `tint: "blue"`, `tier: "custom"`) for any valid `harnessIdSchema` socket `<id>-pid-<pid>.sock` that has no failed manifest. If an external plugin directory had an invalid manifest or failed to load, its ID is recorded in `rejectedPluginIds` so `getAdapter(id)` returns `undefined` and socket discovery does not silently bypass the manifest error.

#### 3. Four Conflict-Resolution & Fault-Isolation Invariants
1. **Reserved Built-in IDs (`pi`, `codex`, `claude`)**: Built-in plugins cannot be overridden by external plugins or replaced via `registerAdapter` unless explicitly forced in internal test fixtures. External plugins whose `plugin.json` `id` is reserved, mismatches `<dir>`, or duplicates another external plugin are rejected with a diagnostic warning on stderr.
2. **Reserved CLI Flags & Universal `--harness <id> [--session <sid>]`**:
   - `cito listen` and `cito unlisten` accept `--harness <id>` / `--harness=<id>` and `--session <sid>` / `--session=<sid>`. Passing `--session` without `--harness` (or `--<id>`) or passing an unknown/rejected harness ID fails with exit `1`.
   - Any plugin with `manifest.cliFlag === true` whose `id` is not in `RESERVED_CLI_FLAGS` (`hub`, `team`, `scope`, `tag`, `harness`, `session`, `help`, `version`, `setup`) also accepts `--<id>[=<sid>]` (using optional `adapter.cli.consumesNextArg(cmd, nextToken, config)` for space-separated `--<id> <sid>`).
   - Conflicting flags (e.g., `--codex` + `--harness pi`, or two different session IDs) fail with exit `1`.
3. **Caller Binding Arbitration (`resolveCaller`)**:
   - `getAllPidKeys()` unions `BASE_PID_KEYS` (`["endpoint_pid", "caller_pid"]`) with all registered plugins' `pidKeys` (`plugins/pi` contributes `["pi_pid"]`).
   - Evaluates `adapter.detectExplicitCaller(ctx, config)` (under a `1500ms` per-plugin deadline; a timeout or thrown `CitoError` fails caller binding rather than silently falling through to implicit PID binding) across all registered plugins. If `> 1` plugin returns a claim, or if `1` plugin returns a claim while `hasExplicitPid(meta)` is true, throws `INVALID_INPUT: Conflicting caller metadata`.
   - Otherwise walks parent PIDs bottom-up (`depth = 0..5`) and binds to the closest ancestor PID with a live socket `<id>-pid-<ancestor>.sock`.
4. **Per-Plugin `1500ms` Discovery Timeout & Single Stdio Channel Guard**:
   - `discoverAllAvailableSessions` wraps each plugin's `discoverAvailableSessions(config, signal)` in a `1500ms` timeout so a hanging external plugin never delays built-in discovery.
   - `probe` (`1500ms`) and `deliver` (`3000ms` per attempt via `ctx.runEffect` with owner/generation fencing and `markQueueWritten` checkpoints) preserve existing retry and cancellation semantics.
   - `startOptionalStdioChannel(config, stdio)` invokes `startStdioChannel` across plugins; if more than one plugin activates a channel in the same process, it closes them and logs an error.

---

### 2.2 Scalable NATS KV Discovery (`HubDirectoryIndex` in `src/mcp/service.ts`)
1. **Bounded Dual-Termination `broker.kv.watch()` Snapshot + Fallback + Coalesced TTL Cache (`HubDirectoryIndex`)**:
   - Each `HubSlot` owns a `HubDirectoryIndex` caching active `IndexedRegistration` entries with a `5,000 ms` freshness window, singleflight promise coalescing, and a `directoryGen` counter.
   - `watchSnapshot` and `boundedPointGetSnapshot` share `recordKvEntry` (ignoring `value.state !== "ACTIVE"`), and `pendingMutations` preserves local `upsert()` / `remove()` calls that occur while a snapshot is in flight.
2. **Read-Time Lease & Visibility Evaluation**:
   - Filters entries by `evaluateLease(item.value.state, currentBrokerIso, new Date(item.createdMs)) === "ACTIVE"` and `item.value.scope === "workspace" || item.value.principal_id === principal.principal_id`.

### 2.3 Actionable Pi Footer Status Badge (`src/mcp/receiver.ts`)
- `refreshSessionBadge()` strictly encodes unread notification count (`🔔 ${badgeSlug}` or `🔔 ${badgeSlug} (${unread})`), without appending `"!"` on `replay_gap_detected`.

---

## 3. Session Sorting & Candidate Discovery Rules (`plugins/pi/cito-bridge.ts`, `src/adapters/`, `plugins/`)

### 3.1 Pi Interaction Recency Tracking (`lastActiveMs` in `plugins/pi/cito-bridge.ts`)
- Tracks `lastActiveMs` on lifecycle events (`session_start`, `session_info_changed`, `session_tree`), `/cito` commands, and `deliver` notifications (never on read-only `identify` / `status` polls), returning `updated_at_ms: lastActiveMs` in `identify`.
- `SocketBridgeAdapter.discoverAvailableSessions` (`src/adapters/socket-bridge.ts`) sorts candidates by `updated_at_ms DESC, endpoint_pid DESC`.

### 3.2 Codex Desktop Candidate Sorting (`plugins/codex/index.ts`)
- Queries `state_5.sqlite` for unarchived user threads ordered by `recency_at_ms DESC, updated_at DESC, rowid DESC` (`LIMIT 8`).

### 3.3 Multi-Hub Candidate Retention & Per-Harness Quota (`discoverAllAvailableSessions` in `src/adapters/index.ts`)
- Computes `listening_hubs` for each candidate and excludes a candidate only when it is listening on all configured hubs (or matches a hub-less legacy test listener).
- Retains up to **8 candidates per harness** (ordered by built-in plugin registration order `pi` $\to$ `codex` $\to$ `claude`, then custom plugins alphabetically, with no global cross-harness cap so custom plugins are never starved when built-in harnesses have 8 candidates each).

---

## 4. Two-Surface macOS UI Architecture & Data-Driven Plugin Rendering (`src/ui/index.ts`, `src/mcp/service.ts`, `src/macos/cito-ui.swift`)

### 4.1 HCI & Systems Rationale
- Surface 1 (`380pt` `NSMenu` `cardMenu`): Ego-centric personal control plane (Zone 1 Active on This Mac, Zone 2 `MY OTHER HOSTS`, Zone 3 `AVAILABLE SESSIONS`, Zone 4 `Search Network Directory…` launcher).
- Surface 2 (`560 × 440 pt` `KeyPanel: NSPanel` `DirectoryPanel`): Allo-centric network search across all visible active sessions (`allPeers`, including local-host sessions on `this.hostId`).

### 4.2 Surface 1: Ego-Centric Menu Bar Dropdown (`380 pt` `NSMenu`)
1. **Header Bar (`380 × 40 pt`, `id = "header"`)**: `Cito` + dynamic subtitle + `hubStrip` pills and state-driven drawers.
2. **Zone 1 — Active on This Mac (`listeners`)**:
   - `360 × 58/72 pt` rounded cards (`id = "session:\(listener.scoped_id)"`).
   - When `hubs.count > 1`, renders `switch_hub:\(listener.scoped_id)` (`4pt` left of `scope:\(listener.scoped_id)`).
   - When `listener.can_jump` is `true` (or `listener.jump_url` is non-empty), renders the `22 × 22 pt` jump button (`id = "open:\(listener.scoped_id)"`, symbol `"arrow.up.forward.app"`, `toolTip = listener.jump_label ?? "Open in \(visual.name)"`) which opens `listener.jump_url` (or expands the harness's `jump_url_template` with `{session_id}`) via `openJumpUrl`.
3. **Zone 2 — My Other Hosts (`my_remote_listeners`)**: Collapsible `MY OTHER HOSTS · N` header + `34pt` rows.
4. **Zone 3 — Available Sessions (`!available.isEmpty`)**:
   - Section Header (`380 × 24 pt`, `id = "available_header"`): `"AVAILABLE SESSIONS"` + `target_hub:<hub>` pills when `hubs.count > 1`.
   - Collapsible Harness Group Header (`380 × 28 pt`, `id = "group:\(harness)"`):
     - Ordered by `harnesses` metadata `order` (`pi`: 0, `codex`: 1, `claude`: 2, custom: 3 + alphabetical `id`).
     - Group label (`11.5pt .semibold`): `"\(visual.name) · \(items.count) \(noun)"`, where `noun` is `"open"` when `items.contains(where: { $0.endpoint_pid != nil })`, `"recent"` when `visual.jump_url_template != nil`, and `"available"` otherwise (purely data-driven with zero harness-ID checks, preserving exact `"Pi · N open"`, `"Codex · N recent"`, `"Claude · N open"`, and `"<Custom> · N available"` strings!). When `visual.tier == "experimental"`, `groupControl.toolTip` is set to `"\(visual.name) (Experimental)"`.
5. **Zone 4 & Footer**: `open_directory` launcher row + `stop_all` / `quit` footer (`menuActions` indices `0..9` unchanged).

### 4.3 Surface 2: Spotlight-Style Network Directory Panel (`560 × 440 pt` `KeyPanel: NSPanel`)
- **Facet Bar (`560 × 32 pt`, `id = "directory_facet_bar"`)**:
  - Scope pills: `All` (`id = "dir_scope:all"`, `x: 16, width: 42`), `My Hosts` (`id = "dir_scope:mine"`, `x: 62, width: 76`).
  - Harness pills: `All Harnesses` (`id = "dir_harness:all"`, `x: 154, width: 98`), followed by the ordered union of registered plugins from `harnesses` and any additional harness IDs present in `directoryAllPeers` (starting at `x: 256` with `4pt` gap: `Pi` width `42` at `x: 256`, `Codex` width `58` at `x: 302`, `Claude` width `62` at `x: 364`, and any custom plugin pills placed at `x: 430+` with width `max(42, ceil(measureTextWidth(name, font: 11pt .medium)) + 20)`, hosted in a horizontal `NSScrollView` when total width exceeds `544pt`). If an active `directoryHarnessFilter` is no longer in `directoryAllPeers`, it remains visible until switched back to `"all"`.
- **Full-Index Query Execution (`query_directory`)**:
  - Searches full untruncated `IndexedRegistration` fields across all connected hubs before truncating top 100 matched rows for display and enforcing `< 128 KiB` UTF-8 frame size.

### 4.4 Wire Protocol, Prebuilt `arm64` Companion & Data-Driven Harness Metadata (`src/ui/index.ts`, `src/macos/cito-ui.swift`)
1. **Prebuilt Apple Silicon Binary with Automatic `swiftc` Fallback (`ensureUiCompiled` in `src/ui/index.ts`, `bin/cito-ui-darwin-arm64`)**:
   - `npm run compile` builds `bin/cito-ui-darwin-arm64` (`swiftc -O -target arm64-apple-macos13.0` + `strip -x` + `codesign -s - --force`).
   - `ensureUiCompiled(stateDir, source?)` installs `bin/cito-ui-darwin-arm64` into `${stateDir}/bin/cito-ui` via `readFile` + `writeFile(temp)` + `rename` in `< 200ms` without `/usr/bin/swiftc` when `src/macos/cito-ui.swift` is not newer than `bin/cito-ui-darwin-arm64` (with `5000ms` git checkout mtime tolerance), and falls back to `/usr/bin/swiftc` when `cito-ui.swift` is modified or a custom `source` path is passed.
   - `ensureUiCompiled(stateDir, source?)` also writes `${stateDir}/bin/harnesses.json` containing the serialized `UiHarnessMetadata[]` of currently registered plugins so `cito-ui` can load `harnesses.json` from its own executable directory (`CommandLine.arguments[0]`) at startup even when spawned directly in raw-stdin tests without `UiBridge`.
2. **Data-Driven Harness Metadata (`UiHarnessMetadata`) & Jump URL Contract**:
   - `DaemonToUiMessage` (`op: "state"`) includes `harnesses?: UiHarnessMetadata[]`:
     ```ts
     export interface UiHarnessMetadata {
       id: string;
       display_name: string;
       sf_symbol: string;
       tint: AdapterTint;
       tier: AdapterTier;
       order: number;
       jump_url_template?: string | null;
     }
     ```
   - `UiListenerSummary` and `BannerPayload` include optional `jump_url?: string | null` and `jump_label?: string | null`.
   - When `jumpUrlTemplate` is defined on a plugin, `expandJumpUrl(template, { session_id, cwd })` URL-encodes `{session_id}` (and `{cwd}` when present; if `{cwd}` is required by the template but absent on `BannerPayload`, jump is omitted) and validates that the resulting URL has a safe non-`javascript:`/`file:`/`data:`/`http:`/`https:` scheme.
   - `UiBridge.state(...)` and `UiBridge.banner(...)` in `src/ui/index.ts` automatically populate `harnesses` (from `ADAPTER_REGISTRY`) and `jump_url` / `jump_label` (when `listener.can_jump !== false`) whenever callers omit them, and cache `harnesses` in `UiBridge.snapshot` for replay on child restart. In `cito-ui.swift`, `Frame` decodes optional `harnesses: [HarnessMeta]?`, updates `self.harnesses` (invalidating the state-diff guard when `harnesses` changes), and falls back to `harnesses[target_harness]?.jump_url_template` when `payload.jump_url` is omitted in raw-stdin tests.

---

## 5. Direct Session Control Commands, Packaging & Versioning (`plugins/pi/cito-bridge.ts`, `src/mcp/cli.ts`, `package.json`)

### 5.1 Direct Session Control Commands
- **Pi Slash Command (`/cito` in `plugins/pi/cito-bridge.ts`)**: Standalone zero-dependency extension supporting `/cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]`.
- **CLI Direct Control (`cito listen` & `cito unlisten` in `src/mcp/cli.ts`)**: Supports `--harness <id> [--session <sid>]` and plugin `cliFlag` shorthands (`--codex[=<thread_id>]`, `--claude[=<session_id>]`).

### 5.2 Git & Global `npm install` Packaging Contract (`package.json`, `tsconfig.json`, `bin/`)
- `package.json`:
  - `"files": ["bin", "dist/src", "dist/plugins", "src", "plugins", "README.md"]`
  - `"exports": { ".": { "types": "./dist/src/mcp/service.d.ts", "default": "./dist/src/mcp/service.js" }, "./plugin-sdk": { "types": "./dist/src/adapters/index.d.ts", "default": "./dist/src/adapters/index.js" } }`
  - `"pi": { "extensions": ["./plugins/pi/cito-bridge.ts"] }`
  - `"scripts"` contains none of `prepare`, `build`, `prepack`, `preinstall`, `install`, or `postinstall`; `"tsx"` remains in `"devDependencies"` only.
- `tsconfig.json`: `"include": ["src/**/*.ts", "plugins/**/*.ts", "tests/**/*.ts"]` with `"erasableSyntaxOnly": true`.

### 5.3 Versioning, `/health` Handshake & Automatic Stale-Daemon Handover
- `CITO_VERSION` in `src/shared/index.ts` is `"0.12.0"`, matching `package.json` `"version"`. `ensureDaemon` automatically replaces any running daemon whose `/health.version !== CITO_VERSION`.

---

## 6. Bundled Plugins Specification (`plugins/pi`, `plugins/codex`, `plugins/claude`)

1. **`plugins/pi` (`tier: "stable"`)**:
   - `plugin.json`: `{ "id": "pi", "displayName": "Pi", "tier": "stable", "sfSymbol": "terminal.fill", "tint": "indigo" }`.
   - `index.ts`: registers `SocketBridgeAdapter("pi", { manifest, pidKeys: ["pi_pid"] })`.
   - `cito-bridge.ts`: standalone Pi extension.
2. **`plugins/codex` (`tier: "stable"`)**:
   - `plugin.json`: `{ "id": "codex", "displayName": "Codex", "tier": "stable", "sfSymbol": "sparkles", "tint": "teal", "jumpUrlTemplate": "codex://threads/{session_id}", "cliFlag": true }`.
   - `index.ts`: `CodexDesktopAdapter` reading `${codexHome}/state_5.sqlite` and delivering via `codex queue` + `open -g codex://threads/wake/<id>`.
3. **`plugins/claude` (`tier: "experimental"`)**:
   - `plugin.json`: `{ "id": "claude", "displayName": "Claude", "tier": "experimental", "sfSymbol": "cpu.fill", "tint": "orange", "cliFlag": true }`.
   - `index.ts`, `channel.ts`, `hook-cli.ts`: opt-in `CITO_CLAUDE_CHANNEL=1` socket bridge + `fsync` wake queue (`${stateDir}/claude/wakes/<session>.jsonl`) + `bin/cito-claude-hook` atomic drain and exact `batch_id` `cito_inbox` acknowledgment.

### 6.2 Stage 0 Test Migration & Verification Obligations
- `tests/auth-callout/cli.test.ts`:
  - Update the `pkg.files` assertion (`line 116`) to `["bin", "dist/src", "dist/plugins", "src", "plugins", "README.md"]`.
  - Add `"plugins"` to both the dist-less sandbox copy list (`line 128`) and the temporary Git repository copy list (`line 167`).
  - Add a discriminating plugin execution check verifying that built-in plugins and an external user plugin under `${stateDir}/plugins/cursor/` (importing `@tefx/cito/plugin-sdk`) load and execute in both the dist-less sandbox and the compiled `dist/` layout, and that `bin/cito-claude-hook` loads `plugins/claude/hook-cli` with zero stderr loader fallback errors.
- `tests/adapters/pi-host.test.ts` & `tests/mcp/support.ts`:
  - Update `cito-bridge.ts` paths from `src/pi-extension/cito-bridge.*` to `plugins/pi/cito-bridge.*`.
- `tests/shared/shared.test.ts` & `tests/adapters/*.test.ts`:
  - Migrate `codexHome`/`codexBin`/`claudeHome`/`enableClaudeChannel` assertions to test `loadCodexConfig` (`plugins/codex/index.ts`) and `loadClaudeConfig` (`plugins/claude/index.ts`).
- `tests/ui/lifecycle.test.ts`:
  - Preserve all existing built-in group header assertions (`"Pi · 5 open"`, `"Codex · 5 recent"`, `"Claude · 1 open"`), facet pill geometry (`dir_harness:claude` at `x: 364, width: 62`), and `"Open in Codex ↗"` banner actions, and add a custom plugin test verifying dynamic `harnesses` badge, group header, facet pill, and custom `jump_url` rendering.
