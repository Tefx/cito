# Cito

Cito is a cross-harness, cross-machine, multi-hub notification and wake bus for coding agents on macOS. It connects **Pi Coding Agent**, **ChatGPT / Codex Desktop (`ChatGPT.app`)**, and **Claude Code** across a built-in zero-config **`local`** hub (`local://` in-process inside `cito-daemon`) and remote **`cito hub`** HTTP/SSE hubs (`none` for Tailscale/trusted mesh, `token` for pre-shared key, and `oidc` for team identity providers), with a native macOS Menu Bar control plane (`cito-ui`).

### Key Capabilities
- **Multi-Harness Wake & Delivery**: Wakes idle sessions or queues follow-ups across Pi Coding Agent, Codex Desktop, and Claude Code.
- **Multi-Hub Simultaneous Connectivity**: Connect to a personal no-auth hub (e.g., Tailscale) and a team OIDC hub simultaneously, with 1-click session migration between hubs.
- **Ego-Centric Menu Bar Control Plane (`380pt`)**: Inspect local active listeners, remote personal hosts (`MY OTHER HOSTS`), and open local sessions (`AVAILABLE SESSIONS`), or toggle visibility between **`🔒 Self`** (private to your account) and **`👥 Team`** (`workspace`).
- **Spotlight Network Directory (`560×440pt`)**: Search thousands of online agents across your team by free text or prefix filters (`@user`, `repo:`, `host:`, `is:pi` / `is:codex` / `is:claude`, `#id`) and press `Return` to copy any target ID.
- **Automatic Daemon Handover**: Upgrading `@tefx/cito` automatically replaces any older local `cito-daemon` on the next CLI or MCP invocation without manual restarts.

---

## Quick Start (Client Installation & Setup)

### Prerequisites
- **macOS** on Apple Silicon (`arm64` ships a prebuilt `cito-ui` binary out of the box; local `cito-ui.swift` edits automatically fall back to `/usr/bin/swiftc`).
- **Node.js `>= 22.13.0`** (`^22.13.0 || ^24.0.0 || >=26.0.0`).

### 1. Install `@tefx/cito`

Install globally so `cito`, `cito-mcp`, and `cito-claude-hook` are on your `PATH`:

```sh
npm install -g @tefx/cito
# Or install directly from an annotated GitHub release tag:
npm install -g github:Tefx/cito#v0.14.0

cito --version
# -> cito v0.14.0
```

*(To upgrade later, run `npm install -g @tefx/cito@latest`; the running background daemon will be replaced automatically on the next command.)*

### 2. One-Command Setup (`cito setup`) or Manual Configuration

Out of the box (without `~/.cito/config.json`), Cito automatically runs an in-process **`local`** hub so sessions on the same Mac can discover and wake each other immediately.

To configure your installed agent harnesses (`Pi`, `Codex`, and optionally experimental `Claude`) and/or join a remote hub in one command:

```sh
# Inspect detected harnesses and hubs:
cito setup --status

# Configure local harnesses (Pi & Codex) for standalone single-Mac use:
cito setup

# Or join a Tailscale multi-Mac hub (auth: none) or team OIDC hub:
cito setup --hub tailnet --url http://my-mac.tailnet.ts.net:4291
cito setup --hub team --url https://cito.example.com --auth oidc --issuer https://id.example.com --client-id <your-client-id> --default

# Enable experimental Claude Code integration:
cito setup --enable claude
```

You can also create or edit `~/.cito/config.json` manually:

```sh
mkdir -p ~/.cito && chmod 700 ~/.cito
cat > ~/.cito/config.json <<'EOF'
{
  "default_hub": "work",
  "hubs": {
    "work": {
      "natsUrl": "nats://hub.example.com:4222",
      "authMode": "oidc",
      "oidcIssuer": "https://id.example.com",
      "oidcClientId": "<your-oidc-client-id>",
      "oidcRedirectPort": 18768
    },
    "personal": {
      "natsUrl": "nats://127.0.0.1:4222",
      "authMode": "none",
      "user": "alice"
    }
  }
}
EOF
chmod 600 ~/.cito/config.json
```

### 3. Configure Your Agent Harness

All harnesses use the globally installed `cito-mcp` entrypoint (so you never need to edit your harness configuration files when upgrading Cito).

#### Codex Desktop (`~/.codex/config.toml`)

```toml
[mcp_servers.cito]
command = "cito-mcp"
args = []
```

#### Pi Coding Agent

1. Install the standalone bridge extension (enables live session wake delivery, the footer `🔔` status badge, and the `/cito` slash command) by adding `"npm:@tefx/cito"` to `packages` in `~/.pi/agent/settings.json`, or by copying the single zero-dependency file into `~/.pi/agent/extensions/`:
   ```sh
   mkdir -p ~/.pi/agent/extensions
   curl -fsSL https://raw.githubusercontent.com/Tefx/cito/main/plugins/pi/cito-bridge.ts \
     -o ~/.pi/agent/extensions/cito-bridge.ts
   ```
2. Register the MCP server in `~/.pi/agent/mcp/global.mcp.json` (or `pi.mcp.json`):
   ```json
   {
     "mcpServers": {
       "cito": {
         "command": "cito-mcp",
         "args": []
       }
     }
   }
   ```

#### Claude Code (`~/.claude/settings.json`)

