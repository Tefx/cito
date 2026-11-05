# Cito Architecture & Protocol Specification (V5 — Theory-Grounded macOS Menu Bar UI, Streamable HTTP & Pure Adapter Layer)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for coding agents that wakes or delivers messages to opted-in agent conversations across:
1. **Multiple NATS Hubs** (e.g., a personal Tailscale no-auth hub `personal` and a team OIDC hub `work` simultaneously),
2. **Users** (e.g., `alice`, `bob`),
3. **Hosts / Machines** (e.g., `macbook-work`, `macbook-personal`),
4. **Extensible Agent Harnesses** (shipped: **Pi Coding Agent** and **ChatGPT / Codex Desktop App**; open extensible architecture for **Claude Code** and future CLI/IDE harnesses), and
5. **Concurrent Conversations** within the same repository or working directory.

### 1.2 Language & Neutral Naming Policy
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names (such as `rda`, `casdoor`, `rcatwin`, `keppel`, `mingchen`) may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).

---

## 2. Module Boundaries, Pure Adapter Contract & UI State Contract

### 2.1 Acyclic Module Graph
`src/shared/` $\to$ `src/adapters/` & `src/ui/` $\to$ `src/mcp/`

- **`src/mcp/server.ts` Purity Invariant**: `src/mcp/server.ts` MUST NOT import `codexThreadId` or reference `"x-codex-turn-metadata"`, `"codex"`, or `"pi"`. It passes `{ meta, headers, httpClientPid, httpTransport: true }` into `service.call(name, args, context)`.
- **Protocol Version Negotiation (`src/mcp/server.ts`)**:
  - Ensures `'2026-07-28'` is present in `SUPPORTED_PROTOCOL_VERSIONS` alongside `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`, and `2024-10-07`.

### 2.2 UI State & Hub Lifecycle Contract (`src/ui/index.ts`, `src/mcp/service.ts`, `src/macos/cito-ui.swift`)
1. **Extended `UiListenerSummary`**:
   To provide complete session context (Information Scent) in the Menu Bar Panel, `UiListenerSummary` and Swift's `Listener` include optional `about` and `branch` fields (falling back gracefully when omitted in legacy/minimal payloads):
   ```ts
   export interface UiListenerSummary {
     hub: string;
     listener_id: string;
     public_id: string;
     scoped_id: string;
     harness: string;
     session_id: string;
     title: string;
     about?: string;
     cwd: string;
     repo: string | null;
     branch?: string | null;
     scope: "self" | "workspace";
     can_jump: boolean;
   }
   ```
   In `CitoService.pushUiState()` (`src/mcp/service.ts`), `about: row.about` and `branch: row.branch` are populated from `local_listeners`.
2. **Clean Manual `disconnect()` & Forced Interactive `login` (`src/mcp/service.ts`)**:
   - In `HubSlot.disconnect()`: when setting `this.manuallyDisconnected = true` and closing the broker, clear `this.error = null` alongside `this.status = "disconnected"`, so an intentional user `Disconnect` or `Cancel` does not leave a stale abort error (`"CitoError: Hub disconnected"`) in `slot.error`.
   - In `HubSlot.reconnect(interactive: boolean, { forceLogin = false } = {})` (invoked with `forceLogin: action.action === "login"` from `handleUiAction`): when `forceLogin === true` and `this.login` is defined, close any active `this.broker?.nc` first and force interactive OIDC re-authentication (`this.login.recover(null, true, signal)`) rather than short-circuiting on the existing connected broker. This ensures clicking **`Switch Account`** in the `connected (oidc)` state genuinely performs interactive OIDC re-login and principal-change fencing.

---

## 3. Theory-Grounded macOS Menu Bar UI & HUD Specification (`src/macos/cito-ui.swift`)

### 3.1 HCI & Visual Design Foundations
Every layout, control, and typography choice in `src/macos/cito-ui.swift` is governed by explicit HCI and perceptual principles:

1. **Shannon Information Theory ($I(x) = -\log_2 P(x)$) & Tufte's Data-Ink Ratio**:
   - **Zero-Entropy Default Hub Elimination**: When `hubs.count == 1 && hub.name == "default"`, $P(\text{hub} = \text{"default"}) = 1.0 \implies H = 0\text{ bits}$. Printing the config fallback key `"default"` in the header pill wastes horizontal space without conveying information. In single-default-hub mode, the header renders a compact icon trigger (`[ ● 🌐 ▾ ]` when `auth_mode == "none"` and `status == "connected" && error == nil`, or `[ ● <user> ]` when `auth_mode == "oidc"`, or `[ ● <State> ]` when not connected), never the literal word `"default"`.
   - **Redundant Harness Pill Elimination**: Because the left `34×34pt` tinted badge (`terminal.fill` in `.systemIndigo`, `sparkles` in `.systemTeal`, `cpu.fill` in `.systemOrange`) preattentively encodes `listener.harness`, the redundant floating `[Pi]` / `[Codex]` text pill is removed from Row 1.
