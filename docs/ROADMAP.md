# Cito Product & Architecture Roadmap (Towards v1.0 Standalone Application & Turnkey Hub)

This document defines the staged evolution of Cito from a developer CLI/MCP package (`@tefx/cito`) to a harness-agnostic plugin architecture, a one-command self-hosted hub appliance, and a self-contained macOS application (`Cito.app`).

---

## Guiding Architectural Principles

1. **Harness-Agnostic Core & First-Class Bundled Plugins (Eating Our Own Dogfood)**:
   - Core (`src/shared/`, `src/mcp/`, `src/ui/`, `src/macos/cito-ui.swift`, and the Plugin Host SDK in `src/adapters/`) has **zero** hardcoded knowledge of specific harnesses.
   - The repository ships and maintains exactly three built-in plugins under `plugins/` inside the single `@tefx/cito` package, using the **exact same Plugin Host SDK (`@tefx/cito/plugin-sdk`) and `plugin.json` + `index.ts` contract** as external user plugins:
     - **Pi Coding Agent (`plugins/pi`)** — `tier: "stable"`
     - **ChatGPT / Codex Desktop (`plugins/codex`)** — `tier: "stable"`
     - **Claude Code (`plugins/claude`)** — `tier: "experimental"` (explicitly opt-in; never enabled silently)
   - All additional coding agents, IDEs, or custom workflows live out-of-tree in `${CITO_STATE_DIR}/plugins/<id>/` (default `~/.cito/plugins/<id>/`) without modifying or recompiling Cito.
2. **One Unified Plugin Format (`plugin.json` + Optional `index.ts` + Optional Host-Side Script)**:
   - Every plugin is a directory `<id>/` containing a flat `plugin.json` (`id`, `displayName`, optional `tier`, `sfSymbol`, `tint`, `jumpUrlTemplate`, `cliFlag`) and an optional `index.ts` (or `index.js`) default-exporting `PluginFactory` (`HarnessAdapter` or `(manifest) => HarnessAdapter`) from `@tefx/cito/plugin-sdk`.
   - Depending on whether the target harness can be driven externally:
     - **External-Driven Harnesses (e.g. `plugins/codex`)**: Only need `plugin.json` + `index.ts` (managed 100% by `cito-daemon`).
     - **Host-Assisted Harnesses (e.g. `plugins/pi`, `plugins/claude`)**: Include a host-side bridge/hook script (`cito-bridge.ts`, `channel.ts` / `hook-cli.ts`) managed by the target harness, communicating with `index.ts` via the SDK's `SocketBridgeAdapter` or wake queue.
3. **Fault Isolation & Deterministic Conflict Resolution**:
   - Built-in plugin IDs (`pi`, `codex`, `claude`) and core CLI flag names (`hub`, `team`, `scope`, `tag`, `harness`, `session`, `help`, `version`, `setup`) are reserved.
   - Every plugin discovery, caller-detection, probe, and delivery call is isolated with bounded timeouts (`1500ms` discovery deadline) so a hanging or failing user plugin in `~/.cito/plugins/` never stalls `cito-daemon` or built-in plugins.
4. **Single Setup Engine & Client/Server Separation**:
   - CLI onboarding (`cito setup`) and the native macOS Settings window share a single idempotent, backup-safe configuration engine (`src/setup/`).
   - `Cito.app` is a pure client and local control plane; self-hosted hubs remain server/container deployments (`deploy/nats/`).

---

## Stage Overview

| Stage | Focus | Primary Deliverable |
| --- | --- | --- |
| **Stage 0 (`v0.12.0`)** | **Unified Plugin Architecture, Built-in Extraction (`plugins/*`) & Data-Driven UI** | Zero-harness-coupling Core & `cito-ui.swift`, `plugins/{pi,codex,claude}/`, `~/.cito/plugins/<id>/` loader, flat `plugin.json` + `index.ts` standard, and `--harness <id> [--session <sid>]` CLI. |
| **Stage 1 (`v0.13.0`)** | **Turnkey Self-Hosted Hub & `cito setup` Onboarding Engine** | One-command NATS 2.14 + `cito-auth-callout` Compose bootstrap (one-shot `cito-hub-init` key persistence, end-to-end callout readiness check, join descriptor output) and `cito setup` CLI. |
| **Stage 2 (`v0.14.0`)** | **Standalone macOS Application (`Cito.app`) & Visual Settings UI** | Self-contained `/Applications/Cito.app` bundle (`Cito-darwin-arm64.tar.gz`), native Settings & Integrations window, `SMAppService` Login Item toggle, and confirmed `cito://join` URL handler. |
| **Stage 3** | **Project Website & Static `/join` Trampoline** | Official landing page, visual architecture/UI showcase, custom plugin authoring guide, and client-side `/join#...` deep-link trampoline. |
| **Stage 4 (`v1.0.0`)** | **Notification Activity Feed, Human Quick-Reply & Public Notarization** | Recent signal history + human-to-agent quick reply in `DirectoryPanel`, Apple Developer ID notarization, and automatic update checks. |

