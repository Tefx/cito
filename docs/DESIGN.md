# Cito Architecture & Protocol Specification (V18 — Stage 2 `v0.14.0`: Rust Hot-Path Multi-Call Binary `cito-native`, Surface 3 AppKit `Settings…` Window, In-Process LAN Hub Sharing & `< 5 MB` `Cito.app` Bundle)

## 1. Purpose, Scope, and Architectural Principles

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for agent apps that wakes or delivers messages to opted-in agent conversations across:
1. **In-Process Single-Mac (`local`), Local-Network Shared (`Share Hub on Local Network`), and Remote Shared Hubs (`cito hub`)**,
2. **Users** (e.g., `alice`, `bob`, scaling to $U = 50\dots 500+$ team members),
3. **Hosts / Machines** (e.g., `macbook-work`, `macbook-personal`),
4. **Extensible Agent Apps (Harnesses) via Unified Plugin Architecture** (bundled in `plugins/`: **Pi Coding Agent** (`tier: "stable"`), **ChatGPT / Codex Desktop App** (`tier: "stable"`), and **Claude Code** (`tier: "experimental"`); open out-of-tree plugin standard in `${CITO_STATE_DIR}/plugins/<id>/` via `@tefx/cito/plugin-sdk`), and
5. **Concurrent Conversations** within the same repository or working directory (scaling to $N = 100\dots 4,000+$ online agents across a network).

### 1.2 Language, Neutral Naming, Harness-Agnostic Core & Mechanism-Not-Policy Rules
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, directory panel, settings panel, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names (such as `rda`, `casdoor`, `rcatwin`, `keppel`, `mingchen`) may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`local`, `tailnet`, `team`, `personal`, `work`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **User-Facing "Agent Apps" Terminology**: User-facing Settings UI surfaces use **"Agent Apps"** (`Agent Apps` tab, `CONNECTED AGENT APPS` header) instead of internal engineering jargon `"Harnesses"`, while preserving `harness` in internal wire and config schemas.
- **Harness-Agnostic Core (`src/` & `src/macos/cito-ui.swift`)**: Core modules (`src/shared/`, `src/hub/`, `src/mcp/`, `src/setup/`, `src/ui/`, `src/macos/cito-ui.swift`, and the Plugin Host SDK in `src/adapters/`) MUST NOT contain hardcoded branches, configuration fields, CLI flag names, or visual tables for specific agent harnesses (`pi`, `codex`, `claude`).
- **Mechanism, Not Policy (`cito_send.message`)**: `cito_send` and `cito_listen` MCP tool descriptions recommend Cito primarily as a lightweight control plane while pairing with external channels for extended multi-turn discussions or large artifacts, without enforcing a hard 500-character policy cap (`sanitizeSignalMessage` allows `1..65,536` Unicode codepoints on both `sendInputSchema.message` and `signalSchema.message`).
- **Opt-In Host Channels**: A host channel that binds a second bridge socket from inside another process (`CITO_CLAUDE_CHANNEL=1`) MUST remain strictly opt-in.

---

## 2. Hybrid Runtime Architecture: Rust Hot-Path Binary (`crates/cito-native`), Singleton Daemon & Unified `HubEngine`

### 2.1 Process Anatomy & Targeted Rust Acceleration (`crates/cito-native` $\to$ `bin/cito-native-darwin-arm64`)
On a Mac running $N$ concurrent agent sessions, Cito runs four process roles:
1. **`cito-claude-hook` (`bin/cito-claude-hook`)**: Runs synchronously on every Claude Code turn boundary (`SessionStart`, `UserPromptSubmit`, `Stop` $\times N$ sessions).
2. **`cito-mcp` stdio proxy (`bin/cito-mcp` with no CLI arguments)**: Runs $N$ persistent processes (1 per open MCP client session).
3. **`cito-daemon` (`cito daemon`)**: Runs **1 singleton per Mac** (`127.0.0.1:4290`), hosting `CitoService`, `HubEngine` (`src/hub/`), `Setup` (`src/setup/`), and `@tefx/cito/plugin-sdk` (`src/adapters/`).
4. **`cito-ui` (`bin/cito-ui-darwin-arm64`)**: Runs **1 singleton per Mac** (`401 KB` native Swift AppKit child of `cito-daemon`).