2. **Norman's State-Driven Affordances & Hick–Hyman Law ($RT = a + b \log_2(n+1)$)**:
   - A Hub drawer MUST display only the valid state transitions for the hub's current `status` (`connected` vs `disconnected` vs `auth_required` vs `connecting`), rather than presenting `[Reconnect]` and `[Disconnect]` simultaneously when already disconnected or healthy.
   - Underneath, `menuActions` still registers the deterministic hub closures (`[login (if oidc), reconnect, disconnect]`) in fixed order so headless `test_action` index addressing remains 100% deterministic across state transitions.
3. **Gestalt Law of Proximity & Orthogonal Row Allocation**:
   - Anchoring the session card's inline action cluster (`[Self]` / `[Team]`, `[↗]`, `[✕]`) to **Row 1 (`y = cardHeight - 28`)** instead of vertically centering it across Rows 1–2 frees the full `296pt` horizontal span (`x: 54 .. 350` inside the `360pt` card) for **Row 2 (`about` description)** and **Row 3 (`short_id` copy pill + `cwd · branch`)**, expanding path width by $4.1\times$.
4. **AppKit `NSTextField` Cell Geometry (`measureTextWidth`)**:
   - Because `NSTextField(labelWithString:)` includes `2pt` left + `2pt` right internal `lineFragmentPadding`, measuring strings with `NSString.size(withAttributes:)` underestimates width by `4pt` (or more when font weight differs). All dynamic pill and button widths MUST be computed via `measureTextWidth(_ text: String, font: NSFont) -> CGFloat` using `ceil(NSTextField(labelWithString: text).fittingSize.width)` with the exact rendered `NSFont`.
5. **Preattentive Chromatic Discipline & Native SF Symbols (Treisman & Ware)**:
   - Full-color emojis (`🔒`, `👥`, `⏹`, `⧉`) are replaced by monochrome SF Symbols (`lock.fill`, `person.2.fill`, `doc.on.doc`, `arrow.up.forward.app`, `xmark`, `stop.fill`) inside `MenuControl` (via `leadingSymbol: String?`), reserving high-chroma hues strictly for semantic status signals (`.systemGreen`, `.systemOrange`, `.systemRed`, `.systemBlue`).

### 3.2 Bespoke Monochrome Vector Status Icon: `Cito Resonance Node` (`isTemplate = true`)
- **Canvas**: `18 × 18 pt` (`@2x` Retina = `36 × 36 px`), `image.isTemplate = true`.
- **Geometry**:
  1. **Primary C-Resonator Arc**: center `(8.0, 9.0)`, radius `5.50 pt`, angle `42°` to `318°`, `lineWidth = 1.75 pt`, `lineCapStyle = .round`, `alpha = 1.0`.
  2. **Focal Core Node**: center `(8.0, 9.0)`, radius `1.65 pt` (oval `3.3 × 3.3 pt`), solid fill (`alpha = 1.0` when `active`, `0.40` when `idle` or `warning`).
  3. **Outbound Signal Wave**: center `(8.0, 9.0)`, radius `7.75 pt`, angle `-28°` to `+28°`, `lineWidth = 1.65 pt`, `lineCapStyle = .round` (`alpha = 1.0` when `active`, `0.40` when `idle` or `warning`).
  4. **Warning Cutout Badge (State `.warning` only)**: at `(14.2, 4.0)`, punch a `r = 3.6 pt` transparent cutout (`.destinationOut`) and fill a `r = 2.4 pt` solid warning dot.
- **Icon State Rule**:
  - `let warning = hubs.contains { ($0.status != "connected" && !($0.status == "disconnected" && $0.error == nil)) || $0.error != nil }` (or `hubs.contains { $0.status != "connected" || $0.error != nil }` to preserve existing warning badge semantics).
- **Button Count Formatting (`NSStatusBarButton`)**:
  - `uniqueSessionCount == 0`: `button.title = ""`, `button.imagePosition = .imageOnly`.
  - `uniqueSessionCount > 0`: `button.imagePosition = .imageLeading`, `button.attributedTitle` = `" \(uniqueSessionCount)"` with `NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)`.

### 3.3 Control-Center Custom-View Menu Panel (`width: 380 pt`)

