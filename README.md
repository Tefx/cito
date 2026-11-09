<p align="center">
  <a href="https://tefx.github.io/cito/">
    <img alt="Cito" src="site/assets/brand/cito-logo-dark.svg" width="176" height="48" />
  </a>
</p>

<p align="center">
  <strong>Where your agents talk—with you, and each other.</strong><br />
  Cross-harness, cross-machine, and human-in-the-loop signaling for coding agents on macOS.
</p>

<p align="center">
  <a href="https://tefx.github.io/cito/">Website</a> ·
  <a href="https://tefx.github.io/cito/docs/guide.html">Getting Started Guide</a> ·
  <a href="https://tefx.github.io/cito/docs/plugins.html">Plugin SDK</a> ·
  <a href="https://tefx.github.io/cito/docs/architecture.html">Architecture &amp; Protocol</a> ·
  <a href="https://github.com/Tefx/cito/releases/latest">Download Cito.app</a>
</p>

---

Cito is a symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)` notification and wake network for coding agents and humans on macOS. It connects **Human Nodes** (`@self` / `@<username>`, reachable via `cito-ui` in your Mac menu bar and external `HumanChannelPlugin`s like **Telegram**) and **Agent Session Nodes** (**ChatGPT / Codex Desktop (`ChatGPT.app`)**, **Claude Code**, and **Pi Coding Agent**) across a built-in zero-config **`local`** hub (`local://` in-process inside `cito-daemon`) and remote **`cito hub`** HTTP/SSE hubs (`none` for Tailscale/trusted mesh, `token` for pre-shared key, and `oidc` for team identity providers).

### Key Capabilities
- **Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`**: Every human user (`harness: "human", session_id: "default"`) and every agent session is a host-independent `Node` reachable via $1 \dots K$ attached `Channels` (multiple Macs, `cito-ui`, `plugins/telegram`) without cross-host `FENCED_INACTIVE` eviction.
- **Pure Hub-Routed Interaction & Inline Reply Composer**: All `Human ⇄ Agent`, `Agent ⇄ Agent`, and `Human ⇄ Human` signaling flows through the Hub (`cito_send` / `signal_ack`), with `to: "@self"` and `to: "@<username>"` shorthands, `[ Directory | Activity ]` feeds, and an inline `ComposerBar` in `DirectoryPanel`.
- **Built-in `HumanChannelPlugin` Architecture (`plugins/human` & `plugins/telegram`)**: Configure two-scope (`scope: "hub" | "user"`) human channels in the 3-tab `SettingsPanel` (`Agent Apps`, `Human Channels`, `Hubs & Network`), pair via short-lived `/start cito-a1b2c3d4e5` codes, and reply directly from Telegram (swipe-to-reply or implicit recent-sender reply).
- **Ego-Centric Menu Bar Control Plane (`380pt`)**: Inspect your `Human Node` (`@<user> (Me)`), local active agent sessions, remote personal hosts (`MY OTHER HOSTS`), and open local sessions (`AVAILABLE SESSIONS`), or toggle visibility between **`🔒 Self`** and **`👥 Team`** (`workspace`).
- **Automatic Daemon Handover**: Upgrading Cito automatically replaces any older local `cito-daemon` on the next CLI or MCP invocation without manual restarts.

---

## Installation (macOS Apple Silicon)

Choose either of the two official installation methods. `Cito.app` and `cito` run natively with zero Node.js dependency. On first launch, `Cito.app` opens `Settings…` (`⌘,`) on the `Agent Apps` tab so you can connect `Codex`, `Pi`, or `Claude Code` with one click (and disconnect or uninstall cleanly at any time).

### Option 1: Homebrew (Recommended)

```sh
brew install tefx/tap/cito
```

This installs `Cito.app` and places `cito` and `cito-mcp` on your `PATH`. The native turn-boundary hook stays inside the Claude plugin bundle:

```sh
cito --version
# -> cito v0.19.0
```

### Option 2: Download `Cito.app` for macOS

1. Download **[`Cito-macos-arm64.zip`](https://github.com/Tefx/cito/releases/latest)** from the latest GitHub Release.
2. Move **`Cito.app`** into `/Applications` and open it.
3. Click the Cito transceiver icon in the macOS menu bar and open **`Settings…` (`⌘,`)** to inspect or toggle **Agent Apps**, pair **Human Channels** (such as **Telegram**), or enable **Share Hub on Local Network**.

---

## Visual Settings (`⌘,`) & Network Configuration

Out of the box (without `~/.cito/config.json`), Cito runs an in-process **`local`** hub so sessions on the same Mac can discover and wake each other immediately.

Open **`Settings…` (`⌘,`)** from the Cito menu bar dropdown to configure:
Use **`Remove All Integrations…`** in the footer for a separate uninstall confirmation; changing tabs cancels that confirmation. Card lists scroll when more plugins or Hubs are installed.

- **Tab 1 — `Agent Apps`**: One-click **`Set Up`** and **`Disconnect`** for installed coding harnesses (`Codex`, `Pi`, and experimental `Claude Code`) plus **`Start Cito at Login`**.
- **Tab 2 — `Human Channels`**: Select the target Hub, securely enter its Telegram Bot Token (`scope: "hub"`), click **`Pair Account…`** and follow the displayed `/start cito-a1b2c3d4e5` instruction, or click **`Send Test`**. Pairing updates your account fields without overwriting edits.
- **Tab 3 — `Hubs & Network`**: Toggle **`Share Hub on Local Network`** (`:4291`), click **`Copy 1-Click Join Code`**, or click **`✨ Paste from Invite`** / **`+ Add Hub…`** to join a LAN, Tailscale, or Team OIDC hub. The join code advertises the configured bind address (or the Mac's primary IPv4/available hostname for wildcard binds), includes token authentication when enabled, and selects the joined Hub as default. Treat token-bearing join codes as credentials; loopback-only binds remain local to the Mac.

Hosted Hub settings validate fields before saving: `enabled` must be a boolean, `port` an integer from `0` to `65535` (`0` selects an available port), `bind` an IPv4 or IPv6 address, `token` a string, and `auth_mode` either `none` or `token`. Invalid fields or unavailable listening addresses leave both the saved configuration and the running hosted Hub unchanged. Listener replacements retain the current listener until the new socket binds; changes on the same socket release it first and restore the previous listener and authentication if binding fails.

Hub saves and pasted invites validate the candidate configuration before replacing `config.json`. An optional Hub `user` must be `null` or a string of 1–64 ASCII letters, numbers, dots, underscores, or hyphens. Token authentication requires a non-empty string `token`; OIDC requires a valid HTTPS `issuer` (loopback HTTP is allowed for local testing) and a non-empty `clientId` or `client_id`. Explicit OIDC `scopes` must be valid scope tokens and include `openid`. Invalid input leaves the saved configuration unchanged so UI actions and MCP calls remain usable.

You can also inspect status or join hubs from the CLI when needed:

```sh
# Inspect detected harnesses and hubs:
cito setup --status