To eliminate the $O(N)$ V8 heap multiplication (`~65 MB × N`) on `cito-mcp` stdio proxies and the `~70 ms` V8 startup delay on every `cito-claude-hook` turn boundary while preserving 100% of the 247 Vitest tests and `@tefx/cito/plugin-sdk`, Stage 2 introduces a self-contained Rust multi-call binary **`crates/cito-native`** compiled to **`bin/cito-native-darwin-arm64`** (`< 1.5 MB` stripped Mach-O):

- **Mode 1: `claude-hook` (`cito-native claude-hook` or `argv[0]` ending in `cito-claude-hook`)**:
  - Reads hook JSON from `stdin` (`session_id`, `hook_event_name`, `cwd`).
  - Fast-exits `{}\n` in **`< 2 ms`** when `hook_event_name` is not `"Stop" | "SessionStart" | "UserPromptSubmit"` or when no wake file exists in `${stateDir}/claude/wakes/`.
  - Resolves session ID (`input.session_id` $\to$ `CLAUDE_SESSION_ID` $\to$ newest `<uuid>.jsonl` in `${claudeHome}/projects/<slug>/`), resolves any session alias in `${stateDir}/claude/aliases/<session>`, atomically renames `${stateDir}/claude/wakes/<session>.jsonl` to `.draining-<uuid>`, parses/sorts/deduplicates wake batches, clears unread count in `${stateDir}/claude/badges/<session>`, sends a best-effort `500ms`-timeout `cito_inbox` ack `POST` to `http://127.0.0.1:${daemonPort}/mcp`, and writes the exact hook JSON response (`decision: "block"` on `Stop` when `wake === true`, or `hookSpecificOutput.additionalContext`) to `stdout`.
- **Mode 2: `mcp-stdio` (`cito-native mcp-stdio`)**:
  - Probes `GET http://127.0.0.1:${daemonPort}/health`; if unreachable or `version !== CITO_VERSION`, spawns `<sibling-cito-mcp> daemon` detached using `CITO_NODE_BIN` / `node` and polls `/health` for up to `5,000 ms`.
  - When `CITO_CLAUDE_CHANNEL=1` (`true`/`yes` or `plugins.claude.enableClaudeChannel` in `config.json`), binds `${stateDir}/sockets/claude-pid-${ppid}.sock` (`0600` socket inside `0700` directory) answering `identify` (with `resolveClaudeSessionId` and 128 KiB tail `custom-title` scan), `status` (writing `${stateDir}/claude/badges/<session>`), and `deliver` (`fsync` wake queue append + `notifications/claude/channel` stdout notification).
  - Relays newline-delimited JSON-RPC requests from `stdin` to `POST http://127.0.0.1:${daemonPort}/mcp` (passing `_meta.endpoint_pid = ppid` or preserving the client TCP socket so `resolveHttpClientPid` attributes the caller identically) and writes JSON-RPC responses back to `stdout` (`~1.5 MB` RSS per session).
- **Entrypoint Dispatch & Fallback (`bin/cito-claude-hook`, `bin/cito-mcp`)**:
  - When `process.platform === "darwin" && process.arch === "arm64"` and `bin/cito-native-darwin-arm64` exists and `CITO_DISABLE_NATIVE !== "1"`, `bin/cito-claude-hook` and `bin/cito-mcp` (when invoked with 0 args) spawn `bin/cito-native-darwin-arm64` passing `CITO_ENDPOINT_PID: process.env.CITO_ENDPOINT_PID ?? String(process.ppid)` in `env` so `cito-native mcp-stdio` binds `${stateDir}/sockets/claude-pid-${endpointPid}.sock` to the caller harness PID rather than the intermediate Node wrapper PID; when `bin/cito-native-darwin-arm64` is absent (such as in the dist-less sandbox test in `tests/auth-callout/cli.test.ts`), they fall back seamlessly to `bin/loader.js` $\to$ `plugins/claude/hook-cli.ts` and `src/mcp/cli.ts`.