```text
┌──────────────────────────────────────────────────────────────────────┐ ← 380pt wide
│  Cito   1 listening                                        [ ● 🌐 ▾ ] │ ← Header (40pt): Compact icon trigger
├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─┤
│  Connected · alice                                    [ Disconnect ] │ ← Drawer (32pt): State-driven action
├──────────────────────────────────────────────────────────────────────┤
│ ┌──────────────────────────────────────────────────────────────────┐ │
│ │ ┌──────┐  agent                        [ 🔒 Self ]  [ ↗ ]  [ ✕ ] │ │ ← Row 1: Title + Top-right actions
│ │ │  >_  │  Local dotfiles/agent Pi main session                   │ │ ← Row 2: About description (296pt)
│ │ └──────┘  [ ⧉ dotfiles#rdap ]   ~/dotfiles/agent · main          │ │ ← Row 3: Untruncated ID + cwd · branch
│ └──────────────────────────────────────────────────────────────────┘ │
├──────────────────────────────────────────────────────────────────────┤
│  [ ■ Stop All ]                                     [ Quit Cito ⌘Q ] │ ← Footer (36pt)
└──────────────────────────────────────────────────────────────────────┘
```

1. **Header Bar (`380 × 40 pt`, `id = "header"`)**:
   - Left: **`Cito`** (`13pt .bold`) + subtitle (`"\(count) listening"` when `count > 0`, or `"Idle"` when `0`, `11.5pt .regular`, `.secondaryLabelColor`).
   - Right: Interactive Hub status pills (`height: 22pt, cornerRadius: 11pt`) inside `hubStrip` (`NSScrollView` at `x: 138, y: 9, width: 230, height: 22`, supporting horizontal scrolling when multiple pills exceed `230pt`):
     - **Single-Default-Hub Mode (`hubs.count == 1 && hub.name == "default"`)**:
       - Never displays the literal word `"default"`.
       - When `auth_mode == "none" && status == "connected" && error == nil`: renders a compact icon-only pill (`width: 44 pt, height: 22 pt`, `title = ""`, `6×6pt` green dot at `x: 7, y: 8` + `network` SF Symbol `12×12pt` at `x: 16, y: 5` + `chevron.down` `8×8pt` at `x: 31, y: 7`, `toolTip = "Connected · \(hub.user ?? "local")"`).
       - Otherwise (e.g., `auth_mode == "oidc"` where `title = hub.user ?? "Connected"`, or `status == "connecting"` $\to$ `"Connecting…"`, `status == "auth_required"` $\to$ `"Sign In"`, `status == "disconnected"` $\to$ `"Offline"`): renders `6×6pt` status dot + `title` with `width = min(180, measureTextWidth(title, font: .systemFont(ofSize: 11, weight: .medium)) + 26)`.
     - **Named or Multi-Hub Mode (`hubs.count > 1 || hub.name != "default"`)**:
       - Displays `6×6pt` status dot + `hub.name` with `width = min(180, measureTextWidth(hub.name, font: .systemFont(ofSize: 11, weight: .medium)) + 26)` and `toolTip = "\(hub.name) · \(hub.status)"`.
2. **State-Driven Hub Action Drawer (`380 × 32 pt`, `id = "drawer:\(hub.name)"`)**:
   - **Visibility & Toggle**: Auto-expanded by default when `hub.status != "connected" || hub.error != nil`; clicking `hub:<name>` toggles `drawer.isHidden = !drawer.isHidden` so the user can always expand or collapse any hub's drawer on demand.
   - **Left Status Summary (`NSRect(x: 12, y: 8, width: buttonsMinX - 20, height: 16)`)**:
     - If `hub.error != nil`: displays `hub.error!` (`toolTip = hub.error`)
     - Else if `status == "connected"` (`none`): `"Connected · \(hub.user ?? "local")"`
     - Else if `status == "connected"` (`oidc`): `"Connected as \(hub.user ?? "user")"`
     - Else if `status == "connecting"`: `"Connecting to hub…"`
     - Else if `status == "auth_required"`: `"Authentication required"`
     - Else (`disconnected`): `"Disconnected"`
   - **Right State-Driven Action Buttons** (each button sized via `measureTextWidth(title, font: .systemFont(ofSize: 11, weight: .medium)) + 16`, laid out right-to-left starting at `x = 368`; meanwhile `hubActions` always appends `[login (if oidc), reconnect, disconnect]` in deterministic order for `menuActions`):
     - `connected` (`none`, `error == nil`): **`Disconnect`** (`id: "disconnect:\(hub.name)"`, emits `disconnect`)
     - `connected` (`oidc`, `error == nil`): **`Switch Account`** (`id: "login:\(hub.name)"`, emits `login`) + **`Disconnect`** (`id: "disconnect:\(hub.name)"`, emits `disconnect`)
     - `connected` (`error != nil`, e.g., post-reconnect restoration error): **`Reconnect`** (`id: "reconnect:\(hub.name)"`, emits `reconnect`) + **`Disconnect`** (`id: "disconnect:\(hub.name)"`, emits `disconnect`)
     - `disconnected` (`none`): **`Connect`** (`id: "reconnect:\(hub.name)"`, primary `.systemBlue` tint, emits `reconnect`)
     - `disconnected` (`oidc`): **`Connect`** (`id: "reconnect:\(hub.name)"`, primary `.systemBlue` tint, emits `reconnect`) + **`Sign In…`** (`id: "login:\(hub.name)"`, emits `login`)
     - `auth_required`: **`Sign In…`** (`id: "login:\(hub.name)"`, primary `.systemBlue` tint, emits `login`) + **`Disconnect`** (`id: "disconnect:\(hub.name)"`, emits `disconnect`)
     - `connecting`: **`Cancel`** (`id: "disconnect:\(hub.name)"`, emits `disconnect`)
