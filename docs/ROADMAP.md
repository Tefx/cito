# Cito v1.0 Product & Architecture Roadmap

Cito's evolution from `v0.11.3` to `v1.0.0` is organized into **five stages (`Stage 0` through `Stage 4`)**:
- **Stage 0 (`v0.12.0` — Completed)**: Unified Plugin Architecture, Built-in Plugin Extraction (`plugins/{pi,codex,claude}`) & Data-Driven macOS UI
- **Stage 1 (`v0.13.0` — Completed)**: NATS-Free Native `HubEngine` (`local://` In-Process + `cito hub` HTTP/SSE Server), Control-Plane Guidance Without Hard Message Caps & `cito setup` CLI Onboarding
- **Stage 2 (`v0.14.0`)**: Rust Hot-Path Multi-Call Binary (`cito-native`), Surface 3 AppKit `Settings…` (`⌘,`) Window, In-Process LAN Hub Sharing (`:4291`) & `< 5 MB` `Cito.app` Packaging
- **Stage 3 (`v0.15.0`)**: Self-Diagnostics (`cito doctor`), Auto-Healing & End-to-End Verification (`cito ping`)
- **Stage 4 (`v1.0.0`)**: Notification Activity Feed, Human Quick-Reply & Production Hardening

---

## Architectural Guardrails (Applicable to All Stages)

1. **Harness-Agnostic Core (`src/` & `src/macos/cito-ui.swift`)**:
   - Core modules (`src/shared/`, `src/hub/`, `src/mcp/`, `src/setup/`, `src/ui/`, `src/macos/cito-ui.swift`) and the Plugin Host SDK (`src/adapters/`) MUST NOT contain hardcoded branches, configuration fields, CLI flag names, or visual tables for specific agent harnesses (`pi`, `codex`, `claude`).
   - All harness-specific discovery, caller detection, delivery, stdio channel wrapping, CLI flags, and UI presentation metadata live inside plugin directories (`plugins/pi`, `plugins/codex`, `plugins/claude`, or user-installed `${CITO_STATE_DIR}/plugins/<id>/`).
2. **One Unified Plugin Format (`plugin.json` + Optional `index.ts` + Optional Host-Side Script)**:
   - Every plugin is a directory `<id>/` containing a flat `plugin.json` (`id`, `displayName`, optional `tier`, `sfSymbol`, `tint`, `jumpUrlTemplate`, `cliFlag`) and an optional `index.ts` (or `index.js`) default-exporting `PluginFactory` (`HarnessAdapter` or `(manifest) => HarnessAdapter`) from `@tefx/cito/plugin-sdk`.
   - Built-in plugins (`plugins/pi` [Stable], `plugins/codex` [Stable], `plugins/claude` [Experimental]) and external user plugins (`${CITO_STATE_DIR}/plugins/<id>/`, defaulting to `~/.cito/plugins/<id>/`) share the exact same format and loader contract.
3. **Mechanism, Not Policy (Control-Plane Recommendation Without Hard Message Caps)**:
   - MCP tool descriptions (`cito_send`, `cito_listen`) guide agents to use Cito primarily as a lightweight control plane (waking sessions, status handoffs, and coordinating work) while pairing with dedicated channels (shared files, Git branches, issue trackers, or discussion tables) for extended multi-turn discussions or large artifacts.
   - However, Cito enforces **no hard 500-character policy cap** on `cito_send.message` (only a `65,536`-codepoint transport safety bound): when short context suffices or other channels are unavailable or have excessive overhead, users and agents may transmit full messages directly without artificial rejection.
4. **Unified Single-Engine Hub (`HubEngine` — Zero NATS Dependency)**:
   - Both single-Mac standalone mode (`url: "local://"`, running in-process inside `cito-daemon`) and remote shared hubs (`cito hub`, running over HTTP/SSE + SQLite WAL) execute the **exact same `HubEngine` (`src/hub/engine.ts`)**.
   - All 5 NATS packages (`nats`, `nats.ws`, `@nats-io/jwt`, `nkeys.js`, `@eplightning/nats-server`) and the `cito-auth-callout` sidecar are removed. OIDC authentication (`auth_mode: "oidc"`) and pre-shared token authentication (`auth_mode: "token"`) are verified directly in `cito hub` HTTP/SSE handlers with Row-Level Security (`scope: "self"` sessions never leave the server for other principals).

---

## Stage 0 (`v0.12.0` — Completed): Unified Plugin Architecture, Built-in Plugin Extraction (`plugins/{pi,codex,claude}`) & Data-Driven macOS UI

Completed in Phase `v16` (`1f46ab2`, tag `v0.12.0`).

---

## Stage 1 (`v0.13.0`): NATS-Free Native `HubEngine` (`local://` + `cito hub`), Control-Plane Guidance & `cito setup` CLI

