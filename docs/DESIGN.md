# Cito Architecture & Protocol Specification (V3 — macOS Menu Bar Daemon & Streamable HTTP Edition)

## 1. Purpose, Scope, and Non-Goals

### 1.1 Purpose
**Cito** (Latin *citō*: "to summon / to rouse into action / swiftly") is a macOS-native notification and signaling daemon for coding agents that wakes or delivers messages to opted-in agent conversations across:
1. **Multiple NATS Hubs** (e.g., a personal Tailscale no-auth hub `personal` and a corporate OIDC hub `rda` simultaneously),
2. **Users** (e.g., `tefx`, colleague `mingchen`),
3. **Hosts / Machines** (e.g., `mbp-work`, `mbp-personal`),
4. **Extensible Agent Harnesses** (shipped: **Pi Coding Agent** and **ChatGPT / Codex Desktop App**; open extensible architecture for **Claude Code** and future CLI/IDE harnesses), and
5. **Concurrent Conversations** within the same repository or working directory.

### 1.2 V3 Implementation Scope vs. Proposed Design Scope
- **In Scope for V3 Implementation & Verification (inside `/Users/tefx/Projects/cito`)**:
  1. **Single Per-User macOS Daemon (`cito daemon`) & Streamable HTTP MCP (`http://127.0.0.1:4290/mcp`)**:
     - Runs as a single per-user background daemon on macOS (`CITO_DAEMON_PORT`, default `4290`), managed via LaunchAgent (`~/Library/LaunchAgents/one.tefx.cito.plist`) or auto-started by the `cito-mcp` stdio forwarder.
     - Exposes `POST /mcp`, `GET /mcp`, `DELETE /mcp` (via stateless-per-request `StreamableHTTPServerTransport` sharing one `CitoService` instance) and `GET /health` (`{ ok: true, pid, stateDir }`) on `127.0.0.1`.
     - Direct Streamable HTTP clients use **0 Cito child processes per conversation tab**; stdio-only clients retain one lightweight auto-forwarder per MCP connection.
     - Eliminates all cross-process `flock` lockfiles (`src/banner/flock.ts`, `listen-*.lock`, `receiver-*.lock`, `session-*.lock`, `banner.lock`, `banner-instance.lock`, `oidc-login-*.lock`), replacing them with in-memory `KeyedMutex` instances inside the single daemon process.
  2. **Combined macOS Menu Bar Extra (`NSStatusItem`) & In-Process HUD Banner (`NSPanel`) (`src/macos/cito-ui.swift` & `src/ui/index.ts`)**:
     - Replaces `src/banner/index.ts` and `src/macos/cito-banner.swift` with a single persistent AppKit companion child process (`cito-ui`) connected to `cito-daemon` via newline-delimited JSON over `stdin`/`stdout`.
     - Provides a progressive-disclosure macOS Menu Bar Extra (`NSStatusItem` + `NSMenu`) for viewing active local listeners, 1-click manual unlistening (via submenu or holding `Option ⌥`), toggling `self`/`workspace` visibility in place, jumping to Codex threads, copying scoped listener IDs, and managing Hub connections and OIDC login.
     - Renders the 388×136 `.vibrantDark` frosted-glass top-right HUD notification banner (`NSPanel`) directly in-process (avoiding per-banner process startup, hiding via `panel?.orderOut(nil)` after 6s without exiting).
  3. **Non-Intrusive Codex Desktop Integration (`src/adapters/codex.ts`)**:
     - Removes `assertCodexThreadOpen` (`~/.codex/ipc/ipc.sock`) completely so background/unfocused Codex conversations in the sidebar never time out after 60s.
     - Treats `~/.codex/state_5.sqlite` as an optional read-only metadata source (`readCodexThreadOptional`: returning `'archived'`, `'found'`, or `'unavailable'`), falling back to `_meta.cwd` $\to$ `lsof -a -d cwd -p <http_client_pid>` $\to$ `lookupCachedSession` $\to$ `process.cwd()` + `gitMetadata(cwd)`.
     - Uses `codex queue --resume --thread <id> --message <card>` + `open -g "codex://threads/wake/<id>"`.
  4. **Standard, Vendor-Neutral OIDC (`src/auth-callout/oidc.ts`, `src/mcp/oidc.ts`)**:
     - Compatible with any standard OpenID Connect provider (Okta, Auth0, Keycloak, Microsoft Entra ID, Google, Casdoor, etc.).
     - Supports full standard JWS asymmetric algorithms (`RS256`, `RS384`, `RS512`, `PS256`, `ES256`, `ES384`, `EdDSA`), negotiates default scopes against `.well-known/openid-configuration` `scopes_supported`, supports optional fixed loopback redirect port (`oidcRedirectPort` / `CITO_OIDC_REDIRECT_PORT`), and fences listeners belonging to a previous `principal_id` on account switch.