3. **Active Session Cards (`width: 360 pt`, `x: 10`, `cornerRadius: 10 pt`)**:
   - **Dynamic Height**:
     - Let `aboutText = (listener.about ?? "").trimmingCharacters(in: .whitespacesAndNewlines)`.
     - Let `hasDistinctAbout = !aboutText.isEmpty && aboutText.caseInsensitiveCompare(listener.title.trimmingCharacters(in: .whitespacesAndNewlines)) != .orderedSame`.
     - If `hasDistinctAbout == true`: row height is `78 pt`, card frame is `NSRect(x: 10, y: 3, width: 360, height: 72)` (3-row layout).
     - If `hasDistinctAbout == false`: row height is `64 pt`, card frame is `NSRect(x: 10, y: 3, width: 360, height: 58)` (2-row layout).
   - **Left Harness Badge (`34 × 34 pt`, `x: 10`, `y: cardHeight - 44`, `cornerRadius: 8 pt`)**:
     - `"pi"` $\to$ `"terminal.fill"` (`.systemIndigo`); `"codex"` $\to$ `"sparkles"` (`.systemTeal`); `"claude"` $\to$ `"cpu.fill"` (`.systemOrange`).
   - **Row 1 (`y: cardHeight - 26`)**:
     - **Top-Right Action Cluster (`y: cardHeight - 28, height: 22 pt`)**:
       - Total width `actionWidth = listener.can_jump ? 114 : 88`, starting at `x = 350 - actionWidth`.
       1. **Scope Toggle (`id: "scope:\(listener.scoped_id)"`, `width: 62 pt, height: 22 pt`)**: Uses `leadingSymbol: scope == "self" ? "lock.fill" : "person.2.fill"` + `label.stringValue = scope == "self" ? "Self" : "Team"`. Clicking toggles between `"self"` and `"workspace"`, updating both `leadingSymbol` and `label.stringValue` (`"Team"` / `"Self"`).
       2. **Open Button (`id: "open:\(listener.scoped_id)"`, `22 × 22 pt`, when `listener.can_jump`)**: SF Symbol `"arrow.up.forward.app"`.
       3. **Stop Button (`id: "stop:\(listener.scoped_id)"`, `22 × 22 pt`)**: SF Symbol `"xmark"` (turns `.systemRed` on hover).
     - **Session Title**: `listener.title` (+ `" · \(listener.hub)"` when `hubs.count > 1`), `13pt .semibold`, frame `NSRect(x: 54, y: cardHeight - 26, width: 350 - 54 - actionWidth - 6, height: 18)`. No redundant `[Pi]` / `[Codex]` text capsule is rendered.
   - **Row 2 (`y: 28`, rendered when `hasDistinctAbout == true`, full width `296 pt` from `x: 54` to `x: 350`)**:
     - Displays `aboutText` (`11.5pt .regular`, `.secondaryLabelColor`, `.byTruncatingTail`, `toolTip = aboutText`).
   - **Bottom Row (`y: 7`, full width `296 pt` from `x: 54` to `x: 350`)**:
     - **Untruncated Short ID Copy Pill (`id: "copy:\(listener.scoped_id)"`, `height: 19 pt`)**:
       - Formats `<slug>-<fp12>` as `<slug>#<fp4>` (if `<slug>` exceeds 18 chars, middle-truncates `<slug>` to 16 chars before `#<fp4>`), with `leadingSymbol: "doc.on.doc"` and `label.stringValue = short`.
       - Width is computed via `let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium); let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 24` with **no artificial `0.72` multiplier**, guaranteeing neither `<slug>#<fp4>` nor `"✓ Copied"` is ever clipped.
       - Clicking copies `listener.scoped_id` to `NSPasteboard.general`, hides `leadingSymbolView` while setting `label.stringValue = "✓ Copied"` in `.systemGreen` for 1.2s, and then restores `leadingSymbolView` and `short`.
     - **Workspace Path & Branch (`x: 54 + copyWidth + 8`, `width: 296 - copyWidth - 8`)**:
       - Formats `cwd` (`$HOME` $\to$ `~`) + optional `" · \(branch)"` (when `branch` is non-empty and not `"HEAD"`), `11pt .regular`, `.tertiaryLabelColor`, `.byTruncatingMiddle`, `toolTip = listener.cwd`.
   - **Empty State Card (`360 × 64 pt`, shown when `listeners.isEmpty`)**: SF Symbol `"waveform.slash"` + **`No active listeners`** + **`Call cito_listen in a session to receive notifications`**.