---

## 3. In-Process LAN Hub Sharing (`host_hub`), Launch at Login & Settings IPC (`src/shared/config.ts`, `src/setup/`, `src/ui/`, `src/mcp/service.ts`)

### 3.1 In-Process "Share Hub on Local Network (`:4291`)" (`host_hub`)
- **Configuration (`src/shared/config.ts`)**:
  - `CitoConfig` includes optional `hostHub?: { enabled: boolean; port: number; token: string }`, parsed from `config.json` (`"host_hub"` or `"hostHub"`, default `port: 4291`).
- **Zero-Process In-Daemon Sharing (`CitoService` in `src/mcp/service.ts`)**:
  - When `config.hostHub?.enabled === true`, `CitoService` starts `startHubServer({ bind: "0.0.0.0", port: hostHub.port, authMode: "token", token: hostHub.token, engine: getLocalHubEngine(this.config.stateDir) })`, exposing the **exact same in-process `HubEngine` (`getLocalHubEngine(this.config.stateDir)`)** over HTTP/SSE without spawning a second process or duplicating SQLite state.
  - Primary LAN/Tailscale hostname/IP is resolved via `os.networkInterfaces()` / `os.hostname()`, producing:
    - `endpoint_url`: `http://${lanHost}:${port}`
    - `invite_command`: `cito setup --hub ${hostSlug} --url http://${lanHost}:${port} --auth token --token ${token} --default`
  - Toggling `set_host_hub` (`{ op: "set_host_hub", enabled: boolean, port?: number, reset_token?: boolean }`) updates `config.json` (auto-generating a 32-byte hex token if none exists or if `reset_token` is true), starts or stops the in-process `HubServer` in `< 100 ms` without dropping existing `local` listeners, and pushes updated `UiSettingsState` to `cito-ui`.

### 3.2 `Start Cito at Login` (LaunchAgent) & `~/.cito/bin/` Absolute-Path Shims (`src/setup/index.ts`)
1. **Self-Contained `~/.cito/bin/{cito,cito-mcp,cito-claude-hook}` Shims (`ensureCliShims`)**:
   - On `applySetup()` and `CitoService.create()`, `ensureCliShims(config)` writes executable shims (`0755`) into `${stateDir}/bin/{cito,cito-mcp,cito-claude-hook}`:
     - For `cito-claude-hook`: directly executes `bin/cito-native-darwin-arm64 claude-hook` when present (so Claude Code hook execution bypasses Node startup completely and runs in `< 2 ms`!), falling back to `"${nodeBin}" "${pkgRoot}/bin/cito-claude-hook"`.
     - For `cito` and `cito-mcp`: executes `bin/cito-native-darwin-arm64 mcp-stdio` when invoked with 0 arguments (`$# -eq 0`), and `"${nodeBin}" "${pkgRoot}/bin/cito-mcp" "$@"` when CLI subcommands (`listen`, `unlisten`, `setup`, `hub`, `daemon`) are passed.
   - This eliminates macOS LaunchServices GUI `PATH` failures when `ChatGPT.app` or `Claude.app` spawns `cito-mcp` without a login shell!
2. **LaunchAgent Management (`setLaunchAtLogin` / `inspectLaunchAtLogin`)**:
   - Manages `${HOME}/Library/LaunchAgents/com.tefx.cito.daemon.plist` (`Label = "com.tefx.cito.daemon"`, `RunAtLoad = true`, `KeepAlive = false`, `ProgramArguments = [nodeBin, citoMcpPath, "daemon"]`, `EnvironmentVariables = { HOME, PATH, CITO_STATE_DIR }`), respecting `env.HOME` in tests so unit tests never touch the real user `~/Library/LaunchAgents`.
   - Controlled via CLI (`cito setup --login-item` / `--no-login-item`) and UI action `{ op: "set_launch_at_login", enabled: boolean }`.