- **Proposed Design Suggestion Only (Not Yet Implemented)**:
  - **Claude Code (`claude`)**: Documented in **Section 6** strictly as a **设计建议（仅建议，尚未实现 / Proposed Design Suggestion Only — Not Yet Implemented）**, describing how Claude Code can integrate with `cito-daemon` over Streamable HTTP MCP + SSE notifications or local socket bridges without concrete code details. `README.md` provides installation and usage guides per harness (Pi, Codex Desktop, Claude Code marked as not yet implemented, and custom harnesses).

---

## 2. Module Boundaries & Process Lifecycle

### 2.1 Acyclic Module Graph
`src/shared/` $\to$ `src/adapters/` & `src/ui/` $\to$ `src/mcp/`

- **`src/shared/`**: `config.ts` (`daemonPort`, `oidcRedirectPort`, multi-hub `hubs`), `validation.ts` (open `harnessIdSchema = z.string().regex(/^[a-z][a-z0-9_-]{0,31}$/)`), `identity.ts` (`formatScopedId`, `parseScopedId`), `mutex.ts` (in-memory `KeyedMutex`), `nats.ts`, `errors.ts`.
- **`src/adapters/`**: `types.ts`, `pi.ts` (`SocketBridgeAdapter`), `codex.ts` (`CodexDesktopAdapter`), `index.ts` (`ADAPTER_REGISTRY`, `extractCallerPidRoots`, `resolveCaller`), `git.ts`, `wake-card.ts`.
- **`src/ui/`**: `index.ts` (`UiBridge` managing the persistent `cito-ui` child process over `stdin`/`stdout` JSON lines; imports only `src/shared/` and emits UI actions via an `onAction` callback).
- **`src/macos/cito-ui.swift`**: Combined `NSStatusItem` Menu Bar Extra + `NSPanel` HUD Banner (compiled to `${stateDir}/bin/cito-ui`, zero `SQLite3` dependency).
- **`src/mcp/`**: `state.ts` (3-table `LocalState`, `PRAGMA user_version = 4`), `oidc.ts` (`OidcLogin`), `receiver.ts` (`Receiver` + `refreshSessionBadge`), `service.ts` (`CitoService` + `HubSlot`), `server.ts` (Streamable HTTP `/mcp` + `/health` + stdio server), `cli.ts` (`cito daemon` & `cito-mcp` auto-forwarding entrypoint).

### 2.2 Singleton Daemon Ownership & Startup
1. **Singleton Enforcement**:
   - `CitoConfig` includes `daemonPort: number` (`env.CITO_DAEMON_PORT ? Number(env.CITO_DAEMON_PORT) : 4290`, validated `1..65535`).
   - On `cito daemon` startup, `LocalState.open(stateDir)` runs a `BEGIN IMMEDIATE` transaction checking a singleton metadata table/row (or `GET http://127.0.0.1:${port}/health`): if a live `cito-daemon` process (`process.kill(pid, 0)`) already owns `stateDir` and responds on `/health` with `body.stateDir === config.stateDir`, `cito daemon` exits `0` cleanly (if `existing.port !== config.daemonPort`, exits `1` with an error).
   - If `cito-mcp` is invoked in stdio mode without `daemon` subcommand, it checks `GET http://127.0.0.1:${config.daemonPort}/health` (spawning `cito daemon` detached if unreachable and polling `/health` up to `5000ms`), then forwards stdio JSON-RPC messages to `http://127.0.0.1:${config.daemonPort}/mcp`.
2. **Daemon Lifecycle Table**:

