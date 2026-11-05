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

## 4. Optional `CITO_CLAUDE_CHANNEL=1` Stdio Channel Bridge (`src/mcp/cli.ts`, `src/adapters/pi.ts`)

- When `env.CITO_CLAUDE_CHANNEL === "1"` (or `config.enableClaudeChannel === true`, default `false`):
  - `cito-mcp` in stdio mode advertises `capabilities: { tools: {}, experimental: { "claude/channel": {} } }`, binds `${stateDir}/sockets/claude-pid-${process.ppid}.sock`, and emits `notifications/claude/channel` on `deliver` (`confirmDelivery: false` $\to$ `delivered_unconfirmed`).

---

## 5. Proposed Design for Claude Code Integration (Design Suggestion Only — Not Yet Implemented)

> **Status Notice (Proposed Design Suggestion Only — Not Yet Implemented)**: This section records the architectural design suggestion for full native Claude Code integration. The opt-in `CITO_CLAUDE_CHANNEL=1` stdio forwarding bridge provides experimental channel push, while native Claude Code lifecycle hooks and session-discovery integration remain a design proposal for future implementation.

1. **Session Identity & Transport Topology**:
   - Claude Code sessions can connect either directly to `cito-daemon`'s stateless Streamable HTTP endpoint (`http://127.0.0.1:4290/mcp`) for pull-based `cito_inbox()` workflows, or through the local stdio forwarder with experimental channel support (`--channels`) so each terminal session binds its parent PID and working directory to a dedicated local bridge endpoint.
2. **Dual-Mode Notification Delivery (Channel Push + Turn-Start Inbox Drain)**:
   - **Active Push (MCP Channel Notification)**: When launched with channel notifications enabled, incoming wake batches are pushed over the session's stdio MCP stream via `notifications/claude/channel` to wake an idle Claude Code conversation.
   - **Turn-Start Pull Fallback (Inbox Drain)**: Because channel push requires explicit client flags and may be ignored while a turn is busy, deliveries are recorded as unconfirmed (`delivered_unconfirmed`) so `cito_inbox()` (or a `UserPromptSubmit` hook) can reliably drain and acknowledge unread notifications at the start of the next turn.