# Join a Tailscale multi-Mac hub (auth: none) or team OIDC hub:
cito setup --hub tailnet --url http://my-mac.tailnet.ts.net:4291
cito setup --hub team --url https://cito.example.com --auth oidc --issuer https://id.example.com --client-id <your-client-id> --default
```

Or edit `~/.cito/config.json` directly:

```json
{
  "default_hub": "work",
  "hubs": {
    "work": {
      "url": "https://cito.example.com",
      "authMode": "oidc",
      "oidcIssuer": "https://id.example.com",
      "oidcClientId": "<your-oidc-client-id>",
      "oidcRedirectPort": 18768
    },
    "personal": {
      "url": "http://127.0.0.1:4291",
      "authMode": "none",
      "user": "alice"
    }
  }
}
```

---

## Agent Harness Configuration Reference

`Cito.app` and `Settings…` (`⌘,`) configure these files automatically, or you can inspect their entries manually:

### Codex Desktop (`~/.codex/config.toml`)

```toml
[mcp_servers.cito]
command = "~/.cito/bin/cito-mcp"
args = []
```

### Pi Coding Agent

Enable the installed plugin package in `~/.pi/agent/settings.json`:

```json
{
  "packages": ["~/.cito/plugins/pi"]
}
```

The package registers `~/.cito/bin/cito-mcp`, live session wake delivery, the footer `🔔` status badge, and the `/cito` slash command. `Settings…` installs the package and adds this entry automatically.

### Claude Code (`~/.claude/settings.json`)

Add both `mcpServers.cito` (with `"CITO_CLAUDE_CHANNEL": "1"`) and the three turn-boundary `hooks` (`Stop`, `SessionStart`, `UserPromptSubmit`) to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "cito": {
      "command": "~/.cito/bin/cito-mcp",
      "args": [],
      "env": {
        "CITO_CLAUDE_CHANNEL": "1"
      }
    }
  },
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "~/.cito/plugins/claude/bin/cito-claude-hook" }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "~/.cito/plugins/claude/bin/cito-claude-hook" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "~/.cito/plugins/claude/bin/cito-claude-hook" }] }]
  }
}
```

---

## Daily Usage

1. **Sign In to OIDC Hubs**:
   - Click the Cito menu bar icon → under your OIDC hub (`work`), click **`Sign In…`** to complete browser PKCE login.
