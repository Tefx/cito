# Cito Architecture & Protocol Specification (V4 — macOS Menu Bar Daemon, Streamable HTTP & Production Card UI)

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
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).

### 1.3 V4 Implementation Scope vs. Proposed Design Scope
- **In Scope for Implementation & Verification**:
  1. **Single Per-User macOS Daemon (`cito daemon`) & Streamable HTTP MCP (`http://127.0.0.1:4290/mcp`, MCP `2026-07-28` + `2025-*`)**:
     - Registers `'2026-07-28'` in `SUPPORTED_PROTOCOL_VERSIONS` alongside `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`, and `2024-10-07`.
     - Serves stateless-per-request `StreamableHTTPServerTransport({ sessionIdGenerator: undefined })` at `http://127.0.0.1:4290/mcp` and health at `GET /health` (`{ ok: true, pid, stateDir }`).
     - Eliminates per-tab MCP subprocess memory overhead in Codex Desktop and Pi, and uses in-memory `KeyedMutex` instances inside the singleton daemon process.
  2. **Pure Adapter Layer (`HarnessAdapter.detectExplicitCaller`) & Optional `CITO_CLAUDE_CHANNEL=1` Bridge**:
     - `src/mcp/server.ts` has zero references to `codex`, `pi`, or `x-codex-turn-metadata`; it passes generic `{ meta, headers, httpClientPid, httpTransport }` into `resolveCaller` in `src/adapters/`.
     - `CodexDesktopAdapter` owns extraction and conflict validation of `x-codex-turn-metadata` from both JSON-RPC `_meta` and HTTP request headers.
     - When optional `CITO_CLAUDE_CHANNEL=1` (or `enableClaudeChannel: true` in config, default `false`) is enabled on the `cito-mcp` stdio forwarder, it advertises `capabilities: { tools: {}, experimental: { "claude/channel": {} } }`, binds `${stateDir}/sockets/claude-pid-${process.ppid}.sock`, and translates `{ op: "deliver" }` into a `notifications/claude/channel` JSON-RPC notification on `stdout` (returning `delivered_unconfirmed` so `cito_inbox()` remains available as a turn-start drain fallback).
  3. **Production-Grade English macOS Menu Bar UI & Bespoke Vector Icon (`src/macos/cito-ui.swift`)**:
     - Replaces the emoji status bar string with a custom-drawn `18×18pt` monochrome template vector icon (`makeCitoStatusIcon`, `image.isTemplate = true`, `1.75pt` stroke) plus a monospaced count badge.
     - Replaces plain-text `NSMenuItem` submenus with a `340pt`-wide Control-Center-style custom AppKit card layout inside `NSMenuItem.view` (Header Bar with Hub status pills and expandable Hub action drawer, `320×58pt` rounded session cards with `32×32pt` Harness badge, click-to-copy short ID pill `slug#fp4 ⧉` with inline `✓ Copied` feedback, and 1-click inline `🔒 Self` / `👥 Team`, `↗`, and `✕` buttons, plus Footer Bar).
     - All HUD banner strings (`Notification`, `Notification · ×N`, `Passive`, `Open in Codex ↗`, `<Harness> session · Click to dismiss`) and multi-signal Wake Card headers (`🔔 Cito · N notifications`) are in English.
  4. **Standard, Vendor-Neutral OIDC (`src/auth-callout/oidc.ts`, `src/mcp/oidc.ts`)**:
     - Supports `RS256`, `RS384`, `RS512`, `PS256`, `ES256`, `ES384`, and `EdDSA`, discovery-driven `scopes_supported` negotiation, optional fixed `oidcRedirectPort`, and principal-change fencing.
- **Proposed Design Suggestion Only (Not Yet Implemented)**:
  - **Claude Code Native Integration (`claude`)**: Documented in **Section 5** strictly as a **Proposed Design Suggestion Only — Not Yet Implemented**, describing the architectural design without concrete implementation code details.

---

## 2. Module Boundaries & Pure Adapter Contract

### 2.1 Acyclic Module Graph
`src/shared/` $\to$ `src/adapters/` & `src/ui/` $\to$ `src/mcp/`