4. **Footer Bar (`380 × 36 pt`, `id = "footer"`)**:
   - Left (when `!listeners.isEmpty`): **`Stop All`** (`leadingSymbol: "stop.fill"`, `width: 82 pt, height: 24 pt, x: 12, y: 6`).
   - Right: **`Quit Cito ⌘Q`** (`x: 262, y: 6, width: 106 pt, height: 24 pt`).

---

## 4. Direct Session Control Commands (`Pi /cito` & CLI `cito listen` / `cito unlisten`)

To let users enable or disable listening immediately—without spending an LLM turn, waiting for a busy agent, or polluting conversation history—Cito provides two lightweight, zero-token entrypoints that reuse `cito-daemon`'s existing `POST /mcp` (`tools/call` `cito_listen` / `cito_unlisten`) pipeline:

### 4.1 Pi Slash Command (`/cito` in `src/pi-extension/cito-bridge.ts`)
- **Zero-Dependency Standalone Contract**: `src/pi-extension/cito-bridge.ts` is copied standalone to `~/.pi/agent/extensions/cito-bridge.ts` and MUST NOT import any `../shared/*` or external npm modules.
- **Registration & Completion**:
  - Registers `pi.registerCommand?.("cito", { description: "Toggle or configure Cito session listening", getArgumentCompletions, handler })`.
  - `getArgumentCompletions(prefix)` filters `["on", "team", "off"]` by `startsWith(prefix.trim())`.
- **Subcommands**:
  - `/cito` (no arguments): toggles listening for the current Pi session (`activeBadge ? cito_unlisten({}) : cito_listen({ about: defaultAbout(ctx), scope: "self" })`).
  - `/cito on [about...]`: calls `cito_listen({ about: rest || defaultAbout(ctx), scope: "self" })`.
  - `/cito team [about...]`: calls `cito_listen({ about: rest || defaultAbout(ctx), scope: "workspace" })`.
  - `/cito off`: calls `cito_unlisten({})` (rejecting trailing arguments with the usage warning).
  - Any unknown subcommand notifies `ctx.ui.notify?.("Usage: /cito [on [about...] | team [about...] | off]", "warning")` without mutating state.
- **Transport & Caller Binding**:
  - Sends a stateless `POST http://127.0.0.1:${port}/mcp` with headers `Content-Type: application/json` and `Accept: application/json, text/event-stream`, method `"tools/call"`, and `params._meta: { endpoint_pid: process.pid }`.
  - Parses either SSE (`data: {...}`) or JSON response payloads, and reports the outcome via `ctx.ui.notify?.(...)` (`"Cito listening as <listener_id> (<scope>)"`, `"Cito stopped listening"`, or error message).

### 4.2 CLI Direct Control (`cito listen` & `cito unlisten` in `src/mcp/cli.ts`, `src/adapters/codex.ts`)
- **Invocation**:
  - `cito listen [--scope self|workspace] [--team] [--hub <name>] [--tag <tag>] [--codex[=<thread_id>]] [about...]`
  - `cito unlisten [--hub <name>] [--codex[=<thread_id>]]`
  - Can be invoked from any terminal or inline inside Pi / Claude Code via `!cito listen` and `!cito unlisten` (0 tokens, `<50ms`).
- **Caller Metadata Disambiguation**:
  - **Default (Pi / Claude Code / Socket Bridge ancestry)**: sends `_meta: { caller_pid: process.ppid }` (must NOT include `codexThreadId`).
  - **Codex Mode (`--codex` or `--codex=<thread_id>` or `--codex <uuid>`)**: sends `_meta: { codexThreadId, cwd: process.cwd() }` (must NOT include `caller_pid`, `endpoint_pid`, or `pi_pid`).
  - When `--codex` is passed without an explicit UUID, `resolveCodexThreadId(process.cwd(), config, { allowArchived: cmd === "unlisten" })` in `src/adapters/codex.ts` queries `state_5.sqlite` (dynamically checking `PRAGMA table_info(threads)` for `updated_at` vs `rowid` ordering), preferring `archived = 0 AND cwd = ?` before falling back to `archived = 0` (and archived rows when `allowArchived: true`).