---

## Stage 0 (`v0.12.0`): Unified Plugin Architecture, Built-in Plugin Extraction & Data-Driven UI

### 1. Flat `plugin.json` Manifest Contract
```json
{
  "id": "codex",
  "displayName": "Codex",
  "tier": "stable",
  "sfSymbol": "sparkles",
  "tint": "teal",
  "jumpUrlTemplate": "codex://threads/{session_id}",
  "cliFlag": true
}
```
- **Required fields (2)**:
  - `id`: Lowercase plugin identifier matching the directory name (`^[a-z][a-z0-9_-]{0,31}$`).
  - `displayName`: Human-readable name displayed in UI badges, group headers, facet pills, and `"Open in <displayName>"` jump actions.
- **Optional fields (5)**:
  - `tier`: `"stable" | "experimental" | "custom"` (defaults to `"custom"`; external user plugins are always forced to `"custom"`).
  - `sfSymbol`: Apple SF Symbol icon name (defaults to `"bolt.horizontal.fill"`).
  - `tint`: Semantic color token (`"indigo" | "teal" | "orange" | "blue" | "purple" | "green" | "pink" | "gray"`, defaults to `"blue"`).
  - `jumpUrlTemplate`: Deep-link URL template with `{session_id}` / `{cwd}` placeholders (validated against safe non-`javascript:`/`file:`/`data:` schemes).
  - `cliFlag`: Boolean (defaults to `false`); when `true` and `id` is not in `RESERVED_CLI_FLAGS`, registers `--<id>[=<session_id>]` on `cito listen` / `cito unlisten`.
- **Automatically derived (no manifest fields needed)**:
  - Group count noun (`"open"` when candidates report a live `endpoint_pid`, `"recent"` when `jumpUrlTemplate` is present, and `"available"` otherwise).
  - Jump action label (`"Open in \(displayName)"` on cards, `"Open in \(displayName) ↗"` on HUD banners).
  - Group & facet sort order (`pi` $\to$ `codex` $\to$ `claude` $\to$ custom plugins alphabetically).

### 2. Conflict-Resolution & Fault-Isolation Rules
1. **Plugin ID Protection**: Built-in plugins (`pi`, `codex`, `claude`) load first and lock their IDs. Any external plugin under `~/.cito/plugins/<dir>/` whose `id` collides with a built-in ID, mismatches `<dir>`, or duplicates another plugin is skipped with a diagnostic warning.
2. **CLI Flag Protection**: Universal CLI selection is `cito listen --harness <id> [--session <sid>]`. Shorthand `--<id>` is rejected if `<id>` collides with `RESERVED_CLI_FLAGS`.
3. **Caller Binding Arbitration**:
   - Bottom-up parent-PID walk (`depth = 0..5`) binds to the **closest ancestor socket** (e.g., Pi running inside a Cursor terminal binds to Pi at depth 1, not Cursor at depth 3).
   - Simultaneous explicit claims from multiple plugins (or an explicit claim combined with `endpoint_pid`) fail with `INVALID_INPUT: Conflicting caller metadata`.
4. **Stdio Channel & Discovery Isolation**: At most one plugin may activate a `startStdioChannel` wrapper per `cito-mcp` process; each plugin's `discoverAvailableSessions` runs under an independent `1500ms` timeout.

---

## Stage 1 (`v0.13.0`): Turnkey Self-Hosted Hub & `cito setup` Onboarding Engine
1. **Compose Hub with One-Shot Bootstrap (`deploy/nats/`)**: Align to `nats:2.14-alpine`, add `cito-hub-init` to persist `SA...`/`SX...` NKeys and `max_control_line: 65536` on `/data`, gate readiness on end-to-end `cito-auth-callout` verification, and print the non-secret join descriptor.
2. **Shared `src/setup/index.ts` Engine**: Route `cito setup` before `loadConfig()`, providing `inspectSetup`, `previewSetup`, and `applySetup` with timestamped backups, atomic file replacement, and comment-preserving append-only TOML updates.

## Stage 2 (`v0.14.0`): Standalone macOS Application (`Cito.app`) & Visual Settings UI
- Native Settings window in `cito-ui.swift` (backed by `src/setup/`), `SMAppService.mainApp` Login Item toggle, confirmed `cito://join` URL handler, and self-contained `/Applications/Cito.app` bundle (`Cito-darwin-arm64.tar.gz`).

## Stage 3: Project Website & Static `/join` Trampoline
- Official landing page, visual UI/architecture showcase, plugin SDK guide, and static client-side `/join#...` trampoline page.

## Stage 4 (`v1.0.0`): Notification Activity Feed, Human Quick-Reply & Public Notarization
- Recent notification history + inline quick-reply composer in `DirectoryPanel`, Apple Developer ID notarization, and automatic update checks.