### 3.3 `< 5 MB` `Cito.app` Bundle Builder (`src/setup/index.ts`)
- `buildMacAppBundle(options?: { destDir?: string; env?: NodeJS.ProcessEnv }): Promise<{ appPath: string; bytes: number }>` (also exposed via `cito setup --build-app [destDir]`):
  - Creates `<destDir>/Cito.app` (defaulting to `join(stateDir, "Applications/Cito.app")`):
    - `Contents/Info.plist` (`CFBundleIdentifier = "one.tefx.cito"`, `CFBundleName = "Cito"`, `CFBundleExecutable = "Cito"`, `CFBundleShortVersionString = CITO_VERSION`, `LSUIElement = true`, `LSMinimumSystemVersion = "13.0"`),
    - `Contents/MacOS/Cito` (executable launcher that resolves Node across `CITO_NODE_BIN`, `/opt/homebrew/bin/node`, `/usr/local/bin/node`, `~/.local/share/mise/shims/node`, `~/.nvm`, `~/.fnm`, `~/.volta`, and `~/.cito/runtime/bin/node`, installs `~/.cito/bin/` shims, and runs `cito daemon`),
    - `Contents/Helpers/cito-ui` (`bin/cito-ui-darwin-arm64`) and `Contents/Helpers/cito-native` (`bin/cito-native-darwin-arm64`),
    - Ad-hoc codesigned (`codesign -s - --force --deep` when on Darwin), total bundle size **`< 5 MB`**.

### 3.4 Settings Wire Protocol (`src/ui/index.ts`, `src/mcp/service.ts`)
- **`DaemonToUiMessage` (`op: "state"`) Extension**:
  - Adds optional `settings?: UiSettingsState` to `DaemonToUiMessage` (`op: "state"`) and optional 7th parameter `settings?: UiSettingsState` to `UiBridge.state(hubs, listeners, available = [], myRemoteListeners = [], directoryOnlineCount = 0, harnesses, settings)` (defaulting `settings` when omitted so all existing 2- to 6-argument `bridge.state(...)` calls in `tests/ui/lifecycle.test.ts` remain 100% compatible):
    ```ts
    export interface UiAppSetupSummary {
      id: string;
      display_name: string;
      tier: AdapterTier;
      installed: boolean;
      configured: boolean;
      status: "ready" | "not_configured" | "conflict";
      detail: string;
      paths: string[];
    }
    export interface UiHostHubSummary {
      enabled: boolean;
      running: boolean;
      port: number;
      endpoint_url: string;
      token: string;
      invite_command: string;
      error: string | null;
    }
    export interface UiHubConfigSummary {
      name: string;
      url: string;
      auth_mode: HubAuthMode;
      enabled: boolean;
      is_default: boolean;
      status: "connected" | "connecting" | "auth_required" | "disconnected";
      user: string | null;
      error: string | null;
    }
    export interface UiSettingsState {
      apps: UiAppSetupSummary[];
      hubs: UiHubConfigSummary[];
      host_hub: UiHostHubSummary;
      launch_at_login: boolean;
    }
    ```
- **`UiToDaemonAction` Extensions (`validAction` in `src/ui/index.ts` & `handleUiAction` in `src/mcp/service.ts`)**:
  - `{ op: "open_settings" }` / `{ op: "close_settings" }`
  - `{ op: "setup_apply"; harness: string; force?: boolean }` (validates `harness` with `harnessIdSchema`, runs `applySetup({ env: this.config.env, enableHarnesses: [action.harness], force: action.force ?? true })`, and refreshes UI state)
  - `{ op: "set_host_hub"; enabled: boolean; port?: number; reset_token?: boolean }`
  - `{ op: "set_launch_at_login"; enabled: boolean }`
  - `{ op: "setup_save_hub"; name: string; url: string; auth_mode: HubAuthMode; token?: string; issuer?: string; client_id?: string; set_default?: boolean }` (calls `applySetup`, hot-reloads `this.config` and `this.slots` in `CitoService` without restarting the daemon, and connects the added/updated `HubSlot`)
  - `{ op: "setup_toggle_hub"; hub: string; enabled: boolean }` (toggles `enabled` in `config.json`, hot-adds or closes/removes the `HubSlot` in `this.slots`, and updates UI state)
  - `{ op: "setup_remove_hub"; hub: string }` (removes a non-`local` hub from `config.json`, closes/removes its `HubSlot`, and updates UI state)