- **Execution & Output**:
  - Calls `await ensureDaemon(config)` and `POST http://127.0.0.1:${config.daemonPort}/mcp` (`Accept: application/json, text/event-stream`), decoding the SSE `data:` frame.
  - On success, prints a single-line summary to `stdout` (`Listening as <listener_id> (<scope>): <about>\n` or `Unlistened (<removed> removed)\n`) and exits `0`; on error, prints to `stderr` and exits non-zero.

### 4.3 Menu Bar On-Demand Session Discovery, 1-Click Listen, Viewport Capping & Flicker-Free Dynamic Refresh (`src/adapters/`, `src/ui/index.ts`, `src/mcp/service.ts`, `src/macos/cito-ui.swift`)

To provide a zero-intrusion, 1-click GUI control plane for local desktop sessions (especially Codex Desktop, which has no local zero-token slash-command extension API) while preserving zero background polling when idle:

1. **Adapter Session Discovery Contract (`HarnessAdapter.discoverAvailableSessions`)**:
   - `HarnessAdapter` (`src/adapters/types.ts`) adds an optional method:
     ```ts
     export interface AvailableSessionSummary {
       harness: string;
       session_id: string;
       title: string;
       cwd: string;
       branch?: string | null;
       endpoint_pid?: number;
       updated_at_ms?: number;
     }
     ```
     `discoverAvailableSessions?(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]>`
   - **`SocketBridgeAdapter` (`src/adapters/pi.ts`)**:
     - `identifyBridge` accepts an optional `timeoutMs = 1500` parameter (`identifyBridge(harness, pid, config, signal, timeoutMs = 1500)`).
     - `discoverAvailableSessions` scans `${config.stateDir}/sockets/${this.id}-pid-*.sock`, verifies `process.kill(pid, 0)` succeeds, calls `identifyBridge(this.id, pid, config, signal, 400)` concurrently (isolating per-socket errors), and returns live sessions ordered by socket `mtimeMs` descending.
   - **`CodexDesktopAdapter` (`src/adapters/codex.ts`)**:
     - Checks `await (config.codexDesktopRunning ?? codexDesktopRunning)(signal)` first; if Codex Desktop is not running, returns `[]` immediately (preventing ghost threads when the app is closed).
     - Opens `${config.codexHome}/state_5.sqlite` read-only (`PRAGMA busy_timeout = 100`, closed in `finally`) and inspects `PRAGMA table_info(threads)`. If `id`, `cwd`, or `archived` is absent, returns `[]`.
     - Dynamically builds user-thread predicates only from columns present in `PRAGMA table_info(threads)`:
       - `if (cols.has("thread_source")) filters.push("COALESCE(thread_source, 'user') = 'user'");`
       - `if (cols.has("source")) filters.push("COALESCE(source, '') NOT LIKE '{\"subagent\":%'");` (using `COALESCE` so `NULL` values are never dropped by SQLite 3-valued logic).
     - Dynamically builds `ORDER BY`: `[...(cols.has("recency_at_ms") ? ["recency_at_ms DESC"] : []), ...(cols.has("updated_at") ? ["updated_at DESC"] : []), "rowid DESC"].join(", ")`.
     - Queries `SELECT * FROM threads WHERE ${["archived = 0", ...filters].join(" AND ")} ORDER BY ${order} LIMIT 8` and computes `title = (typeof row.name === "string" && row.name.trim()) || (typeof row.title === "string" && row.title.trim()) || basename(row.cwd)` in TypeScript.
     - Shares the same schema-aware user-thread predicate helper across all branches of `resolveCodexThreadId()` (retaining `cwd` precedence and `allowArchived` fallback).
   - **`discoverAllAvailableSessions(config, activeListeners, signal)` (`src/adapters/index.ts`)**:
     - Runs `discoverAvailableSessions` across registered adapters in parallel (catching individual adapter errors so one failing adapter never discards healthy results), filters out any session already present in `activeListeners` (matching `(harness, session_id)` or `(harness, endpoint_pid)`), sorts live socket sessions first followed by recent Codex user sessions, and caps the combined result at **10 candidates maximum**.