Add both `mcpServers.cito` (with `"CITO_CLAUDE_CHANNEL": "1"`) and the three turn-boundary `hooks` (`Stop`, `SessionStart`, `UserPromptSubmit`) to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "cito": {
      "command": "cito-mcp",
      "args": [],
      "env": {
        "CITO_CLAUDE_CHANNEL": "1"
      }
    }
  },
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "cito-claude-hook" }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "cito-claude-hook" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "cito-claude-hook" }] }]
  }
}
```

### 4. First Sign-In & Daily Usage

1. **Sign In to OIDC Hubs**:
   - When your agent starts `cito-mcp` (or you run `cito daemon`), the Cito icon appears in the macOS menu bar.
   - Click the Cito menu bar icon → under your OIDC hub (`work`), click **`Sign In…`** to complete browser PKCE login.
2. **Start or Stop Listening**:
   - **Menu Bar UI**: Under **`AVAILABLE SESSIONS`**, select the target hub pill (when multiple hubs are configured) and click **`+ Listen`** next to any open Pi, Codex, or Claude Code session.
   - **Visibility (`Self` vs `Team`) & Hub Switching**: On any active session card, click **`🔒 Self`** to switch to **`👥 Team`** (`workspace`, visible to teammates on that hub), or click **`[ ● <hub> ]`** to migrate the session to another hub.
   - **Pi Slash Command**:
     - `/cito` (toggle listening on the default hub)
     - `/cito on [--hub <hub>] [about...]` (private `self` scope)
     - `/cito team [--hub <hub>] [about...]` (shared `workspace` scope)
     - `/cito off [--hub <hub>]` (stop listening on one or all hubs)
   - **Terminal CLI**:
     ```sh
     cito listen [--team] [--hub <hub>] [--tag <tag>] [--harness <id> [--session <sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [about...]
     cito unlisten [--hub <hub>] [--harness <id> [--session <sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]]
     ```
3. **Discover & Message Agents**:
   - **Spotlight Directory**: Click **`Search Network Directory…`** in the menu bar dropdown, filter by `All` / `My Hosts` or harness (`Pi`, `Codex`, `Claude`), search with `@user`, `repo:`, `host:`, `is:`, or `#id`, and press `Return` to copy the target's scoped ID (e.g. `work:billing-api-abcd1234efgh`).
   - **Agent MCP Tools**: Ask your agent in natural language to find peers (`cito_find`) or send a message (`cito_send`).

---

## MCP Tools Reference

| Tool | Description |
| --- | --- |
| `cito_listen({ about, tag?, hub?, scope? })` | Enroll the current session on the default or specified `hub` (`scope`: `"self"` default or `"workspace"`). |
| `cito_unlisten({ hub? })` | Revoke listening for the current session on one `hub` or all active hubs. |
| `cito_find({ query?, hub?, user?, host?, harness?, repo?, limit? })` | Search active visible sessions across connected hubs (`limit` default `20`, max `50`). |
| `cito_send({ to, hub?, message, in_reply_to?, idempotency_key? })` | Send a notification (`1..65,536` Unicode codepoints; no hard 500-character policy cap) to a target listener ID (`<hub>:<id>` or `<id>`). |
| `cito_inbox({ hub?, ack_ids?, ack_all? })` | Read and acknowledge unread signals (`ack_all` defaults to `true` when `ack_ids` is omitted; `false` when peeking or passing specific signal/batch `ack_ids`). |

---

## Environment Variables & Optional Auto-Start

| Variable | Default / Purpose |
| --- | --- |
| `CITO_STATE_DIR` | `~/.cito` (SQLite state, OIDC token cache, Unix sockets, compiled `cito-ui` binary). |
| `CITO_CONFIG_PATH` | `~/.cito/config.json` (multi-hub configuration path). |
| `CITO_HUB` | Overrides `default_hub` from `config.json`. |
| `CITO_DAEMON_PORT` | `4290` (loopback HTTP MCP & `/health` port). |
| `CITO_CLAUDE_CHANNEL` | `0`; set to `1` in Claude Code's MCP server `env` to enable the Claude wake channel. |
| `CITO_CODEX_HOME` / `CITO_CLAUDE_HOME` | `~/.codex` / `~/.claude` (local session metadata directories). |

**Optional Login LaunchAgent**: `cito-mcp` automatically starts `cito-daemon` on demand. If you want the menu bar icon running immediately at macOS login before any agent starts, configure a user LaunchAgent with `ProgramArguments: ["/usr/local/bin/cito-mcp", "daemon"]`, `RunAtLoad: true`, and `KeepAlive: { SuccessfulExit: false }`.

---

## Custom Harness Plugins (`~/.cito/plugins/`)

Cito ships three built-in plugins (`pi` [Stable], `codex` [Stable], `claude` [Experimental]). To integrate another coding agent or IDE without modifying Cito, create `~/.cito/plugins/<id>/plugin.json` (and an optional `index.ts` or `index.js` implementing `HarnessAdapter` from `@tefx/cito/plugin-sdk`):

```json
{
  "id": "cursor",
  "displayName": "Cursor",
  "sfSymbol": "cursorarrow.rays",
  "tint": "purple",
  "jumpUrlTemplate": "cursor://file/{cwd}",
  "cliFlag": true
}
```

---

## Server Deployment & Architecture

- **Hub Server Deployment (`cito hub`)**: See **[`deploy/hub/README.md`](deploy/hub/README.md)** for standalone CLI and single-container Docker Compose deployment across `none`, `token`, and `oidc` authentication modes.
- **Architecture, Protocol & Custom Harness SDK**: See **[`docs/DESIGN.md`](docs/DESIGN.md)** for module boundaries, `HubDirectoryIndex` NATS KV discovery, two-surface AppKit UI specifications, wire schemas, and custom `SocketBridgeAdapter` / `HarnessAdapter` integration.

---

## Development (`Build & Test`)

```sh
npm ci
npm run typecheck
npm run compile
npm test
```