### 1.1 Orthogonal Hub Topology, Naming & Scope Rules
1. **Orthogonal Dimensions (Topology × Auth Mode × User Alias)**:
   - **Topology**:
     - **In-Process Local (`url: "local://"`)**: Runs `HubEngine` directly inside `cito-daemon` (`0` child processes, `0` extra ports, `0` external binaries). When `~/.cito/config.json` is absent and no `CITO_HUB_URL` / `CITO_NATS_URL` env override is set, Cito defaults to a single built-in hub named **`"local"`** (`url: "local://"`, `authMode: "none"`, `enabled: true`).
     - **Remote HTTP/SSE (`url: "http://..." | "https://..."`)**: Connects to a `cito hub` instance over standard HTTP + Server-Sent Events (`GET /v1/events`). Legacy `nats://` / `ws://` / `wss://` URLs and `CITO_NATS_URL` are transparently normalized to `http://` / `https://` for configuration and test compatibility.
   - **Authentication Mode (`authMode: "none" | "token" | "oidc"`)**:
     - `"none"`: Zero-ceremony trusted network mode (for `local://`, `127.0.0.1`, or Tailscale `100.64.0.0/10` / `*.ts.net` where WireGuard provides L3 device authentication and encryption).
     - `"token"`: Pre-shared bearer token (`Authorization: Bearer <token>`) for non-Tailscale LAN or simple self-hosted servers without an OIDC provider.
     - `"oidc"`: Multi-user OIDC authentication (`Authorization: Bearer <id_token>`) verified directly by `OidcVerifier` in `cito hub` with browser PKCE sign-in.
   - **Per-Hub `enabled?: boolean` (Default `true`)**:
     - Any hub in `config.json` can be toggled on/off via `"enabled": true | false` (or `cito setup --enable-hub <name>` / `--disable-hub <name>`) without deleting its configuration. Disabled hubs are omitted from `CitoService.slots` and UI state frames.
2. **Scope Optimization & Cross-Hub Privacy Safety**:
   - On `local://` (or when `hub.defaultScope === "workspace"`), `cito_listen` defaults `scope` to `"workspace"`.
   - In `cito-ui.swift`, when a session's hub has `auth_mode != "oidc"` and `name == "local"`, the `62pt` `🔒 Self / 👥 Team` toggle pill is omitted from the card, reclaiming horizontal space for the session title.
   - When migrating a session via `switch_hub` (`from_hub` $\to$ `to_hub`), if `to_hub` has `authMode === "oidc"` and `from_hub` has `authMode !== "oidc"`, `scope` resets to `"self"` (fail-closed privacy protection so moving a session from `local`/`tailnet` to a team OIDC hub never accidentally exposes it to the whole organization).

### 1.2 Unified `HubEngine` & `cito hub` Server (`src/hub/`, `deploy/hub/`)
1. **Single Authority `HubEngine` (`src/hub/engine.ts`)**:
   - Uses `node:sqlite` (`DatabaseSync` in WAL mode when persisted to `${dataDir}/hub.sqlite`, or `:memory:`) + in-memory `EventEmitter` dispatch.
   - Enforces **Server-Side Row-Level Security (RLS)** when `authMode === "oidc"`:
     - Queries (`GET /v1/kv`, `GET /v1/kv/:key`, SSE watch streams) filter out any registration where `scope === "self"` and `principal_id !== caller.principal_id`.
     - Mutations (`PUT /v1/kv/:key`) reject attempts to create or update a registration whose `principal_id !== caller.principal_id` or where the existing record belongs to a different `principal_id` (`403 PERMISSIONS_VIOLATION`).
     - Signal publishing (`POST /v1/signals/:targetId`) verifies `signal.from.principal_id === caller.principal_id` on OIDC hubs.
   - Built-in structured audit hook (`onAudit?: (event: HubAuditEvent) => void` and optional `CITO_HUB_AUDIT_LOG` JSONL file append) recording verified `principal_id`, `action` (`register | unlisten | send | ack`), `listener_id`, `signal_id`, and `timestamp`.
2. **`cito hub` Subcommand & Single-Container Deployment (`deploy/hub/`)**:
   - `cito hub [--port 4291] [--bind 127.0.0.1|0.0.0.0] [--auth none|token|oidc] [--token <token>] [--issuer <url> --client-id <id>] [--data-dir <dir>]` starts the standalone HTTP/SSE Hub server.
   - Prints the ready-to-copy client onboarding command on startup:
     - `auth=none`: `cito setup --hub tailnet --url http://<host>:4291`
     - `auth=token`: `cito setup --hub tailnet --url http://<host>:4291 --auth token --token <token>`
     - `auth=oidc`: `cito setup --hub team --url http://<host>:4291 --auth oidc --issuer <issuer> --client-id <id>`
   - `deploy/hub/docker-compose.yml` + `deploy/hub/Dockerfile` replaces the multi-container `deploy/nats/` stack with a single `cito-hub` service.