---

## 4. Three-Surface macOS UI Architecture (`src/macos/cito-ui.swift`)

### 4.1 Surface 1 (`cardMenu`, `380pt` `NSMenu`) & Surface 2 (`DirectoryPanel`, `560 × 440 pt` `KeyPanel`) Invariants
1. **Footer `Settings…` Button (`id = "open_settings"`)**:
   - Placed in `footer` (`380 × 36 pt`) at `NSRect(x: 168, y: 6, width: 88, height: 24)` (`MenuControl("Settings…", frame: NSRect(x: 168, y: 6, width: 88, height: 24), leadingSymbol: "gearshape")`, `identifier = "open_settings"`).
   - **Critical `menuActions` Invariant**: Do **NOT** pass `open_settings` through `register(...)` (`menuActions.append`), keeping `initial.actions === 10` (`tests/ui/lifecycle.test.ts:87`) and `test_action` indices `2..9` (`tests/ui/lifecycle.test.ts:50`) unchanged! Clicking `open_settings` invokes `openSettingsPanel()` directly (cancelling menu tracking, opening Surface 3 `SettingsPanel`, and emitting `{"op": "open_settings"}`).
   - Preserve `stop_all` at `x: 12, y: 6, width: 82, height: 24` and `quit` at `x: 262, y: 6, width: 106, height: 24`.
2. **Preserved Empty State & Directory Facet Coordinates**:
   - Keep `"No active listeners"` text and `360 × 64 pt` card geometry in `empty` (`tests/ui/lifecycle.test.ts:163-164`).
   - Keep `dir_harness:all` at `x: 154, y: 5, width: 98, height: 22` and `curX = 256` in `DirectoryPanel` (`tests/ui/lifecycle.test.ts:468-470, 776, 787`).

### 4.2 Surface 3: Native AppKit `SettingsPanel` (`520 × 460 pt` `KeyPanel: NSPanel`)
- **Panel Container (`id: "settings_panel"`, `520 × 460 pt`)**:
  - Floating/titled `KeyPanel` (`NSRect(x: 0, y: 0, width: 520, height: 460)`), closed via `close_settings` button (`x: 484, y: 12, w: 24, h: 24`, symbol `"xmark"`) or `Escape` key (emitting `{"op": "close_settings"}`).
- **Top Segmented Tab Bar (`id: "settings_header"`, `x: 0, y: 412, width: 520, height: 48`)**:
  - `id: "settings_tab:apps"`: **`Agent Apps`** (`leadingSymbol: "square.grid.2x2"`, `x: 132, y: 12, width: 126, height: 24`)
  - `id: "settings_tab:hubs"`: **`Hubs & Network`** (`leadingSymbol: "network"`, `x: 262, y: 12, width: 128, height: 24`)
- **Tab 1 (`settingsActiveTab == "apps"`, `id: "settings_apps_tab"`, `x: 0, y: 44, width: 520, height: 368`)**:
  - Section header: `"CONNECTED AGENT APPS"`.
  - Renders a `480 × 58 pt` card (`id: "settings_app:\(app.id)"`, `x: 20`) for each app in `settings.apps` (falling back to registered `harnesses` if `settings` has not been sent yet):
    - Harness badge (`28 × 28 pt`), title (`app.display_name`), optional `"Experimental"` pill when `app.tier == "experimental"`, and human-friendly subtitle (`app.detail`, with `toolTip` showing `app.paths.joined(separator: ", ")`).
    - Status label (`id: "settings_app_status:\(app.id)"`): `"● Connected"` when `app.configured`, else `"○ Not Set Up"`.
    - Action button (`MenuControl`, `id: "setup_app:\(app.id)"`, `x: 402, y: 17, width: 66, height: 24`): titled `"Set Up"` (`.prominent`) when `!app.configured`, or `"Re-check"` (`.neutral`) when `app.configured`; clicking emits `{"op": "setup_apply", "harness": app.id}`.