| Event | Deterministic Behavior |
| :--- | :--- |
| **Daemon startup** | Opens `LocalState`, starts `UiBridge` (`cito-ui`), binds `127.0.0.1:${daemonPort}`, and runs non-interactive background connection across all hubs (`interactive = false`: connects `none` hubs and `oidc` hubs with valid/refreshable tokens and restores their `ACTIVE` listeners; if an `oidc` hub has no valid token, sets `slot.status = "auth_required"` without launching a browser). |
| **Menu `断开` (`disconnect`)** | Sets `slot.manuallyDisconnected = true`, aborts any in-flight login, cancels in-memory receivers on that slot (retaining `local_listeners.status = 'ACTIVE'` in SQLite), closes its NATS connection, sets `slot.status = "disconnected"`, and updates Pi badges & UI state. While `manuallyDisconnected === true`, **all** calls to `slot.network()` (including foreground MCP tools and background restoration) immediately throw `CitoError("NATS_UNAVAILABLE", "Hub '<name>' is manually disconnected")`; any in-flight connection resolving after `manuallyDisconnected` becomes `true` immediately closes without installing `slot.broker`. |
| **Menu `重连` / `登录 OIDC`** | Clears `slot.manuallyDisconnected = false`, calls `slot.network({ interactive: action === "login" })`, restores `ACTIVE` listeners for that hub, and broadcasts updated UI state. |
| **Menu `退出 Cito` (`quit`)** | Calls `service.close()` (stopping all in-memory receivers, clearing Pi badges, closing NATS connections, and closing `UiBridge` `stdin` so `cito-ui` exits on `EOF`) and exits `0`. (`LaunchAgent` uses `KeepAlive: { SuccessfulExit: false }` so exit `0` stays stopped while crash non-zero exits auto-restart). |

---

## 3. Streamable HTTP MCP & Non-Intrusive Caller Resolution

### 3.1 HTTP `/mcp` & Client PID Attribution (`src/mcp/server.ts`)
- Binds `http.createServer` strictly to `127.0.0.1:${config.daemonPort}`.
- Validates `Origin` header when present: allows only `undefined` or parsed origins with hostname `127.0.0.1` or `localhost` (rejecting any other `Origin` with `403 Forbidden`).
- `GET /health` returns `200 OK` with `{ ok: true, pid: process.pid, stateDir: config.stateDir }`.
- On `/mcp` requests:
  - Creates a fresh per-request `StreamableHTTPServerTransport({ sessionIdGenerator: undefined })` and per-request MCP `Server` wired to the shared `CitoService`; request close disposes only that transport and never closes `CitoService`.
  - Resolves the TCP client's OS PID (`clientPid`) from `req.socket.remotePort` via `/usr/sbin/lsof -nP -iTCP:${remotePort} -sTCP:ESTABLISHED -Fpn` (`1000ms` timeout, cached on the `net.Socket` instance), matching `p<pid>` whose `n` line equals `127.0.0.1:${remotePort}->127.0.0.1:${daemonPort}` (excluding `process.pid` when another PID matches).
  - Sets server-owned `meta.http_transport = true` and `meta.http_client_pid = clientPid` (ignoring any client-supplied `http_transport` or `http_client_pid`). If the HTTP request headers include `x-codex-turn-metadata`, merges it into `meta["x-codex-turn-metadata"]` (throwing `INVALID_INPUT` if both header and JSON-RPC `_meta` supply disagreeing thread IDs).

### 3.2 Caller Resolution (`resolveCaller` & `extractCallerPidRoots`)
1. **Step 1 — Explicit Claim Validation**:
   - Codex thread claims: `x-codex-turn-metadata.thread_id` / `session_id`, `openai/threadId`, `codexThreadId` (>1 distinct values $\to$ `INVALID_INPUT`).
   - Explicit bridge PID claims: top-level `meta.endpoint_pid`, `meta.pi_pid`, `meta.caller_pid` (non-positive integer $\to$ `PI_BRIDGE_UNAVAILABLE`; 2 distinct integers $\to$ `INVALID_INPUT`).
   - Simultaneous Codex claim + top-level explicit bridge PID claim $\to$ `INVALID_INPUT` (`meta.http_client_pid` is excluded from `explicitPid` so Codex HTTP requests carrying `x-codex-turn-metadata` + `http_client_pid` never conflict).