### 1.3 `cito setup` Pure CLI Configuration Engine (`src/setup/index.ts`)
- Intercepted in `src/mcp/cli.ts` **before** `loadConfig()` so `cito setup` can create or repair `~/.cito/config.json`.
- Exports pure programmatic functions `inspectSetup`, `previewSetup`, and `applySetup` (reused by Stage 2's native macOS Settings window).
- Supports:
  - `cito setup --status`: inspects `~/.cito/config.json` and detected harnesses (`pi`, `codex`, `claude`).
  - `cito setup [--dry-run] [--hub <name> --url <url> [--auth none|token|oidc] [--token <token>] [--issuer <url> --client-id <id>] [--default]] [--enable-hub <name>] [--disable-hub <name>] [--enable <harness>]`:
    1. Atomically updates `~/.cito/config.json` (`0600`, preserving existing hubs and `plugins` settings; when adding the first remote hub and `local` was not explicitly requested, sets `default_hub` to the new hub so single-hub UI mode is preserved unless multiple hubs are enabled).
    2. Configures **Stable** installed harnesses by default (`codex` and `pi`), and configures **Experimental** harnesses (`claude`) only when `--enable claude` is explicitly passed:
       - **Codex (`~/.codex/config.toml`)**: Pre-validates TOML syntax; if `[mcp_servers.cito]` is absent, appends the block at EOF (preserving 100% of user comments and formatting); if already pointing to `cito-mcp`, no-ops; if conflicting, reports an error unless `--force` is passed.
       - **Pi (`~/.pi/agent/settings.json` + `~/.pi/agent/mcp/global.mcp.json`)**: Idempotently adds `"npm:@tefx/cito"` to `packages` in `~/.pi/agent/settings.json` and merges `mcpServers.cito` (`{ "command": "cito-mcp", "args": [] }`) in `~/.pi/agent/mcp/global.mcp.json`.
       - **Claude Code (`~/.claude/settings.json`, only with `--enable claude`)**: Merges `mcpServers.cito` (with `CITO_CLAUDE_CHANNEL: "1"`) and deduplicates `cito-claude-hook` into `hooks.{Stop,SessionStart,UserPromptSubmit}` without overwriting other user hooks.
    3. Creates a timestamped backup (`<file>.bak.<timestamp>`) before modifying any existing user file and writes via temp file + atomic `rename`.

---

## Stage 2 (`v0.14.0`): Rust Hot-Path Binary (`cito-native`), Surface 3 AppKit `Settings…` Window, In-Process LAN Hub Sharing & `< 5 MB` `Cito.app`

1. **Targeted Rust Hot-Path Multi-Call Binary (`crates/cito-native` $\to$ `bin/cito-native-darwin-arm64`)**:
   - Eliminates $O(N)$ V8 heap multiplication on per-session `cito-mcp` stdio proxies (`~1.5 MB` RSS vs `~65 MB`) and cuts per-turn `cito-claude-hook` startup latency from `~70 ms` to **`< 2 ms`**, while keeping the singleton `cito-daemon` on the 247-test verified `CitoService` + `HubEngine` + `@tefx/cito/plugin-sdk` core.
2. **Surface 3: Native AppKit `Settings…` (`⌘,`) 2-Tab Window (`src/macos/cito-ui.swift`)**:
   - **Tab 1 (`Agent Apps`)**: Uses user-friendly `"Agent Apps"` terminology; lists detected apps (`ChatGPT / Codex`, `Pi`, `Claude [Experimental]`, custom plugins) with 1-click **`Set Up`** buttons calling `applySetup`.
   - **Tab 2 (`Hubs & Network`)**:
     - **Top Hero Card (`Share Hub on Local Network`)**: 1-click toggle starting an in-process `:4291` token-protected `HubServer` sharing `getLocalHubEngine(stateDir)` + **`Copy 1-Click Join Code`** button;
     - **Bottom Section (`Connected Hubs`)**: **`✨ Paste from Invite`** button (Universal Clipboard auto-pairing across Macs), inline **`+ Add Hub…`** form, per-hub `Enabled/Disabled` toggle, OIDC `Sign In…`, and hot-reload without restarting `cito-daemon`.
   - **Persistent Footer**: **`Start Cito at Login`** toggle (`~/Library/LaunchAgents/com.tefx.cito.daemon.plist`).
3. **`< 5 MB` `Cito.app` Bundle & `~/.cito/bin/{cito,cito-mcp,cito-claude-hook}` Shims**:
   - `buildMacAppBundle` (`cito setup --build-app`) packages `Cito.app` (`LSUIElement = true`) and installs self-contained `~/.cito/bin/` shims that resolve Node and `bin/cito-native-darwin-arm64` without relying on GUI LaunchServices `PATH`.

---

## Stage 3 (`v0.15.0`): Self-Diagnostics (`cito doctor`), Auto-Healing & End-to-End Verification (`cito ping`)

1. **`cito doctor [--fix]`**: Structured health checks across daemon `/health`, Hub connectivity & OIDC token validity, harness MCP configs, and live sockets.
2. **`cito ping [<target> | --self]`**: End-to-end synthetic round-trip verification (`< 2s`).

---

## Stage 4 (`v1.0.0`): Notification Activity Feed, Human Quick-Reply & Production Hardening

1. **Recent Notifications Feed & One-Click Jump in `cito-ui`**: Display recent wake/passive signals in Surface 1 with one-click session jump and inline quick-reply (`cito_send` with `in_reply_to`).
2. **Release Automation & `v1.0.0` Stability Contract**: Automated release pipeline publishing `@tefx/cito`, `Cito.app`, and `deploy/hub` container image.