2. **UI Wire Protocol, Identity Fencing & Synchronous State Publication (`src/ui/index.ts`, `src/mcp/service.ts`)**:
   - `DaemonToUiMessage` `state` frame adds optional `available?: AvailableSessionSummary[]`. `UiBridge.state(hubs, listeners, available = [])` defaults `available` to `[]` so all existing 2-argument callers and tests remain 100% backward-compatible.
   - `UiToDaemonAction` adds two actions validated in `validAction()` (`src/ui/index.ts`) **before** the `a.hub` check:
     1. `{ op: "refresh_available" }`: triggered by `cito-ui.swift` on `NSMenuDelegate.menuWillOpen(_:)`.
     2. `{ op: "listen_session"; harness: string; session_id: string; endpoint_pid?: number; cwd?: string; title?: string }`: validated for a valid harness ID, non-empty `session_id`, optional positive safe-integer `endpoint_pid`, and optional string `cwd`/`title`.
   - **Identity Fencing on `listen_session`**:
     - In `resolveCaller()` (`src/adapters/index.ts`), when `meta.expected_session_id` (or `meta.expected_harness`) is supplied alongside `endpoint_pid`, verify that the resolved caller's `(harness, session_id)` matches `(expected_harness, expected_session_id)`; otherwise throw `new CitoError("SESSION_NOT_OPEN", "Session changed since discovery")`.
     - In `CitoService.handleUiAction` for `listen_session`, sanitize `about` (`(action.title || basename(action.cwd || "") || "Session").replace(/[\r\n]+/g, " ").trim().slice(0, 200) || "Session"`) and call `this.listen({ about, scope: "self" }, action.endpoint_pid ? { meta: { endpoint_pid: action.endpoint_pid, expected_harness: action.harness, expected_session_id: action.session_id } } : { meta: { codexThreadId: action.session_id, cwd: action.cwd, title: action.title } })`, followed by `void this.refreshAvailableSessions()`.
   - **Synchronous `pushUiState()` + Generation-Guarded `refreshAvailableSessions()`**:
     - `pushUiState(): void` remains **synchronous**: it filters cached `this.availableSessions` against current active `local_listeners` rows (`(harness, session_id)` and `(harness, endpoint_pid)`) and calls `this.ui?.state(hubs, listeners, available)`. Thus, when `listen()` completes, its synchronous `finally { this.pushUiState(); }` immediately removes the newly enrolled session from `available` in the same frame.
     - `async refreshAvailableSessions(): Promise<void>` increments `const gen = ++this.discoveryGen`, awaits `discoverAllAvailableSessions(this.config, activeRows, this.shuttingDown.signal)`, and if `gen === this.discoveryGen && !this.closing`, updates `this.availableSessions` and calls `this.pushUiState()`.
     - Triggered from `startUi()`, `handleUiAction({ op: "refresh_available" })`, `unlistenById()`, `unlistenAll()`, `revokeSession()`, and a debounced (`150ms`) `fs.watch` on `${config.stateDir}/sockets` (created via `mkdir(..., { recursive: true, mode: 0o700 })` first, with `.on("error", () => {})` and cleaned up in `close()`).