- **`src/mcp/server.ts` Purity Invariant**: `src/mcp/server.ts` MUST NOT import `codexThreadId` or reference `"x-codex-turn-metadata"`, `"codex"`, or `"pi"`. It passes `{ meta, headers, httpClientPid, httpTransport: true }` into `service.call(name, args, context)`.
- **`HarnessAdapter` Contract (`src/adapters/types.ts`)**:
  ```ts
  export interface CallerDetectionContext {
    meta: Record<string, unknown>;
    headers?: Record<string, string | string[] | undefined>;
    httpClientPid?: number;
    httpTransport?: boolean;
    requireOpen?: boolean;
    lookupCachedSession?: (query: CachedSessionQuery) => Caller | undefined | Promise<Caller | undefined>;
  }

  export interface HarnessAdapter {
    readonly id: string;
    detectExplicitCaller?(ctx: CallerDetectionContext, config: CitoConfig): Promise<Caller | null>;
    probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome>;
    deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome>;
    updateBadge?(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void>;
  }
  ```
- **Protocol Version Negotiation (`src/mcp/server.ts`)**:
  - Ensures `'2026-07-28'` is present in `SUPPORTED_PROTOCOL_VERSIONS` (`if (!SUPPORTED_PROTOCOL_VERSIONS.includes("2026-07-28")) SUPPORTED_PROTOCOL_VERSIONS.unshift("2026-07-28")`) so both `2026-07-28` stateless clients and `2025-*` clients pass `MCP-Protocol-Version` validation.

---

## 3. Bespoke Vector Menu Bar Icon & Custom Card Panel (`src/macos/cito-ui.swift`)

### 3.1 Bespoke Monochrome Vector Status Icon: `Cito Resonance Node` (`isTemplate = true`)
- **Canvas**: `18 × 18 pt` (`@2x` Retina = `36 × 36 px`), `image.isTemplate = true`.
- **Geometry**:
  1. **Primary C-Resonator Arc**: center `(8.0, 9.0)`, radius `5.50 pt`, angle `42°` to `318°` (`276°` span), `lineWidth = 1.75 pt`, `lineCapStyle = .round`, `alpha = 1.0`.
  2. **Focal Core Node**: center `(8.0, 9.0)`, radius `1.65 pt` (oval `3.3 × 3.3 pt`), solid fill (`alpha = 1.0` when `active`, `0.40` when `idle` or `warning`).
  3. **Outbound Signal Wave**: center `(8.0, 9.0)`, radius `7.75 pt`, angle `-28°` to `+28°` (`56°` span), `lineWidth = 1.65 pt`, `lineCapStyle = .round` (`alpha = 1.0` when `active`, `0.40` when `idle`).
  4. **Warning Cutout Badge (State `.warning` only)**: at `(14.2, 4.0)`, punch a `r = 3.6 pt` transparent cutout (`.destinationOut`) and fill a `r = 2.4 pt` solid warning dot (`.sourceOver`).
- **Button Count Formatting (`NSStatusBarButton`)**:
  - When `uniqueSessionCount == 0`: `button.title = ""`, `button.imagePosition = .imageOnly`.
  - When `uniqueSessionCount > 0`: `button.imagePosition = .imageLeading`, `button.attributedTitle` = `" \(uniqueSessionCount)"` with `NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)`.

### 3.2 Control-Center Custom-View Menu Panel (`width: 340 pt`)
Built using custom `NSView` items hosted in `NSMenuItem.view` inside `NSMenu` (inheriting native macOS `.menu` blur, rounded corners, and shadow):

```text
┌──────────────────────────────────────────────────────────┐
│  Cito  1 listening                        [ ● default · alice ] │
├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─┤
│  work (Auth required)         [Sign In] [Reconnect] [Disconnect] │
├──────────────────────────────────────────────────────────┤
│ ┌──────────────────────────────────────────────────────┐ │
│ │ ┌──────┐  agent  [Pi]            [🔒 Self] [↗] [✕]   │ │
│ │ │ >_   │  dotfiles#f8k2 ⧉ · ~/dotfiles/agent         │ │
│ │ └──────┘                                             │ │
│ └──────────────────────────────────────────────────────┘ │
├──────────────────────────────────────────────────────────┤
│  ⏹ Stop All                                    Quit Cito ⌘Q │
└──────────────────────────────────────────────────────────┘
```