- **Tab 2 (`settingsActiveTab == "hubs"`, `id: "settings_hubs_tab"`, `x: 0, y: 44, width: 520, height: 368`)**:
  - **Top Hero Card: `Share Hub on Local Network` (`id: "host_hub_card"`, `x: 20, y: 232, width: 480, height: 124`)**:
    - Title: `"Share Hub on Local Network"`, subtitle: `"Let your other Macs or teammates connect to this Mac."`
    - Toggle button (`MenuControl`, `id: "toggle_host_hub"`, `x: 396, y: 88, width: 70, height: 24`): titled `"● Sharing"` when `hostHub.enabled`, else `"Off"`; clicking emits `{"op": "set_host_hub", "enabled": !hostHub.enabled}`.
    - Endpoint label (`id: "host_hub_endpoint"`): displays `hostHub.endpoint_url`.
    - **`Copy 1-Click Join Code`** button (`MenuControl`, `id: "copy_hub_invite"`, `x: 14, y: 12, width: 168, height: 24`, `leadingSymbol: "doc.on.doc"`): copies `hostHub.invite_command` to clipboard (`clipboardString` + `NSPasteboard.general`) and triggers `.copied()` (`"✓ Copied"`).
  - **Bottom Section: `CONNECTED HUBS` (`x: 20, y: 8, width: 480, height: 214`)**:
    - Header controls:
      - **`✨ Paste from Invite`** (`MenuControl`, `id: "paste_hub_invite"`, `x: 260, y: 190, width: 136, height: 22`, `leadingSymbol: "doc.on.clipboard"`): parses a `cito setup --hub <name> --url <url> ...` invite string or URL from `clipboardString` and immediately emits `{"op": "setup_save_hub", ...}` (or populates the Add Hub form)!
      - **`+ Add Hub…`** (`MenuControl`, `id: "open_add_hub"`, `x: 404, y: 190, width: 96, height: 22`, `leadingSymbol: "plus"`): toggles the inline Add Hub form (`id: "add_hub_form"`).
    - When `addHubFormOpen` is `false`, renders hub rows (`id: "settings_hub:\(hub.name)"`, `480 × 48 pt`) with:
      - `toggle_hub:\(hub.name)` (`"Enabled"` / `"Disabled"` $\to$ `{"op": "setup_toggle_hub", "hub": hub.name, "enabled": !hub.enabled}`),
      - `login_hub:\(hub.name)` (`"Sign In…"` when `hub.auth_mode == "oidc"` $\to$ `{"op": "hub_action", "hub": hub.name, "action": "login"}`),
      - `remove_hub:\(hub.name)` (for non-`local` hubs $\to$ `{"op": "setup_remove_hub", "hub": hub.name}`).
    - When `addHubFormOpen` is `true`, renders `id: "add_hub_form"` with text inputs `settings_hub_name`, `settings_hub_url`, auth mode selector pills `settings_hub_auth:none`, `settings_hub_auth:token`, `settings_hub_auth:oidc`, conditional fields `settings_hub_token` / `settings_hub_issuer` / `settings_hub_client_id`, `cancel_add_hub`, and `submit_add_hub` (`"Join Hub"` $\to$ emits `{"op": "setup_save_hub", ...}`).
- **Persistent Footer (`id: "settings_footer"`, `x: 0, y: 0, width: 520, height: 44`)**:
  - `id: "settings_launch_at_login"`: `MenuControl("Start Cito at Login", frame: NSRect(x: 20, y: 10, width: 164, height: 24), leadingSymbol: launchAtLogin ? "checkmark.square.fill" : "square")`, clicking emits `{"op": "set_launch_at_login", "enabled": !launchAtLogin}`.