3. **Menu Bar UI Layout, Viewport Capping & Flicker-Free Dynamic Refresh (`src/macos/cito-ui.swift`)**:
   - **Figure-Ground Visual Hierarchy**:
     - **Active Listeners (Top Section — Primary Cards)**: `360 × 58/72 pt` rounded cards (`radius: 10pt`, `alpha: 0.045`) inside `64/78 pt` row views. When `listeners.count > 4`, the active row views are hosted inside an `NSScrollView` (`id = "active_scroll"`, `width: 380, height: 260 pt`) using a top-down `FlippedView` (`override var isFlipped: Bool { true }`, document height = sum of `64/78 pt` row heights); when `1 <= listeners.count <= 4`, each row is hosted directly as its own `NSMenuItem`.
     - **Compact Empty Banner When `listeners.isEmpty && !available.isEmpty`**: Renders a `380 × 34 pt` compact strip (`id = "empty"`, inner pill `360 × 28 pt` at `x: 10, y: 3, radius: 8`) with `waveform.slash` (`14×14pt`) and `"No active listeners — click + Listen below"`. (When both `listeners.isEmpty && available.isEmpty`, renders the full `70pt` empty card `"No active listeners"`).
     - **Available Sessions Section (`!available.isEmpty`)**:
       - **Section Header (`id = "available_header"`, `380 × 24 pt`)**: `1pt` top separator (`x: 12, y: 21, width: 356`, `alpha: 0.08`) + `"AVAILABLE SESSIONS"` (`10pt .semibold`, `.secondaryLabelColor`, `NSRect(x: 12, y: 4, width: 220, height: 14)`).
       - **Candidate Rows (`380 × 36 pt`, `id = "candidate:\(c.harness):\(c.session_id)"`)**:
         - Left: `22 × 22 pt` muted harness badge (`x: 12, y: 7, radius: 6`, `tint.withAlphaComponent(0.08)` fill, `12×12pt` SF Symbol in `tint.withAlphaComponent(0.75)`).
         - Center (`NSRect(x: 42, y: 9, width: 254, height: 18)`): `"<title> · <short_cwd>"` (`toolTip = "\(c.title) — \(c.cwd)"`).
         - Right (`NSRect(x: 304, y: 7, width: 64, height: 22)`): `MenuControl("Listen", ..., leadingSymbol: "plus", radius: 6)` (`id = "listen:\(c.harness):\(c.session_id)"`).
       - **Viewport Capping (`max 4 visible rows = 144 pt`)**:
         - When `available.count <= 4`, candidate rows are hosted directly (`36pt` each).
         - When `available.count > 4`, candidate rows are hosted inside an `NSScrollView` (`id = "available_scroll"`, `width: 380, height: 144 pt`, using `FlippedView` with document height `available.count * 36 pt`) supporting trackpad scrolling for up to `10` candidates.
   - **Flicker-Free Dynamic Refresh, `eventTracking` RunLoop Delivery & `menuActions` Compatibility**:
     - **Persistent `NSMenu` Instance & `NSMenuDelegate`**: `Companion` conforms to `NSMenuDelegate`, creates a single `let cardMenu = NSMenu()` (`cardMenu.autoenablesItems = false; cardMenu.delegate = self; status?.menu = cardMenu`), and emits `["op": "refresh_available"]` in `menuWillOpen(_:)` (also exposed via headless `{"op": "test_menu_open"}`). `rebuild()` mutates `cardMenu` in-place (`cardMenu.removeAllItems()`, repopulates items, and calls `cardMenu.update()`) without replacing `status?.menu`.
     - **Event-Tracking RunLoop Delivery**: Incoming `STDIN` frames are dispatched via `RunLoop.main.perform(inModes: [.common, .eventTracking]) { self?.receive(line) }` so frames arriving while the menu is open update the visible menu immediately.
     - **`Equatable` State Diff Guard with `hasRenderedState` & `pendingMutation`**: `Hub`, `Listener`, and `Candidate` conform to `Equatable`. `Companion` tracks `hasRenderedState = false` and `pendingMutation = false` (set to `true` whenever a button optimistically mutates its label in-place, such as `scope` toggle or candidate `Listen` $\to$ `"…"`). `receive()` skips `rebuild()` only when `hasRenderedState && !pendingMutation && hubs == h && listeners == l && available == a`, ensuring initial renders and failed-action rollbacks are never skipped.
     - **`emit([String: Any])` & `menuActions` Index Stability**: `emit(_ action: [String: Any])` serializes `endpoint_pid` as a JSON number (`Int`). Candidate `listen` closures are appended to `menuActions` **after** `quit` at the end of `rebuild()`, preserving existing action indices `0..9`.

---

## 5. Optional `CITO_CLAUDE_CHANNEL=1` Stdio Channel Bridge (`src/mcp/cli.ts`, `src/adapters/pi.ts`)

- When `env.CITO_CLAUDE_CHANNEL === "1"` (or `config.enableClaudeChannel === true`, default `false`):
  - `cito-mcp` in stdio mode advertises `capabilities: { tools: {}, experimental: { "claude/channel": {} } }`, binds `${stateDir}/sockets/claude-pid-${process.ppid}.sock`, and emits `notifications/claude/channel` on `deliver` (`confirmDelivery: false` $\to$ `delivered_unconfirmed`).

---

## 6. Proposed Design for Claude Code Integration (Design Suggestion Only — Not Yet Implemented)

> **Status Notice (Proposed Design Suggestion Only — Not Yet Implemented)**: This section records the architectural design suggestion for full native Claude Code integration. The opt-in `CITO_CLAUDE_CHANNEL=1` stdio forwarding bridge provides experimental channel push, while native Claude Code lifecycle hooks and session-discovery integration remain a design proposal for future implementation.

1. **Session Identity & Transport Topology**:
   - Claude Code sessions can connect either directly to `cito-daemon`'s stateless Streamable HTTP endpoint (`http://127.0.0.1:4290/mcp`) for pull-based `cito_inbox()` workflows, or through the local stdio forwarder with experimental channel support (`--channels`) so each terminal session binds its parent PID and working directory to a dedicated local bridge endpoint.
2. **Dual-Mode Notification Delivery (Channel Push + Turn-Start Inbox Drain)**:
   - **Active Push (MCP Channel Notification)**: When launched with channel notifications enabled, incoming wake batches are pushed over the session's stdio MCP stream via `notifications/claude/channel` to wake an idle Claude Code conversation.
   - **Turn-Start Pull Fallback (Inbox Drain)**: Because channel push requires explicit client flags and may be ignored while a turn is busy, deliveries are recorded as unconfirmed (`delivered_unconfirmed`) so `cito_inbox()` (or a `UserPromptSubmit` hook) can reliably drain and acknowledge unread notifications at the start of the next turn.