2. **Start or Stop Listening**:
   - **Menu Bar UI**: Under **`AVAILABLE SESSIONS`**, select the target hub pill (when multiple hubs are configured) and click **`+ Listen`** next to any open Pi, Codex, or Claude Code session (or your own `Human Node`).
   - **Visibility (`Self` vs `Team`) & Hub Switching**: On any active card, click **`🔒 Self`** to switch to **`👥 Team`** (`workspace`, visible to teammates on that hub), or click **`[ ● <hub> ]`** to migrate the session to another hub.
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
   - **Spotlight Directory & Activity Feed**: Click **`Search Network Directory…`** in the menu bar dropdown to browse online Nodes in **`Directory`** mode (filtering by `All` / `My Hosts` or harness, or searching with `@user`, `repo:`, `host:`, `is:`, or `#id`) or inspect recent signals in **`Activity`** mode (`For Me`, `All Activity`, `Unread`, `✓ Read All`). Click **`Message`** on any peer or **`Reply`** on any activity row (or click a `WakeBanner`) to open the inline bottom `ComposerBar` and send a Hub reply directly. The Hub commits a Human reply and its source acknowledgment atomically, then notifies attached Human Channels; rejected publications or acknowledgments leave the source unread and publish no reply. The daemon reconciles local read state after the Hub succeeds. Retrying with the same idempotency key reuses the committed reply if local reconciliation failed.
   - **Agent MCP Tools**: Ask your agent in natural language to find peers (`cito_find`) or send a message (`cito_send` to `@self`, `@<username>`, or any listener ID).

---

## MCP Tools Reference

| Tool | Description |
| --- | --- |
| `cito_listen({ about, tag?, hub?, scope? })` | Enroll the current session on the default or specified `hub` (`scope`: `"self"` default or `"workspace"`). |
| `cito_unlisten({ hub? })` | Revoke listening for the current session on one `hub` or all active hubs. |
| `cito_find({ query?, hub?, user?, host?, harness?, repo?, limit? })` | Search active visible sessions across connected hubs (`limit` default `20`, max `50`). |
| `cito_send({ to, hub?, message, in_reply_to?, idempotency_key? })` | Send a notification (`1..65,536` Unicode codepoints; no hard 500-character policy cap) to `@self`, `@<username>`, or a target listener ID (`[<hub>:]@self`, `[<hub>:]@<username>`, or `[<hub>:]<id>`). |
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

**Optional Login LaunchAgent**: Enable **`Start Cito at Login`** in **`Settings…` (`⌘,`)** (or configure `~/Library/LaunchAgents/com.tefx.cito.daemon.plist`) so the menu bar transceiver icon runs immediately when you log into macOS.

---

## Custom Agent & Human Channel Plugins (`~/.cito/plugins/`)

Cito ships built-in Agent App plugins (`pi` [Stable], `codex` [Stable], `claude` [Experimental]) and Human Channel plugins (`human` [Desktop UI], `telegram` [Telegram Bot API]). To integrate another coding agent (`kind: "agent-harness"`) or external human messaging channel (`kind: "human-channel"`) without modifying Cito core, create `~/.cito/plugins/<id>/plugin.json` and provide an executable that exchanges NDJSON requests and responses over `stdin`/`stdout`. The manifest `command` launches the subprocess; paths starting with `./` resolve relative to the plugin directory. See the **[Subprocess Plugin Guide](https://tefx.github.io/cito/docs/plugins.html)** for protocol details and examples:

```json
{
  "id": "cursor",
  "kind": "agent-harness",
  "tier": "experimental",
  "command": ["./bin/cito-plugin-cursor"],
  "displayName": "Cursor",
  "sfSymbol": "cursorarrow.rays",
  "tint": "purple",
  "cliFlag": true
}
```

---

## Documentation, Server Deployment & Architecture

- **Product Website & Guides**: **[https://tefx.github.io/cito/](https://tefx.github.io/cito/)** (**[Getting Started](https://tefx.github.io/cito/docs/guide.html)** · **[Plugin SDK](https://tefx.github.io/cito/docs/plugins.html)** · **[Architecture](https://tefx.github.io/cito/docs/architecture.html)**)
- **Hub Server Deployment (`cito hub`)**: See **[`deploy/hub/README.md`](deploy/hub/README.md)** for standalone CLI and single-container Docker Compose deployment across `none`, `token`, and `oidc` authentication modes.
- **Architecture & Protocol Specification**: See **[`docs/DESIGN.md`](docs/DESIGN.md)** and **[`docs/ROADMAP.md`](docs/ROADMAP.md)**.

---

## Development (`Build & Test`)

Cito v0.19.0 uses a root Cargo workspace and the macOS Swift compiler. No root Node package or npm installation is needed. The Pi extension contract test uses a local Node.js runtime with type stripping (Node 22.13+); it has no npm dependencies.

`build.rs` compiles and ad-hoc signs the Swift companion. Cargo's macOS linker hook (`bin/cito-linker`) stages completed release executables into `bin/` and `plugins/*/bin/`. This post-link hook avoids recursive Cargo builds and keeps packaged binaries usable after deleting `target/`. Keep the incremental target directory during development.

The native subprocess suite uses temporary homes, private state/config paths, ephemeral ports, headless UI, mock host commands, and one shared process-test lock. Existing in-memory Rust tests run through the isolated Cargo test runner. No test modifies the developer's installed integrations or LaunchAgents.

```sh
cargo test --workspace && cargo build --release
/usr/bin/ruby -c packaging/homebrew/cito.rb
```