- **Headless Inspection & Test Hooks (`src/macos/cito-ui.swift`)**:
  - `receive(_:)` state-diff guard includes `frame.settings != settings` so updates to `UiSettingsState` immediately rebuild `settingsPanel` even when `hubs` and `listeners` are unchanged.
  - `inspectMenu()` includes top-level `"settings_open": settingsVisible`, `"settings_tab": settingsActiveTab`, `"settings_launch_at_login": settingsLaunchAtLogin`, and `"settings_items": settingsPanel?.contentView.map { [project($0)] } ?? []`.
  - `findControl(in:id:)` skips hidden views (`guard !view.isHidden else { return nil }`) so only the active tab's controls are matched by `test_control` and `test_hover`.
  - `MenuControl.clearHoverWhere` preserves hover on both `directoryPanel` and `settingsPanel`.
  - Headless commands:
    - `{"op": "test_settings_input", "id": "<field_id>", "value": "<string>"}`: sets `NSTextField.stringValue` inside `settingsPanel`.
    - `{"op": "test_clipboard", "text": "<string>"}`: sets `clipboardString` for deterministic headless testing of `paste_hub_invite`.
    - `{"op": "test_settings_key", "key": "escape"}`: invokes `closeSettingsPanel()`.

---

## 5. Packaging, Build Script & Stage 2 Verification Obligations

1. **`package.json` & Compilation Contract**:
   - Bump `CITO_VERSION` (`src/shared/index.ts`) and `package.json` `"version"` to `"0.14.0"`.
   - `package.json` `"scripts.compile"`:
     `"rm -rf dist && tsc && if [ \"$(uname -s)-$(uname -m)\" = \"Darwin-arm64\" ]; then swiftc -O -target arm64-apple-macos13.0 src/macos/cito-ui.swift -o bin/cito-ui-darwin-arm64 && strip -x bin/cito-ui-darwin-arm64 && codesign -s - --force bin/cito-ui-darwin-arm64 && cargo build --release --manifest-path crates/cito-native/Cargo.toml && cp crates/cito-native/target/release/cito-native bin/cito-native-darwin-arm64 && strip -x bin/cito-native-darwin-arm64 && codesign -s - --force bin/cito-native-darwin-arm64 && rm -rf crates/cito-native/target; fi"`
     (Note: `rm -rf crates/cito-native/target` immediately reclaims intermediate Cargo build artifacts after staging `bin/cito-native-darwin-arm64`, complying with the Storage Lifetime policy!).
2. **Test Suite Updates & New Stage 2 Verification**:
   - `tests/shared/shared.test.ts:117`: update version assertion to `"0.14.0"`.
   - `tests/auth-callout/cli.test.ts:131`: update `pkg.scripts.compile` assertion to match the updated compile script, and verify `bin/cito-native-darwin-arm64` executes `claude-hook` (`< 15 ms`) and `mcp-stdio` on Darwin arm64.
   - `tests/ui/lifecycle.test.ts`: preserve all existing Surface 1 and Surface 2 assertions (`actions: 10`, `footer`, `empty`, `DirectoryPanel` facet coordinates), and add comprehensive headless AppKit tests for Surface 3 `SettingsPanel` (`open_settings`, `settings_tab:apps`, `setup_app:pi`, `settings_tab:hubs`, `toggle_host_hub`, `copy_hub_invite`, `paste_hub_invite`, `open_add_hub` + `test_settings_input` + `submit_add_hub`, `toggle_hub:local`, `settings_launch_at_login`, and `Escape` close).
   - `tests/setup/setup.test.ts` & `tests/mcp/ui.test.ts`: verify in-process `set_host_hub` (`:4291` `startHubServer` sharing `getLocalHubEngine`), dynamic hub hot-reload (`setup_save_hub`, `setup_toggle_hub`, `setup_remove_hub`), `set_launch_at_login` plist lifecycle, `~/.cito/bin/{cito,cito-mcp,cito-claude-hook}` shims, and `buildMacAppBundle` (`Cito.app` `< 5 MB`).