2. **Step 2 — `extractCallerPidRoots(meta)` Precedence**:
   1. Top-level explicit `meta.endpoint_pid`, `meta.pi_pid`, or `meta.caller_pid` $\to$ `[explicitPid]`.
   2. Nested objects in `meta` containing positive safe integers in `endpoint_pid`, `pi_pid`, `caller_pid`, or `parent_pid` $\to$ those roots.
   3. `meta.http_client_pid` $\to$ `[meta.http_client_pid]`.
   4. If `meta.http_transport === true` $\to$ `[]` (never walking `cito-daemon`'s own `process.ppid`).
   5. Otherwise (direct in-process stdio) $\to$ `[process.ppid]`.
3. **Step 3 — Non-Intrusive Codex Resolution (`readCodexThreadOptional`)**:
   - Attempts to read `${codexHome}/state_5.sqlite` (`SELECT * FROM threads WHERE id = ?`):
     - Row exists with `archived !== 0` $\to$ `{ status: "archived" }` (throws `SESSION_NOT_OPEN` when `requireOpen !== false`, returns `closed_definitive` in `probe()`).
     - Row exists with `archived === 0` $\to$ `{ status: "found", cwd, title, repo, branch }`.
     - File missing, unreadable, busy, or row/schema absent $\to$ `{ status: "unavailable" }`. `resolveCaller` then resolves `cwd` from `_meta.cwd` $\to$ `lsof -a -d cwd -p <http_client_pid> -Fn` $\to$ `lookupCachedSession` $\to$ `process.cwd()` and enriches via `gitMetadata(cwd)`.
   - `CodexDesktopAdapter.probe(row, config, signal)`: returns `closed_definitive` if `readCodexThreadOptional` returns `'archived'`, else checks `await codexDesktopRunning(signal)` (`alive` if `ChatGPT.app` is running, `closed_definitive` if not).
   - `queueCodexWake`: executes `execFile(codexBin, ["queue", "--resume", "--thread", threadId, "--message", card], { timeout: 10_000, signal, env: { ...process.env, CODEX_HOME: config.codexHome } })`.

---

## 4. Standard Vendor-Neutral OIDC & Multi-Hub Isolation

1. **Isolated `HubSlot` Configuration**:
   - Each `HubSlot` constructs its config strictly from machine properties (`{ stateDir, codexHome, codexBin, openBin, daemonPort, hostName, defaultHub, hubs }`) + its own `HubConfig` (`natsUrl`, `authMode`, `user`, `oidcIssuer`, `oidcClientId`, `oidcScopes`, `oidcRedirectPort`), never inheriting another hub's optional `oidcScopes` or `oidcRedirectPort`.
2. **Standard JWS Algorithms & Scope Negotiation**:
   - `SUPPORTED_OIDC_ALGS = ["RS256", "RS384", "RS512", "PS256", "ES256", "ES384", "EdDSA"] as const`.
   - `OidcLogin` reads/writes only `join(stateDir, "oidc-token-" + hub.name + ".json")`.
   - Discovery checks `discovery.scopes_supported`: if present as an array and missing `"openid"`, throws `AUTH_UNAVAILABLE`. When `hubConfig.oidcScopes` is unset, filters `["openid", "profile", "email", "offline_access"]` against `discovery.scopes_supported` (when present).
   - Callback server binds `127.0.0.1:${hubConfig.oidcRedirectPort ?? 0}` under `oidcMutex.run("port:" + ((hubConfig.oidcRedirectPort ?? 0) || hub.name))` and aborts immediately if the slot is disconnected or closed.
3. **Principal-Change Fencing**:
   - When `slot.network()` authenticates as `slot.principal`, before calling `restoreReceivers`, any rows in `local_listeners` for `slot.name` with `status IN ('ACTIVE', 'REBINDING')` and `principal_id !== slot.principal.principal_id` are transitioned to `'FENCED_INACTIVE'` and their in-memory receivers stopped.

---

## 5. UI Bridge Protocol & Menu Bar Extra (`src/ui/index.ts`, `src/macos/cito-ui.swift`)

### 5.1 Framed Pipe Protocol (`stdin` / `stdout` JSON Lines)
- Both `UiBridge` and `cito-ui.swift` incrementally buffer and decode `\n`-delimited UTF-8 lines (max line length 1 MiB), runtime-validating JSON payloads and logging invalid frames to `stderr` without crashing.
- `UiBridge` coalesces pending `{ op: "state" }` snapshots, swallows `EPIPE` errors when the child pipe is closed, and respawns `cito-ui` (500ms backoff) with an immediate state snapshot replay if `cito-ui` exits unexpectedly while `cito-daemon` is running.
- On `stdin` `EOF`, `cito-ui.swift` immediately terminates (`exit(0)`).

```ts
export interface UiHubSummary {
  name: string;
  status: "connected" | "connecting" | "auth_required" | "disconnected";
  user: string | null;
  auth_mode: "none" | "oidc";
  error: string | null;
}

export interface UiListenerSummary {
  hub: string;
  listener_id: string; // canonical c_<fp12>
  public_id: string;
  scoped_id: string;
  harness: string;
  session_id: string;
  title: string;
  cwd: string;
  repo: string | null;
  scope: "self" | "workspace";
  can_jump: boolean;
}

export type DaemonToUiMessage =
  | { op: "state"; hubs: UiHubSummary[]; listeners: UiListenerSummary[] }
  | { op: "banner"; payload: BannerPayload };

export type UiToDaemonAction =
  | { op: "unlisten"; hub: string; listener_id: string }
  | { op: "unlisten_all" }
  | { op: "set_scope"; hub: string; listener_id: string; scope: "self" | "workspace" }
  | { op: "hub_action"; hub: string; action: "login" | "reconnect" | "disconnect" }
  | { op: "quit" };
```

### 5.2 UI-Driven Service Operations & Live-Receiver Rule
- **Live-Receiver Rule for Badges & UI State**:
  Both `refreshSessionBadge` and the UI `{ op: "state" }` `listeners` list include only `ACTIVE` rows that have a live, non-aborted in-memory `Receiver` in `slot.receivers` (`receiver && !receiver.abort.signal.aborted && receiver.owner === row.owner_token`).
- **`unlistenById(hub, listenerId)` & `unlistenAll()`**:
  Serialized under `listenMutex.run("${hub}:${listenerId}")`. Immediately cancels the in-memory `Receiver`, updates SQLite `status = 'UNLISTENED'` (and moves pending `'RECEIVED'/'DISPATCHING'` signals to `'STORED_PASSIVE'` with `held_reason = 'unlistened_before_dispatch'`), refreshes the Pi badge, and pushes the updated UI state; then attempts the NATS KV `state: "UNLISTENED"` update best-effort.
- **`setScope(hub, listenerId, scope)` & Scope Reconciliation**:
  Serialized under `listenMutex.run("${hub}:${listenerId}")`. Preserves `listener_id`, `owner_token`, and `binding_generation`. Updates NATS KV **first**; once KV succeeds, the KV entry's `scope` is authoritative for that `(owner_token, binding_generation)` and SQLite is updated (`UPDATE local_listeners SET scope = ? WHERE hub = ? AND listener_id = ?`). In `restoreReceivers` and `Receiver.probe()`, if `remote.owner_token === local.owner_token && remote.binding_generation === local.binding_generation && remote.state === "ACTIVE"` and `local.scope !== remote.scope`, SQLite is reconciled to `remote.scope` before starting the receiver or renewing KV.

---

## 6. Proposed Design for Claude Code Integration (设计建议 — 仅建议，尚未实现)

> **设计建议 — 仅建议，尚未实现 / Proposed Design Suggestion Only — Not Yet Implemented**：以下为在 `cito-daemon` 常驻架构下未来接入 Claude Code 的架构设计建议，不包含具体代码实现细节，当前版本尚未实现 Claude Code 适配。

1. **零子进程 Streamable HTTP 接入与会话标识建议**：
   - 建议在 Claude Code 的 MCP 配置中直接指向本地常驻守护进程端点 `http://127.0.0.1:4290/mcp`。
   - 未来适配需验证 Claude Code 实际提供的会话标识与通知能力，单独设计连接/会话映射；当前 V3 的 HTTP transport 是每请求独立的无状态实例，TCP 连接关闭不能代表会话退出，PID/cwd 本身也不能区分同一进程中的多个会话。以下均为未来扩展建议。
   - 守护进程可通过 HTTP 连接的本地客户端套接字（`lsof` 归属的 Claude 进程 PID 与工作目录 `cwd`，或 MCP 标准 `roots/list` 协议返回的工作区 URI）结合连接级会话标识，自动完成会话绑定与进程存活检测（未来需以经验证的会话生命周期信号注销监听，不能直接以单次 HTTP 请求/连接关闭为依据），无需在 Claude Code 内安装额外的外部桥接进程。
2. **双模通知投递建议（MCP SSE 通道推送 + 回合拉取兜底）**：
   - **主动推送（SSE Channel Push）**：未来需增加会话路由与持久通知通道，并确认 Claude Code 支持相应能力。当前 V3 未实现 Claude 专用 Channel Push。在扩展后的 Streamable HTTP 模式下，Claude Code 保持一条 `GET /mcp` 的 SSE 事件流连接；守护进程在握手时声明实验性通知通道能力（如 `claude/channel`），收到信号时直接通过该会话的 SSE 连接推送唤醒卡片以唤醒空闲的 Claude 会话。
   - **回合开始拉取兜底（Turn-Start Inbox Drain）**：当客户端未启用实验性通道或正忙时，投递结果记为“已派发未确认（`delivered_unconfirmed`）”，使信号保留在未读收件箱中；配合 Claude Code 的 `UserPromptSubmit` Hook 或在回合开始时调用无参 `cito_inbox()`（默认读取即清空），确保消息在下一轮对话开始时可靠进入上下文。