1. **Header Bar (`340 × 40 pt`)**:
   - Left: **`Cito`** (`13pt .bold`) + subtitle (`"\(count) listening"` when `count > 0`, or `"Idle"` when `0`, `11.5pt .regular`, `.secondaryLabelColor`).
   - Right: Interactive Hub status pills (`height: 22pt, cornerRadius: 11pt`), each with a `6×6pt` semantic dot (`connected` $\to$ `.systemGreen`, `connecting` $\to$ `.systemOrange`, `auth_required` $\to$ `.systemRed`, `disconnected` $\to$ `.systemGray`) and label (`default · <user>` in single-default mode, or `<hub.name>` in multi-hub mode).
   - **Expandable Hub Action Drawer (`340 × 32 pt`)**: Hidden when all hubs are `connected` unless the user clicks a hub pill; automatically expanded for any hub in `auth_required`, `disconnected`, or `error != nil`. Displays hub status/error on the left and compact action buttons on the right: **`Sign In`** (for `auth_mode == "oidc"`, emits `login`), **`Reconnect`** (emits `reconnect`), **`Disconnect`** (emits `disconnect`).
2. **Active Session Cards (`320 × 58 pt` per listener, `cornerRadius: 10 pt`, `x: 10`)**:
   - **Left Harness Badge (`32 × 32 pt, cornerRadius: 8 pt`)**:
     - `"pi"` $\to$ SF Symbol `"terminal.fill"` (indigo tint);
     - `"codex"` $\to$ SF Symbol `"sparkles"` (cyan/teal tint);
     - `"claude"` $\to$ SF Symbol `"cpu.fill"` (orange tint);
     - Other $\to$ SF Symbol `"bolt.horizontal.fill"` (secondary tint).
   - **Center Row 1**: Bold `listener.title` (`13pt .semibold`) + Harness capsule pill (`"Pi"`, `"Codex"`, etc., `10pt .semibold`) + optional `"· \(listener.hub)"` when multiple hubs exist.
   - **Center Row 2**:
     - **Click-to-Copy Short ID Pill**: Formats `<slug>-<fp12>` as `<slug>#<fp4> ⧉` (`11pt` monospaced, full `listener.scoped_id` in `toolTip`). Clicking copies `listener.scoped_id` to `NSPasteboard.general` and temporarily changes the pill text to green **`✓ Copied`** for 1.2s without closing the menu.
     - **Directory Path**: `listener.cwd` with `$HOME` abbreviated to `~` (`11pt`, `.tertiaryLabelColor`, `.byTruncatingMiddle`).
   - **Right Inline Action Buttons (`height: 24 pt`)**:
     1. **Scope Toggle Button (`width: 60 pt, cornerRadius: 6 pt`)**: Displays **`🔒 Self`** (`scope == "self"`, neutral background) or **`👥 Team`** (`scope == "workspace"`, `.systemBlue` tinted background). Clicking emits `set_scope` without closing the menu.
     2. **Open Button (`24 × 24 pt, cornerRadius: 6 pt`, shown when `listener.can_jump`)**: SF Symbol `"arrow.up.forward.app"` (tooltip `"Open in Codex"`). Clicking calls `openCodex(listener.session_id)` and dismisses the menu (`menu.cancelTracking()`).
     3. **Stop Button (`24 × 24 pt, cornerRadius: 6 pt`)**: SF Symbol `"xmark"` (tooltip `"Stop listening"`, turns `.systemRed` on hover). Clicking emits `unlisten`.
   - **Empty State Card (`320 × 64 pt`, shown when `listeners.isEmpty`)**: SF Symbol `"waveform.slash"` + **`No active listeners`** + **`Call cito_listen in a session to receive notifications`**.
3. **Footer Bar (`340 × 32 pt`)**:
   - Left (shown when `!listeners.isEmpty`): **`⏹ Stop All`** (emits `unlisten_all`).
   - Right: **`Quit Cito ⌘Q`** (emits `quit`).
