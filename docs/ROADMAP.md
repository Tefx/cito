# Cito Product & Architecture Roadmap (Towards v1.0 Standalone Application & Turnkey Hub)

This document defines the staged evolution of Cito from the `v0.11.3` developer CLI/MCP package (`@tefx/cito`) to a self-contained macOS application (`Cito.app`), an extensible out-of-process adapter ecosystem, and a one-command self-hosted hub appliance.

---

## Guiding Architectural Principles

1. **Fixed Built-in Harnesses with Explicit Stability Tiers**:
   - The core repository ships and maintains exactly three built-in harness integrations:
     - **Pi Coding Agent (`pi`)** — `tier: "stable"`
     - **ChatGPT / Codex Desktop (`codex`)** — `tier: "stable"`
     - **Claude Code (`claude`)** — `tier: "experimental"` (explicitly opt-in; never enabled silently)
   - All additional coding agents, IDEs, or custom workflows are supported through **out-of-tree, out-of-process user adapters** without adding harness-specific code to the Cito repository.
2. **Process-Boundary Fault Isolation for User Extensions**:
   - User-authored adapters run out-of-process (via the Unix domain socket bridge protocol or bounded subprocess commands declared in `~/.cito/adapters/<id>.json`), never via in-process `import()` inside `cito-daemon`. A hanging, crashing, or malformed user adapter cannot stall the daemon event loop, corrupt SQLite state, or degrade built-in adapters.
3. **Single Setup & Configuration Engine**:
   - CLI onboarding (`cito setup`) and the native macOS Settings window share a single idempotent, backup-safe configuration backend for `~/.cito/config.json` and host harness config files (`~/.codex/config.toml`, `~/.pi/agent/`, `~/.claude/settings.json`).
4. **Client / Server Separation**:
   - `Cito.app` is a pure client and local control plane; self-hosted hubs remain server/container deployments (`deploy/nats/`).

---

## Stage Overview

| Stage | Focus | Primary Deliverable |
| --- | --- | --- |
| **Stage 0** | **Out-of-Tree User Adapter Boundary & Stability Tiers** | Declarative out-of-process adapter manifests (`~/.cito/adapters/<id>.json`), built-in `stable` vs. `experimental` tier metadata, generic `--harness <id>` CLI, and dynamic adapter metadata in `cito-ui`. |
| **Stage 1** | **Turnkey Self-Hosted Hub & `cito setup` Onboarding Engine** | One-command NATS 2.14 + `cito-auth-callout` container bootstrap (auto-persisted NKeys, readiness healthcheck, join descriptor output) and `cito setup` CLI. |
| **Stage 2** | **Standalone macOS Application (`Cito.app`) & Visual Settings UI** | Self-contained `/Applications/Cito.app` bundle, native Settings & Integrations window, `SMAppService` Login Item toggle, and confirmed `cito://join` URL handler. |
| **Stage 3** | **Project Website & Static `/join` Trampoline** | Official landing page, visual architecture/UI showcase, custom adapter authoring guide, and client-side `/join#...` deep-link trampoline. |
| **Stage 4** | **Notification Activity Feed, Human Quick-Reply & Public Notarization** | Recent signal history + human-to-agent quick reply in `DirectoryPanel`, Apple Developer ID notarization, and automatic update checks. |

---

## Stage 0: Out-of-Tree User Adapter Boundary & Built-in Stability Tiers

### Objectives
- Freeze repository-maintained adapters to `pi` (`stable`), `codex` (`stable`), and `claude` (`experimental`).
- Allow users to integrate any external agent or IDE out-of-tree (`~/.cito/adapters/`) with first-class CLI and macOS UI support (custom display name, SF Symbol icon, stability badge, and optional jump URL) without editing or recompiling Cito.

### Core Approach
1. **Built-in Stability Tiers & Registry Protection**:
   - Extend adapter metadata with `tier: "stable" | "experimental" | "custom"`.
   - Lock built-in IDs (`pi`, `codex`, `claude`) against external override.
   - Surface `[Experimental]` in UI/documentation for `claude` and require explicit opt-in before enabling experimental host hooks.
