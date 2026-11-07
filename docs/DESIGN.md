# Cito Architecture & Protocol Specification (V17 — NATS-Free Native `HubEngine`, `local://` In-Process & `cito hub` HTTP/SSE Server, Control-Plane Guidance Without Hard Caps & `cito setup` CLI)

## 1. Purpose, Scope, and Architectural Principles

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for coding agents that wakes or delivers messages to opted-in agent conversations across:
1. **In-Process Single-Mac (`local`) and Remote Shared Hubs (`cito hub`)** (e.g., built-in `local` in-process hub, Tailscale private mesh `tailnet`, and team OIDC hub `team` simultaneously),
2. **Users** (e.g., `alice`, `bob`, scaling to $U = 50\dots 500+$ team members),
3. **Hosts / Machines** (e.g., `macbook-work`, `macbook-personal`),
4. **Extensible Agent Harnesses via Unified Plugin Architecture** (bundled in `plugins/`: **Pi Coding Agent** (`tier: "stable"`), **ChatGPT / Codex Desktop App** (`tier: "stable"`), and **Claude Code** (`tier: "experimental"`); open out-of-tree plugin standard in `${CITO_STATE_DIR}/plugins/<id>/` via `@tefx/cito/plugin-sdk`), and
5. **Concurrent Conversations** within the same repository or working directory (scaling to $N = 100\dots 4,000+$ online agents across a network).