4. **Test Action Indexing Compatibility (`menuActions`)**:
   - For each listener: (if `can_jump`) `openCodex`, then `copyId`, then `set_scope`, then `unlisten` (registered twice to preserve index parity with previous submenu + option-alternate slots).
   - If `!listeners.isEmpty`: `unlisten_all`.
   - For each hub: (if `oidc`) `login`, then `reconnect`, then `disconnect`.
   - Finally: `quit`.

### 3.3 English In-Chat Wake Card & HUD Banner Text
- **In-Chat Wake Card (`src/adapters/wake-card.ts`)**:
  - Single signal: `🔔 Cito · <sender>\n\n<message>`
  - Multi-signal batch: `🔔 Cito · ${signals.length} notifications${targetTitle}\n\n[1] <sender>\n<message>...`
- **Top-Right HUD Banner (`src/macos/cito-ui.swift`)**:
  - Top-right pill (`semantic_label`): `"Notification"` (when `effective_wake == true`) or `"Passive"` (when `effective_wake == false`), appending `" · ×\(displayedCount)"` when `displayedCount > 1`.
  - Subtitle prefix when `effective_wake == false`: `"[Passive] "`.
  - Footer (`action_label`): `"Open in Codex ↗"` when `target_harness == "codex"`, else `"\(target_harness) session · Click to dismiss"`.

---

## 4. Optional `CITO_CLAUDE_CHANNEL=1` Stdio Channel Bridge (`src/mcp/cli.ts`, `src/mcp/server.ts`)

- When `env.CITO_CLAUDE_CHANNEL === "1"` (or `config.enableClaudeChannel === true`, default `false`):
  - When `cito-mcp` runs in stdio mode (acting as the local stdio forwarder to `cito-daemon`), it advertises `capabilities: { tools: {}, experimental: { "claude/channel": {} } }` on its stdio MCP server and binds `${stateDir}/sockets/claude-pid-${process.ppid}.sock` (`0600`).
  - The socket responds to `{ op: "identify" }` with `{ ok: true, endpoint_pid: process.ppid, session_id: process.env.CLAUDE_SESSION_ID || "claude-" + process.ppid, session_name: basename(process.cwd()), cwd: process.cwd() }`, and on `{ op: "deliver", session_id, batch_id, content }` it deduplicates `batch_id`, emits `{ jsonrpc: "2.0", method: "notifications/claude/channel", params: { content, meta: { origin: "cito" } } }` on its stdio transport, and replies `{ ok: true, delivered: true, deduplicated }`.
  - `SocketBridgeAdapter` for `"claude"` uses `confirmDelivery: false` (`deliver` returns `"delivered_unconfirmed"`), so the signal is marked `'DISPATCHED'` while `acknowledged_at_ms` stays `NULL` for `cito_inbox()` fallback draining if the client did not enable `--channels`.

---

## 5. Proposed Design for Claude Code Integration (Design Suggestion Only — Not Yet Implemented)

> **Status Notice (Proposed Design Suggestion Only — Not Yet Implemented)**: This section records the architectural design suggestion for full native Claude Code integration. The opt-in `CITO_CLAUDE_CHANNEL=1` stdio forwarding bridge provides experimental channel push, while native Claude Code lifecycle hooks and session-discovery integration remain a design proposal for future implementation.

1. **Session Identity & Transport Topology**:
   - Claude Code sessions can connect either directly to `cito-daemon`'s stateless Streamable HTTP endpoint (`http://127.0.0.1:4290/mcp`) for pull-based `cito_inbox()` workflows, or through the local stdio forwarder with experimental channel support (`--channels`) so each terminal session binds its parent PID and working directory to a dedicated local bridge endpoint.
2. **Dual-Mode Notification Delivery (Channel Push + Turn-Start Inbox Drain)**:
   - **Active Push (MCP Channel Notification)**: When launched with channel notifications enabled, incoming wake batches are pushed over the session's stdio MCP stream via `notifications/claude/channel` to wake an idle Claude Code conversation.
   - **Turn-Start Pull Fallback (Inbox Drain)**: Because channel push requires explicit client flags and may be ignored while a turn is busy, deliveries are recorded as unconfirmed (`delivered_unconfirmed`) so `cito_inbox()` (or a `UserPromptSubmit` hook) can reliably drain and acknowledge unread notifications at the start of the next turn.