2. **Two Out-of-Process User Adapter Pathways**:
   - **Path A — Live Socket Bridge (`${CITO_STATE_DIR}/sockets/<id>-pid-<pid>.sock`)**:
     - Session-scoped companions speak the existing newline-delimited JSON protocol (`identify`, `deliver`, `status`).
   - **Path B — Declarative Subprocess Adapter Manifest (`${CITO_STATE_DIR}/adapters/<id>.json`)**:
     - For harnesses that manage sessions externally (e.g., via local DB, CLI, or URL scheme), a JSON manifest declares `id`, `displayName`, `symbol`, `tint`, optional `jumpUrlTemplate`, and bounded subprocess commands for `discover`, `probe`, `deliver`, and `badge`.
     - `cito-daemon` executes manifest commands with strict per-call timeouts, output-size caps, and `batch_id` deduplication semantics.
3. **Generic CLI & Dynamic UI Metadata**:
   - Add `cito listen --harness <id> [--session <session_id>]` and `cito unlisten --harness <id> [--session <session_id>]` (retaining `--codex` and `--claude` as shorthands).
   - Include adapter presentation metadata (`id`, `display_name`, `symbol`, `tier`, `can_jump`) in `DaemonToUiMessage` so `cito-ui.swift` renders badges, group headers, Directory facet pills, and jump actions dynamically without hardcoded `switch harness` branches.

### Exit Criteria
- A user-defined external adapter placed under `${CITO_STATE_DIR}/adapters/` appears in `cito-ui` and `cito listen --harness <id>`, delivers wake cards, and survives simulated child hang/crash/malformed-output faults without affecting `pi`, `codex`, or `claude`.

---

## Stage 1: Turnkey Self-Hosted Hub & `cito setup` Onboarding Engine

### Objectives
- Reduce self-hosted OIDC hub deployment and client harness onboarding to single, idempotent commands.

### Core Approach
1. **Turnkey Hub Bootstrap (`deploy/nats/`)**:
   - Align Compose and container images to `nats:2.14`.
   - Provide an idempotent bootstrap entrypoint that generates and persists `SA...` Account and `SX...` XKey seeds on first volume initialization, configures `max_control_line: 65536` and JetStream storage bounds, verifies end-to-end `cito-auth-callout` readiness, and prints a non-secret hub join descriptor in container startup logs.
   - Support optional OIDC email-domain / role allowlists in `cito-auth-callout`.
2. **`cito setup` CLI Configuration Engine**:
   - Implement `cito setup` (including `cito setup --join <descriptor-or-url>`) to validate and write `~/.cito/config.json` and optionally configure selected host integrations (`pi`, `codex`, and opt-in `claude`), preserving unrelated user settings and creating timestamped backups before modifying existing files.

### Exit Criteria
- Fresh hub bootstrap and container restart with the same volume succeed without manual NKeys generation; `cito setup` configures a clean client profile idempotently and rolls back safely on malformed input.

---

## Stage 2: Standalone macOS Application (`Cito.app`) & Visual Settings UI

### Objectives
- Package Cito as a self-contained `/Applications/Cito.app` bundle with a native Settings and Onboarding window.

### Core Approach
1. **Native Settings & Integrations Window (`cito-ui.swift`)**:
   - Visual Hub manager (add, edit, remove, test connection), 1-click harness integration toggles backed by Stage 1's `cito setup` engine, and a `Launch at Login` toggle using `SMAppService.mainApp`.
2. **Confirmed `cito://join` Deep-Link Handler**:
   - Register the `cito://` URL scheme in `Info.plist`; clicking a join link presents a confirmation dialog showing the hub name, broker URL, and OIDC issuer before saving and initiating OIDC sign-in.
3. **Self-Contained `.app` Bundle & Daemon Handover**:
   - Bundle the runtime and CLI executables inside `Cito.app`, define deterministic version handover between `.app` and CLI installations via `/health`, and package `Cito-darwin-arm64.tar.gz` with ad-hoc signing for internal testing.

---

## Stage 3: Project Website & Static `/join` Trampoline

### Objectives
- Publish the official documentation and download site once the desktop UI and 1-command hub deployment are settled.

### Core Approach
- Landing page with macOS download + 1-line install, multi-harness wake architecture visuals, out-of-tree adapter authoring guide, 1-command hub deployment guide, and a static client-side `/join#...` trampoline page that renders hub details and opens `cito://join?...`.

---

## Stage 4: Notification Activity Feed, Human Quick-Reply & Public Notarization

### Objectives
- Add missed-notification review and human-to-agent messaging in the UI, and complete Apple Developer ID notarization for public release.

### Core Approach
- Add an `Activity` view and inline quick-reply composer in `DirectoryPanel` backed by `local_signals` and `cito_send`, sign `Cito.app` with a Developer ID Application certificate, notarize via `xcrun notarytool`, and add update checks.