### 1.2 Language, Neutral Naming, Harness-Agnostic Core & Mechanism-Not-Policy Rules
- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, directory panel, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`, `deploy/**`) MUST be written in English.
- **Zero Company- or Environment-Specific Names**: No internal company, hub, identity-provider, project, or colleague names (such as `rda`, `casdoor`, `rcatwin`, `keppel`, `mingchen`) may appear anywhere in source code, UI, configuration defaults, tests, or documentation. All examples and tests use neutral generic names (`local`, `tailnet`, `team`, `personal`, `work`, `alice`, `bob`, `acme/service`, `https://id.example.com`).
- **Harness-Agnostic Core (`src/` & `src/macos/cito-ui.swift`)**: Core modules (`src/shared/`, `src/hub/`, `src/mcp/`, `src/setup/`, `src/ui/`, `src/macos/cito-ui.swift`, and the Plugin Host SDK in `src/adapters/`) MUST NOT contain hardcoded branches, configuration fields, CLI flag names, or visual tables for specific agent harnesses (`pi`, `codex`, `claude`). All harness-specific discovery, caller detection, delivery, stdio channel wrapping, CLI flags, `cito setup` file-mutation descriptors (`adapter.setup`), and UI presentation metadata live inside plugin directories (`plugins/{pi,codex,claude}/` and `${CITO_STATE_DIR}/plugins/<id>/`).
- **Mechanism, Not Policy (Control-Plane Recommendation Without Hard Message-Length Restriction)**:
  - `cito_send` and `cito_listen` MCP tool descriptions recommend using Cito primarily as a lightweight control plane (waking sessions, status handoffs, and coordinating work) while pairing with dedicated channels (shared files, Git branches, issue trackers, or discussion tables) for extended multi-turn discussions or large artifacts.
  - However, `cito_send.message` enforces **no hard 500-character policy cap**:
    - `src/shared/validation.ts` exports `sanitizeSignalMessage(value: unknown): string`, which strips ASCII control characters `[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]`, trims leading/trailing whitespace, and requires `1..65,536` Unicode codepoints (a 64 KiB transport safety bound).
    - **Both** `sendInputSchema.message` (`src/shared/validation.ts`) **and** receiver-side `signalSchema.message` (`src/mcp/state.ts`) MUST use `sanitizeSignalMessage` so messages over 500 codepoints pass admission, wire transport, `Receiver.receive()`, wake card formatting, and `cito_inbox` end-to-end.
    - `sanitizeMessage(value: unknown): string` (`1..500` Unicode codepoints) remains strictly on `listenInputSchema.about` (`src/shared/validation.ts`) and `registrationSchema.about` (`src/mcp/state.ts`).
- **Opt-In Host Channels**: A host channel that binds a second bridge socket from inside another process (`CITO_CLAUDE_CHANNEL=1`) MUST remain strictly opt-in.

---

## 2. Module Boundaries, Plugin Host SDK & Unified NATS-Free `HubEngine`

### 2.1 Acyclic Module Graph & Package Dependencies
`src/shared/` $\to$ `src/hub/` & `src/adapters/` (Plugin Host SDK) & `src/setup/` & `src/ui/` $\to$ `src/mcp/`, with `plugins/{pi,codex,claude}/` importing only `src/shared/` and `src/adapters/`.

- **Zero NATS Dependencies (`package.json`)**:
  - `"dependencies"` contains **only** `"@modelcontextprotocol/sdk"`, `"jose"`, and `"zod"`.
  - `"nats"`, `"nats.ws"`, `"@nats-io/jwt"`, `"nkeys.js"`, and `"@eplightning/nats-server"` are completely removed from `package.json`.
  - `"bin"` map in `package.json`:
    ```json
    {
      "cito": "./bin/cito-mcp",
      "cito-mcp": "./bin/cito-mcp",
      "cito-claude-hook": "./bin/cito-claude-hook"
    }
    ```
    (`bin/cito-auth-callout` and `src/auth-callout/{cli,config,responder}.ts` are removed; `OidcVerifier` and `fetchDiscovery` remain in `src/auth-callout/oidc.ts` and are re-exported by `src/hub/index.ts`).

### 2.2 Orthogonal Hub Configuration & Scope Rules (`src/shared/config.ts`, `src/shared/identity.ts`, `src/mcp/service.ts`)
- **`HubConfig` Schema (`src/shared/config.ts`)**:
  ```ts
  export type HubAuthMode = "none" | "token" | "oidc";
  export interface HubConfig {
    natsUrl: string;              // Validated URL string ("local://", "http://...", "https://...", or legacy "nats://...", "ws://...", "wss://...")
    authMode: HubAuthMode;        // "none" | "token" | "oidc"
    token?: string;               // Required when authMode === "token"
    oidcIssuer?: string;          // Required when authMode === "oidc"
    oidcClientId?: string;        // Required when authMode === "oidc"
    oidcScopes: string[];
    user: string;
    enabled: boolean;             // Default true; when false, omitted from active CitoService.slots
    defaultScope?: "self" | "workspace";
  }
  ```
- **URL Validation & Transport Normalization**:
  - `config.json` hub entries accept `"url"` (preferred) as well as `"nats_url"` / `"natsUrl"`, `"auth"` or `"auth_mode"` / `"authMode"`, `"token"`, `"enabled"` (boolean, default `true`), and `"default_scope"` / `"defaultScope"`.
  - `validateNatsUrl(raw: string)` accepts `"local"` / `"local://"` (normalized to `"local://"`), `"http://..."`, `"https://..."`, `"nats://..."`, `"ws://..."`, and `"wss://..."` (rejecting any other scheme such as `"ftp://..."`, credentials, query strings, or fragments, and preserving the validated string on `HubConfig.natsUrl` so `loadConfig({ CITO_NATS_URL: "wss://nats.example/ws" }).natsUrl === "wss://nats.example/ws"`).
  - At connection time in `src/hub/client.ts`, `toHttpHubUrl(natsUrl)` maps `"nats://host[:port]"` $\to$ `"http://host:${port || 4222}"`, `"ws://..."` $\to$ `"http://..."`, and `"wss://..."` $\to$ `"https://..."`.
- **Default Hub Resolution in `loadConfig(env)` & `isSingleDefaultHub(config)`**:
  - When `config.json` is absent:
    - If `env.CITO_HUB_URL` or `env.CITO_NATS_URL` is set, synthesize hub `[env.CITO_HUB ?? "default"]` with that URL (keeping `defaultHub: "default"` so all existing `fixture()` tests in `tests/mcp/` and `tests/e2e/` that call `f.service.slots.get("default")` work unchanged).
    - Otherwise (true zero-config standalone mode), synthesize `{ local: { natsUrl: "local://", authMode: "none", enabled: true, defaultScope: "workspace", ... } }` with `defaultHub = env.CITO_HUB ?? "local"`.
  - In `src/shared/identity.ts`, `isSingleDefaultHub(config)` returns `Object.keys(config.hubs).length === 1 && (config.defaultHub === "default" || config.defaultHub === "local")` so single-hub unprefixed IDs (`formatScopedId` / `parseScopedId`) work identically for both `"local"` and `"default"`.
  - In `CitoService`, `this.slots` only instantiates `HubSlot`s for hubs where `enabled !== false`. If all configured hubs have `enabled === false`, `this.slots` is empty and tool calls requiring a hub return `CitoError("NATS_UNAVAILABLE", "No enabled hubs configured")`.
- **Scope Resolution & Cross-Hub Privacy Reset (`src/mcp/service.ts`, `src/macos/cito-ui.swift`)**:
  - Define `const defaultScope = slot.config.defaultScope ?? (slot.config.natsUrl === "local://" ? "workspace" : "self")`.
  - In `CitoService.listen`:
    - On initial enrollment (`INSERT`), use `input.scope ?? local?.scope ?? remote?.scope ?? defaultScope`.
    - On re-enrollment of an existing row (`rebind`), preserve `input.scope ?? row.scope` (so re-listening without `scope` never overwrites an existing `"workspace"` scope).
  - In `CitoService.handleUiAction`:
    - `case "listen_session"`: pass `...(action.scope ? { scope: action.scope } : {})` to `this.listen(...)` so `defaultScope` applies when `action.scope` is omitted.
    - `case "switch_hub"`: when migrating from `from_hub` to `to_hub`, if `toSlot.config.authMode === "oidc"` and `fromSlot.config.authMode !== "oidc"`, set `scope: "self"` (fail-closed privacy reset); otherwise inherit `row.scope`.
  - In `src/macos/cito-ui.swift`:
    - `Frame` validation accepts `["none", "token", "oidc"].contains($0.auth_mode)` (and `UiHubSummary.auth_mode` in `src/ui/index.ts` is typed as `"none" | "token" | "oidc"`).
    - On Zone 1 session cards, `let showScopeToggle = !(hub?.name == "local" && hub?.auth_mode == "none")`. When `showScopeToggle` is `false`, omit `scope:\(listener.scoped_id)` and set `baseActionWidth = canJump ? 48 : 22` (shrinking by `66pt` so the card title reclaims the `66pt` width while `open:` at `x = 302` and `stop:` at `x = 328` stay right-aligned). When `showScopeToggle` is `true`, keep `baseActionWidth = canJump ? 114 : 88` and `scope:\(listener.scoped_id)` at `x: 236, y: 30, width: 62, height: 22`.

---

## 3. Unified `HubEngine`, In-Process `local://` & Remote HTTP/SSE `cito hub` (`src/hub/`)

### 3.1 Single-Authority `HubEngine` (`src/hub/engine.ts`)
- Backed by `node:sqlite` (`DatabaseSync` at `${dataDir}/hub.sqlite` with `PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;` or `:memory:` for in-process `local://` / ephemeral test hubs) + in-memory `EventEmitter` dispatch:
  - `hub_meta` table: tracks bucket-wide monotonic `kv_seq INTEGER` (so `hub_kv.revision` is strictly monotonic across all keys in the bucket, matching `streams.info("KV_cito_listeners").state.last_seq`).
  - `hub_kv` table: `key TEXT PRIMARY KEY, revision INTEGER NOT NULL, created_ms INTEGER NOT NULL, value BLOB NOT NULL, principal_id TEXT, scope TEXT`.
  - `hub_signals` table: `seq INTEGER PRIMARY KEY AUTOINCREMENT, subject TEXT NOT NULL, msg_id TEXT UNIQUE, created_ms INTEGER NOT NULL, data BLOB NOT NULL, acked INTEGER NOT NULL DEFAULT 0`.
  - `hub_audit_log` table (plus optional `CITO_HUB_AUDIT_LOG` JSONL file): `id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, principal_id TEXT NOT NULL, action TEXT NOT NULL, target_id TEXT NOT NULL, detail_json TEXT`.
- **Server-Side Row-Level Security (RLS) on `authMode === "oidc"`**:
  - Every `HubEngine` operation receives the verified caller `principal: Principal | null`.
  - **KV Reads (`kv_get`, `kv_keys`, `kv_snapshot`, SSE `"kv"` events)**:
    - In `kv_keys`, `kv_snapshot`, and SSE `"kv"` watch events, omit any registration row where `scope === "self"` and `principal_id !== caller.principal_id`.
    - In `kv_get(key)`: if the row exists, its lease is currently `ACTIVE` (non-expired), and it has `scope === "self"` with `principal_id !== caller.principal_id`, return `{ forbidden_scope: true }` (containing **zero** private registration bytes); the client's `kv.get(key)` returns `null` for ordinary callers, while `CitoService.send` (via `kv.get(key, { checkForbiddenScope: true })` or server-side `js_publish`) maps `{ forbidden_scope: true }` to `CitoError("FORBIDDEN_TARGET_SCOPE", "Target listener is private to another user")` so `cito_send` to another user's active `self`-scoped session throws `FORBIDDEN_TARGET_SCOPE` without leaking a single byte of the private registration!
  - **KV Writes (`kv_create`, `kv_put`, `kv_update`)**:
    - If the incoming registration JSON has `principal_id !== caller.principal_id`, or if the existing row in `hub_kv` has a non-null `principal_id !== caller.principal_id`, reject with `403 PERMISSIONS_VIOLATION`.
    - When `existing.principal_id === caller.principal_id` and `incoming.principal_id === caller.principal_id`, permit the update regardless of whether `scope` is `"self"` or `"workspace"`.
  - **Signal Publish & Consume (`js_publish`, `nc_publish`, `consumer_add`, `consumer_ack`, `consumer_delete`, SSE `"signal"` events)**:
    - Any `nc_publish` on a subject matching `"cito.signal.*"` is ingested into `hub_signals` and dispatched to consumers identically to `js_publish`.
    - Any `nc_publish` or `nc_subscribe` on a subject outside `"cito.signal.*"` and `"cito.ack.*"` (e.g., `"forbidden"` or `"outside.cito.scope"`) is rejected with `403 PERMISSIONS_VIOLATION`.
    - On `authMode === "oidc"`, `js_publish` / `nc_publish` to `cito.signal.<targetId>` verifies both `signal.from.principal_id === caller.principal_id` (else `403 PERMISSIONS_VIOLATION`) and that `<targetId>` in `hub_kv` does not have `scope === "self"` with `principal_id !== caller.principal_id` (else `403 FORBIDDEN_TARGET_SCOPE`).
    - On `authMode === "oidc"`, `consumer_add` and SSE `"signal"` delivery for `cito.signal.<targetId>` verify that `<targetId>` in `hub_kv` belongs to `caller.principal_id`.

### 3.2 `Broker` Seam & HTTP/SSE Transport Semantics (`src/hub/client.ts`, `src/hub/server.ts`)
- Both `connectLocalHub(config)` and `connectRemoteHub(options)` return the `Broker` interface (`{ kv, js, jsm, nc }`), and `src/shared/index.ts` continues to export `connectNats`, `provisionNats`, and `isJsApiError(error, code)` (matching `error?.api_error?.err_code === code || Number(error?.code) === code`) as NATS-free adapters over `src/hub/`.
- **Critical Wire & Timing Invariants**:
  1. **Authenticated Handshake + SSE Readiness Before `connectRemoteHub()` Resolves**:
     - `connectRemoteHub()` opens `GET /v1/events` (SSE), awaits HTTP `200 OK` and the initial `:ok\n\n` readiness comment frame (registering the client in `HubServer`'s active SSE subscriber set), and verifies `POST /v1/rpc` (`{ op: "stream_info", stream: "KV_cito_listeners" }`) **before** resolving `connectRemoteHub()`.
     - On HTTP `401`, `connectRemoteHub()` rejects with `Object.assign(new Error("Authorization Violation"), { code: "AUTHORIZATION_VIOLATION" })` so `HubSlot.network()` triggers `this.login.recover()` in OIDC mode or sets `slot.status = "auth_required"` in token/OIDC mode.
     - Because `/v1/events` is already open before `connectRemoteHub()` resolves, synchronous `broker.nc.subscribe("cito.ack." + signalId)` is immediately active before `js.publish` executes, eliminating any cross-connection ACK race.
  2. **Inflight `nc.publish` Tracking & `nc.flush()`**:
     - `broker.nc.publish(subject, data)` is synchronous (`void`), tracking the underlying `POST /v1/rpc` (`op: "nc_publish"`) promise in an inflight set; `await broker.nc.flush()` awaits all inflight `nc.publish` promises.
     - If any RPC (`nc_publish`, `js_publish`, `kv_*`) fails with `401` or `403` (`PERMISSIONS_VIOLATION` / `AUTHORIZATION_VIOLATION`), the client pushes `{ type: "error", data: "PERMISSIONS_VIOLATION" }` (or `AUTHORIZATION_VIOLATION`) to `broker.nc.status()`, triggering `HubSlot.monitor()`.
  3. **CAS Conflict Error Shape (`10071`)**:
     - Both `kv.create(key, data)` (when `key` already exists in `hub_kv`) and `kv.update(key, data, rev)` (when `rev !== existing.revision`) throw `Object.assign(new Error("wrong last sequence"), { code: "10071", api_error: { err_code: 10071 } })`.
  4. **`QueuedIterator.stop()` Queue-Draining & Empty-Snapshot `initializedFn()`**:
     - `kv.watch(opts)` and `kv.keys()` return an async iterable with `.stop()` and `.getPending()`.
     - Calling `.stop()` (including from inside `opts.initializedFn()` before the `for await` loop begins) marks the iterator closed for future live events while still yielding all already-buffered snapshot items through `for await`.
     - `kv.watch(opts)` always invokes `opts.initializedFn?.()` once the initial snapshot is buffered, **even when the initial snapshot has 0 entries**.
  5. **Signal Consumer Replay + Live SSE Deduplication & `timestampNanos`**:
     - Each signal item carries `seq` and `created_ms`, exposed on `JsMsg.info.timestampNanos = Number(created_ms) * 1_000_000` (strictly a JS `number`, never a `BigInt`, matching `new Date(msg.info.timestampNanos / 1_000_000)` in `src/mcp/receiver.ts:141`).
     - `consumer.consume()` yields unacknowledged signals from `consumer_add` and live SSE `"signal"` events for that subject, deduplicating any `seq` already yielded.

---

## 4. `cito hub` Server CLI & `cito setup` Onboarding Engine (`src/hub/cli.ts`, `src/setup/index.ts`, `src/mcp/cli.ts`)

### 4.1 `cito hub` Subcommand (`src/hub/cli.ts`)
- Syntax:
  ```sh
  cito hub [--port <port>] [--bind <addr>] [--auth none|token|oidc] [--token <token>] [--issuer <url> --client-id <id>] [--data-dir <dir>]
  ```
- Environment fallbacks: `CITO_HUB_PORT` (`4291`), `CITO_HUB_BIND` (`"127.0.0.1"` when `auth === "none"`, `"0.0.0.0"` when `auth === "token" | "oidc"`), `CITO_AUTH_MODE` (`"none" | "token" | "oidc"`), `CITO_HUB_TOKEN`, `CITO_OIDC_ISSUER`, `CITO_OIDC_CLIENT_ID`, `CITO_HUB_DATA_DIR`.
- Validation & lifecycle contract:
  - Unknown flags or invalid/missing configuration (`CITO_AUTH_MODE` not in `none|token|oidc`, non-integer/out-of-range port, missing/invalid `CITO_OIDC_ISSUER` or `CITO_OIDC_CLIENT_ID` when `auth === "oidc"`, whitespace-only `CITO_HUB_TOKEN`) exit with code `1`, empty `stdout`, and `cito hub: <reason>\n` on `stderr` (never echoing secret tokens).
  - When listening, prints `cito hub: ready on http://<bind>:<port> (auth=<mode>)\n` and the `cito setup` join command to `stderr`, and exits cleanly with code `0` on `SIGINT` / `SIGTERM`.

### 4.2 `cito setup` Pure CLI Configuration Engine (`src/setup/index.ts` & Plugin `setup` Descriptors)
- **Intercepted Before `loadConfig()` in `src/mcp/cli.ts`**: `cito setup` runs before `loadConfig()` so it can initialize or repair `~/.cito/config.json`.
- **Harness-Agnostic Plugin Delegation (`adapter.setup` in `src/adapters/types.ts`)**:
  - `HarnessAdapter` includes optional `setup?: HarnessSetupDescriptor`:
    ```ts
    export interface SetupFileAction {
      target: string;
      path: string;
      status: "created" | "updated" | "unchanged" | "skipped" | "conflict";
      backupPath?: string;
      detail: string;
    }
    export interface HarnessSetupDescriptor {
      inspect(env: NodeJS.ProcessEnv): Promise<{ installed: boolean; configured: boolean; paths: string[] }>;
      plan(env: NodeJS.ProcessEnv, options: { force?: boolean }): Promise<SetupFileAction[]>;
      apply(env: NodeJS.ProcessEnv, options: { force?: boolean }): Promise<SetupFileAction[]>;
    }
    ```
  - `plugins/codex/index.ts`, `plugins/pi/index.ts`, and `plugins/claude/index.ts` implement `adapter.setup` (using `CITO_CODEX_HOME ?? "~/.codex"`, `CITO_PI_HOME ?? "~/.pi/agent"`, and `CITO_CLAUDE_HOME ?? "~/.claude"` resolved against `env.HOME ?? os.homedir()`), keeping `src/setup/index.ts` 100% free of harness-specific paths or logic!
- **CLI Syntax & Programmatic Return Shapes**:
  ```sh
  cito setup [--status] [--dry-run] [--force] [--json]
             [--hub <name> --url <url> [--auth none|token|oidc] [--token <token>] [--issuer <url> --client-id <id>] [--default]]
             [--enable-hub <name>] [--disable-hub <name>]
             [--enable <harness>]
  ```
  - **Idempotency & Backup Rule**:
    - When a target file already contains the desired configuration, `status` is `"unchanged"` and **no** file write or `.bak.<timestamp>` backup is created.
    - When an existing file's content actually changes (`status === "updated"`), `applySetup` writes `<file>.bak.<timestamp>` before atomically replacing `<file>` via a same-directory temp file + `rename`.
    - When creating a new file (`status === "created"`), parent directories are created with `mkdir(..., { recursive: true })`.
  - **Codex (`~/.codex/config.toml` in `plugins/codex/index.ts`)**:
    - Strips `#` comments when scanning for `[mcp_servers.cito]` or `mcp_servers.cito.*`.
    - If absent, appends `[mcp_servers.cito]\ncommand = "cito-mcp"\nargs = []\n` at EOF (prepending `\n` if the non-empty file lacks a trailing newline) so 100% of user comments and formatting remain untouched.
    - If present and `command = "cito-mcp"`, returns `status: "unchanged"`.
    - If present with a different `command`, returns `status: "conflict"` unless `force: true` (which replaces only the `[mcp_servers.cito]` section in place).
  - **Pi (`~/.pi/agent/settings.json` & `~/.pi/agent/mcp/global.mcp.json` in `plugins/pi/index.ts`)**:
    - Adds `"npm:@tefx/cito"` to `packages` in `settings.json` and merges `mcpServers.cito = { command: "cito-mcp", args: [] }` into `mcp/global.mcp.json` (reporting `"conflict"` if `mcpServers.cito.command` differs and `!force`).
  - **Claude Code (`~/.claude/settings.json` in `plugins/claude/index.ts`)**:
    - Because `claude` has `tier: "experimental"`, `applySetup` skips `claude` (`status: "skipped"`) unless `--enable claude` (`enableHarnesses: ["claude"]`) is passed.
    - When enabled, merges `mcpServers.cito = { command: "cito-mcp", args: [], env: { CITO_CLAUDE_CHANNEL: "1" } }` and idempotently ensures `cito-claude-hook` is present in `hooks.{Stop,SessionStart,UserPromptSubmit}` without duplicating entries or clobbering existing user hooks.
  - **`~/.cito/config.json` (`0600`)**:
    - Merges hub additions/updates and `--enable-hub` / `--disable-hub` toggles, preserving existing hubs and `plugins` settings, and always writes a valid `default_hub` (updating `default_hub` to `<name>` when `--default` is passed or when adding the first remote hub).

---

## 5. Stage 1 Test Migration & Verification Obligations
1. **Replace `tests/shared/nats-server.ts` with In-Process `startHubServer` (`startNats` compatibility alias)**:
   - `startNats()` in `tests/shared/nats-server.ts` starts the native Node `HubServer` (`src/hub/server.ts`) on `127.0.0.1:0` with zero external binary.
2. **Exact Updates to Existing Unit & E2E Tests**:
   - `tests/shared/shared.test.ts`:
     - Line 117: update version assertion from `"0.12.0"` to `"0.13.0"`.
     - Line 120: update `loadConfig({})` assertion to `expect(config.natsUrl).toBe("local://")` and `expect(config.defaultHub).toBe("local")`.
     - Line 143: replace `{ CITO_NATS_URL: "https://host" }` in the invalid-env loop with `{ CITO_NATS_URL: "ftp://host" }` (since `https://host` is now a valid HTTP/SSE hub URL).
     - Line 84: add assertions that `parseInput(sendInputSchema, { to: "c_abcdefgh2345", message: "😀".repeat(2000) })` succeeds while `parseInput(listenInputSchema, { about: "😀".repeat(501) })` still throws `CitoError`.
   - `tests/shared/multi-hub.test.ts`:
     - Line 39: replace `"https://bad"` with `"ftp://bad"` in the invalid hub URL test vector.
     - Line 47: pass `{ CITO_NATS_URL: "http://127.0.0.1:4222" }` when testing `"default"` single-hub formatting, and also test zero-config `loadConfig({})` (`"local"` single-hub formatting: `formatScopedId("local", "sig_one", loadConfig({})) === "sig_one"`).
   - `tests/mcp/server.test.ts`:
     - Line 68: replace `{ CITO_NATS_URL: "https://wrong" }` with `{ CITO_NATS_URL: "ftp://wrong" }`.
     - Verify `cito_send` tool description includes the control-plane recommendation and no-hard-cap note.
   - `tests/auth-callout/cli.test.ts`, `tests/auth-callout/responder.test.ts`, `tests/shared/nats.test.ts`, `tests/e2e/oidc.test.ts`:
     - Update `tests/auth-callout/cli.test.ts` to test `cito hub` CLI lifecycle/validation and the 3 `bin` symlinks (`cito`, `cito-mcp`, `cito-claude-hook`).
     - Update `tests/auth-callout/responder.test.ts` and `tests/e2e/oidc.test.ts` to test `HubServer` in `oidc` and `token` modes (including Row-Level Security: `Alice` cannot read `Bob`'s `self`-scoped registration via `kv.get`/`kv.watch`/`kv.keys`, `cito_send` to `Bob`'s `self`-scoped listener throws `FORBIDDEN_TARGET_SCOPE`, and `Alice` cannot overwrite `Bob`'s registration).
   - `tests/setup/setup.test.ts` (new):
     - Test `inspectSetup`, `previewSetup` (`--dry-run`), and `applySetup` across `~/.cito/config.json`, `~/.codex/config.toml` (comment preservation, idempotency, conflict detection, `--force` in-place replacement), `~/.pi/agent/settings.json` + `global.mcp.json`, opt-in `--enable claude` hook deduplication, `.bak.<timestamp>` creation only on actual mutation, and zero-config `local://` end-to-end signaling (`cito_listen -> cito_find -> cito_send (>500 chars) -> cito_inbox -> cito_unlisten`).
