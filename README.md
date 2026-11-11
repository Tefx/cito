<p align="center">
  <a href="https://tefx.github.io/cito/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="site/assets/brand/cito-logo-dark.svg" />
      <source media="(prefers-color-scheme: light)" srcset="site/assets/brand/cito-logo-light.svg" />
      <img alt="Cito" src="site/assets/brand/cito-logo-dark.svg" width="157" height="48" />
    </picture>
  </a>
</p>

<p align="center">
  <strong>Where your agents talk—with you, and each other.</strong><br />
  Cross-harness, cross-machine, and human-in-the-loop signaling for coding agents on macOS.
</p>

<p align="center">
  <a href="https://tefx.github.io/cito/">Website</a> ·
  <a href="https://tefx.github.io/cito/docs/guide.html">Getting Started Guide</a> ·
  <a href="https://tefx.github.io/cito/docs/plugins.html">Subprocess Plugins</a> ·
  <a href="https://tefx.github.io/cito/docs/architecture.html">Architecture &amp; Protocol</a> ·
  <a href="https://github.com/Tefx/cito/releases/latest">Download Cito.app</a>
</p>

---

Cito is a symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)` signaling and wake network for coding agents and humans on macOS. It connects **Human Nodes** (`@self` / `@<username>`, reachable via the macOS menu bar and external channels like **Telegram**) and **Agent Session Nodes** (**Codex Desktop (`ChatGPT.app` / `Codex.app`)**, **Claude Code**, and **Pi Coding Agent**) across a built-in zero-config **`local`** hub (`local://`) and remote **`cito hub`** HTTP/SSE hubs (`none` for Tailscale/trusted LAN, `token` for pre-shared key, and `oidc` for team single sign-on).

### Key Capabilities
- **Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`**: Every human user (`harness: "human", session_id: "default"`) and every coding agent session is a host-independent `Node` reachable via $1 \dots K$ attached `Channels` (multiple Macs, the macOS menu bar, and **Telegram**) without evicting other active devices.
- **Pure Hub-Routed Interaction & Inline Reply Composer**: All `Human ⇄ Agent`, `Agent ⇄ Agent`, and `Human ⇄ Human` signaling flows through the Hub. Address people with `@self` or `@<user>` and sessions with `alice/codex#4219` or `alice/pi#8f1a`; prefix non-default Hubs, such as `work:alice/pi#8f1a`. Address chips display exactly what they copy, search accepts the same address, and prefixes widen automatically on collision. Full canonical listener IDs remain compatible. Use the `[ Directory | Activity ]` feeds and inline reply composer.
- **Two-Scope Human Channels & 1-Click Pairing**: Configure shared Hub credentials (`scope: "hub"`) and personal bindings (`scope: "user"`) in the 3-tab `Settings…` (`⌘,`) window, pair via 10-character Base32 `/start cito-a1b2c3d4e5` codes, and reply directly from Telegram (swipe-to-reply or implicit recent-sender reply).
- **Menu Bar Control Plane**: Inspect your `Human Node (@user)`, local active agent sessions, remote personal hosts (`MY OTHER HOSTS`), and open local sessions (`AVAILABLE SESSIONS`), or toggle visibility between **`🔒 Self`** and **`👥 Team`** (`workspace`). The icon counts agent sessions only and shows unread signals separately; a Human-only idle setup has no numeric badge. Semantic status colors adapt to Light and Dark Mode.
- **Sender Auto-Enrollment & Replies**: `cito_send` automatically enrolls a not-yet-enrolled agent sender as a listener so replies can reach it; explicitly stopped sessions remain stopped. Telegram keeps delivery mappings for **7 days**, independently of signal payload pruning, so replies from Telegram or the Mac menu bar **Activity** stream can reach the sender while its session remains available. Pairing codes still expire after ten minutes.
- **Automatic Daemon Handover**: Upgrading Cito automatically replaces any older local `cito-daemon` on the next CLI, UI, or MCP invocation without manual restarts.

---

## Installation (macOS Apple Silicon)

Choose either of the two official installation methods. `Cito.app` and `cito` run natively with zero Node.js dependency. On first launch, `Cito.app` opens `Settings…` (`⌘,`) on the `Agent Apps` tab so you can connect `Codex`, `Pi Coding Agent`, or `Claude Code` with one click (and disconnect or uninstall cleanly at any time).

### Option 1: Homebrew (Recommended)

```sh
brew install --cask tefx/tap/cito
```

Cito is distributed as a Homebrew cask only. This installs `Cito.app` in
`/Applications` (or your configured Homebrew app directory) and places
`cito` and `cito-mcp` on your `PATH`:

```sh
cito --version
# -> cito v0.21.0
```

To uninstall the app and both command symlinks while keeping your settings:

```sh
brew uninstall --cask tefx/tap/cito
```

If you previously installed the legacy formula, remove it before installing the
cask to avoid conflicting command links:

```sh
brew uninstall --formula tefx/tap/cito
brew update
brew install --cask tefx/tap/cito
```

Use `--zap` only if you also want Homebrew to delete Cito's user data.

### Option 2: Download `Cito.app` for macOS

1. Download **[`Cito-macos-arm64.zip`](https://github.com/Tefx/cito/releases/latest)** from the latest GitHub Release.
2. Move **`Cito.app`** into `/Applications` and open it.
3. Click the Cito transceiver icon in the macOS menu bar and open **`Settings…` (`⌘,`)** to inspect or toggle **Agent Apps**, pair **Human Channels** (such as **Telegram**), or enable **Share Hub on Local Network**.

---

## Visual Settings (`⌘,`) & Network Configuration

Out of the box (without `~/.cito/config.json`), Cito runs an in-process **`local`** hub so sessions on the same Mac can discover and wake each other immediately.

Open **`Settings…` (`⌘,`)** from the Cito menu bar dropdown to configure:

- **Tab 1 — `Agent Apps`**: One-click **`Set Up`**, **`Re-check`**, and **`Disconnect`** for installed coding harnesses (`Codex`, `Pi Coding Agent`, and experimental `Claude Code`).
- **Tab 2 — `Human Channels`**: Select the target Hub and toggle **`Mac Desktop Banner`**. Channel cards have two strata:
  1. **`1. HUB GATEWAY · ONCE PER HUB`**: Enter the shared credential (such as a Telegram Bot Token) and click **`Save Gateway Token`**. The active gateway collapses; **`Change Token…`** reopens it and **`Clear Token`** removes it. A managed Hub can provide the gateway for you.
  2. **`2. Your Personal Account (@<user>)`**: Once the gateway is ready, click **`Pair Account…`** and follow the displayed instruction, or enter your personal address, then use **`Send Test`** and **`Save`**. **`Skip for Now`** leaves pairing optional and makes no Hub changes.
- **Tab 3 — `Hubs & Network`**: **`Share Hub on Local Network`** collapses when **`Off`**; enable it to reveal the endpoint, port (`:4291`), authentication controls, and **`Copy 1-Click Join Code`**. Use **`Paste Invite`** / **`Add Hub…`** to join a LAN, Tailscale, or Team OIDC hub.
- **Window Footer**: Toggle **`Start Cito at Login`** or click **`Remove All Integrations…`**. Choose **`Disconnect All`** to stop listeners and remove app registrations while keeping Cito running and staged files available, or **`Confirm Uninstall`** to remove Cito and quit (optionally deleting local data).
- **Guided setup and repair**: Follow the inline steps in Agent Apps and Human Channels, then use Hubs & Network when you want to share or join a Hub. App cards show reload/restart instructions; **`Repair`** restores missing CLI shims, staged plugins, and individual plugin assets. Cito rediscovers configured built-in integrations through the bundled distribution when staged files or manifests are missing, including after restarting from an installed shim. Damaged integrations remain visible with **`Repair`** until you restore them.
- **Channel lifecycle**: Saved secret fields stay blank with a saved-value placeholder. Leave them blank to retain credentials; type a new value to replace them. **`Send Test`** uses unsaved candidate values without saving them. Schema lookup and connection failures also appear as test results. Test details survive status refreshes even before the channel is configured; a failed test keeps an **`Error`** badge until another test replaces it or a channel action changes configuration. **`Pause`** retains your account binding, **`Unpair`** removes your binding, and **`Clear Token`** removes service credentials. Pairing commands expire after ten minutes; starting another pairing, cancelling, or unpairing invalidates earlier commands. Pairing preserves saved account fields and activates the channel only when all required service and account fields are configured.
- **Hub lifecycle**: Edit hub settings inline without re-entering saved secrets. Disconnected hubs remain in the menu with **`Connect`**; OIDC **`Switch Account`** opens the account picker. LAN sharing supports port changes and token-auth controls, retaining the previous listener if a new port cannot bind. Message sends and port saves track their own acknowledgements, so delayed failures from an earlier attempt cannot settle a retry.

All editable controls in Settings, Directory search, and the composer support the native **Edit** menu: `⌘V` Paste, `⌘A` Select All, `⌘C` Copy, `⌘X` Cut, `⌘Z` Undo, and `⇧⌘Z` Redo. Password fields allow paste, selection, undo, and redo while blocking Copy and Cut. Active field editors remain attached during editing.

You can also inspect status or manage hubs from the CLI:

```sh
# Inspect detected harnesses and hubs:
cito setup --status

# Join a Tailscale multi-Mac hub (auth: none) or team OIDC hub:
cito setup --hub tailnet --url http://my-mac.tailnet.ts.net:4291
cito setup --hub work --url https://cito.example.com --auth oidc --issuer https://id.example.com --client-id <your-client-id> --default
```

Or edit `~/.cito/config.json` directly:

```json
{
  "default_hub": "work",
  "daemonPort": 4290,
  "desktopBannerEnabled": true,
  "host_hub": {
    "enabled": false,
    "port": 4291,
    "bind": "0.0.0.0",
    "auth_mode": "token",
    "token": ""
  },
  "hubs": {
    "work": {
      "url": "https://cito.example.com",
      "authMode": "oidc",
      "oidcIssuer": "https://id.example.com",
      "oidcClientId": "<your-oidc-client-id>",
      "oidcRedirectPort": 18768,
      "oidcScopes": ["openid", "profile", "email"],
      "defaultScope": "self",
      "enabled": true
    },
    "personal": {
      "url": "http://127.0.0.1:4291",
      "authMode": "none",
      "user": "alice",
      "enabled": true
    }
  }
}
```

---

## Agent Harness Configuration Reference

`Cito.app` and `Settings…` (`⌘,`) manage these files automatically (writing the expanded absolute path for your macOS account), or you can inspect their entries manually:

### Codex Desktop (`~/.codex/config.toml`)

```toml
[mcp_servers.cito]
command = "/Users/<you>/.cito/bin/cito-mcp"
args = []
```

### Pi Coding Agent (`~/.pi/agent/settings.json`)

```json
{
  "packages": ["/Users/<you>/.cito/runtime/plugins/pi"]
}
```

The installed Pi package registers `/Users/<you>/.cito/bin/cito-mcp`, live session wake delivery, the footer `🔔` status badge, and the `/cito` slash command automatically on package load. Before a session hook supplies context, the bridge uses the latest session file for the working directory (or a process-scoped identity) for discovery, wake delivery, and badge updates. When context arrives for that same session, it displays the stored badge and retains delivery deduplication.

### Claude Code (`~/.claude/settings.json`)

```json
{
  "mcpServers": {
    "cito": {
      "command": "/Users/<you>/.cito/bin/cito-mcp",
      "args": [],
      "env": {
        "CITO_CLAUDE_CHANNEL": "1"
      }
    }
  },
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "/Users/<you>/.cito/runtime/plugins/claude/bin/cito-claude-hook" }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "/Users/<you>/.cito/runtime/plugins/claude/bin/cito-claude-hook" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "/Users/<you>/.cito/runtime/plugins/claude/bin/cito-claude-hook" }] }]
  }
}
```

---

## Daily Usage & CLI Reference

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
     cito listen [--scope <self|workspace>] [--team] [--hub <hub>] [--tag <tag>] [--harness <id> [--session <sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json] [about...]
     cito unlisten [--hub <hub>] [--harness <id> [--session <sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json]
     cito find [--hub <hub>] [--user|-u <user>] [--host <host>] [--repo <repo>] [--query|-q <query>] [--harness <id>|--codex|--claude] [--limit <1..50>] [--json] [query...]
     cito send --to <target> --message|-m <text> [--hub <hub>] [--reply-to|--in-reply-to <signal_id>] [--idempotency-key <key>] [--harness <id> [--session <sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json]
     cito inbox [--hub <hub>] [--unread|--peek|--no-ack] [--ack-all] [--ack <id>...] [--limit <1..1000>] [--harness <id> [--session <sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json]
     ```
3. **Discover & Message Agents**:
   - **Spotlight Directory & Activity Feed**: Use the side-by-side **`Directory…`** and **`Activity…`** menu buttons to choose a view directly. Directory browses online Nodes (filter by `All` / `My Macs` or `All Apps`, or search with `@user`, `repo:`, `host:`, `is:`, or `#id`). Address chips show exactly what they copy: `@alice` for a person or `alice/agent#abcd` for a session; a non-default Hub adds its name, such as `work:alice/agent#abcd`. The Directory opens with `⌘K`. Activity filters before paging (`For Me`, `All Activity`, `Unread`), expands selected messages, and copies the full message even when the preview is shortened. `✓ Read All` acknowledges messages; `Clear` acknowledges unread messages and removes local history. Click **`Message`** on a peer or **`Reply`** on activity addressed to your Human Node to open the inline composer. A Human-targeted wake banner opens a reply; an agent-targeted banner opens Activity without a composer.
   - **Agent MCP Tools**: Ask your agent in natural language to find peers (`cito_find`) or send a message (`cito_send` to `@self`, `@<username>`, a copied readable address, or any canonical listener ID). Sending from an unenrolled agent session registers its return address automatically; explicitly stopped sessions remain stopped. The username `self` is reserved.

---

## MCP Tools Reference

| Tool | Description |
| --- | --- |
| `cito_listen({ about, tag?, hub?, scope? })` | Enroll the current session on the default or specified `hub` (`scope`: `"self"` default or `"workspace"`). |
| `cito_unlisten({ hub? })` | Revoke listening for the current session on one `hub` or all active hubs. |
| `cito_find({ query?, hub?, user?, host?, harness?, repo?, limit? })` | Search active visible sessions across connected hubs (`limit` default `20`, max `50`). |
| `cito_send({ to, hub?, message, in_reply_to?, idempotency_key? })` | Send a notification (`1..65,536` Unicode code points) to `@self`, `@<username>`, a readable session address (`[<hub>:]<user>/<harness>#<prefix>`), or a canonical listener ID. Ambiguous short prefixes require a longer prefix or full ID. |
| `cito_inbox({ hub?, ack_ids?, ack_all?, limit? })` | Read and acknowledge unread signals (`limit` default `50`, max `1000`; `ack_all` defaults to `true` when `ack_ids` is omitted). |

---

## Environment Variables & Optional Auto-Start

| Variable | Default / Purpose |
| --- | --- |
| `CITO_STATE_DIR` | `~/.cito` (SQLite state, local Hub database, OIDC token cache, Unix sockets, CLI shims, and installed plugins). |
| `CITO_CONFIG_PATH` | `~/.cito/config.json` (multi-hub configuration path). |
| `CITO_HUB` | Overrides `default_hub` from `config.json`. |
| `CITO_USER` / `CITO_HOST_NAME` | Overrides the default local username or host display name. |
| `CITO_DAEMON_PORT` | `4290` (loopback HTTP MCP & `/health` port). |
| `CITO_CLAUDE_CHANNEL` | `0`; set to `1` in Claude Code's MCP server `env` to enable the Claude wake channel. |
| `CITO_PI_HOME` / `CITO_CODEX_HOME` / `CITO_CLAUDE_HOME` | `~/.pi/agent` / `~/.codex` / `~/.claude` (local harness configuration directories). |
| `CITO_PLUGIN_PATH` | Colon-separated plugin-root directories, searched left-to-right before user overrides and bundled defaults. |

**Optional Login LaunchAgent**: Enable **`Start Cito at Login`** in **`Settings…` (`⌘,`)** (or run `cito setup --login-item`) so the menu bar transceiver icon runs immediately when you log into macOS.

---

## Custom Agent & Human Channel Plugins (`~/.cito/plugins/`)

Cito ships built-in Agent App plugins (`pi` [Stable], `codex` [Stable], `claude` [Experimental]) and Human Channel plugins (`human` [Desktop UI], `telegram` [Telegram Bot API]). To integrate another coding agent (`kind: "agent-harness"`) or external human messaging channel (`kind: "human-channel"`), create `~/.cito/plugins/<id>/plugin.json` and provide an executable that exchanges NDJSON requests and responses over `stdin`/`stdout`. See the **[Subprocess Plugin Guide](https://tefx.github.io/cito/docs/plugins.html)** for protocol details and examples:

Discovery chooses one plugin per ID in this order, highest priority first:

1. **`CITO_PLUGIN_PATH`** roots, left-to-right.
2. **`${CITO_STATE_DIR}/plugins/<id>/`** (default **`~/.cito/plugins/<id>/`**), reserved exclusively for user-authored plugins and same-ID overrides of built-ins. Remove an override to fall back to its bundled default automatically.
3. **Bundled defaults**, including `Cito.app/Contents/Resources/plugins/<id>/`, with internal CLI staging under **`~/.cito/runtime/plugins/<id>/`**. Staging never overwrites user plugins.

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

- **Product Website & Guides**: **[https://tefx.github.io/cito/](https://tefx.github.io/cito/)** (**[Getting Started](https://tefx.github.io/cito/docs/guide.html)** · **[Subprocess Plugins](https://tefx.github.io/cito/docs/plugins.html)** · **[Architecture](https://tefx.github.io/cito/docs/architecture.html)**)
- **Hub Server Deployment (`cito hub`)**: See **[`deploy/hub/README.md`](deploy/hub/README.md)** for standalone CLI and single-container Docker Compose deployment across `none`, `token`, and `oidc` authentication modes.
- **Architecture & Protocol Specification**: See **[`docs/DESIGN.md`](docs/DESIGN.md)** and **[`docs/ROADMAP.md`](docs/ROADMAP.md)**.
- **CI, Signing & Releases**: See **[`docs/RELEASING.md`](docs/RELEASING.md)** for main-based development, Developer ID credentials, version tags, and automatic Homebrew updates.

---

## Development (`Build & Test`)

Cito is a Rust + Swift workspace with zero root npm dependencies. The Rust
toolchain is pinned in `rust-toolchain.toml`. Contributor checks use Python 3.11+
and Node.js 24+ for release tooling and the dependency-free Pi extension tests;
installed applications do not require either runtime.

```sh
python3 packaging/release.py check
python3 -m unittest discover -s packaging -p 'test_*.py' -v
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=4
cargo build --release --locked
/usr/bin/ruby -c packaging/homebrew/cito.rb
```

To retain 2× Retina AppKit snapshots in both Light and Dark appearances:

```sh
CITO_TEST_SNAPSHOT_DIR=/tmp/cito-snapshots cargo test --workspace --locked -- --test-threads=1
```

The snapshot matrix covers the menu, Directory and Activity (including expanded rows and Composer), all Settings tabs and Hub forms, the uninstall overlay, and human/agent wake banners. Every exported state passes the six real-view checks for truncation, overlap, clipping, contrast, jargon, and address consistency before and after rendering. The separate extreme-length stress test requires detected truncation and complete cleaned text/tooltips; it exports no snapshots.

Direct pushes to `main` run CI and retain an ad-hoc candidate tied to the source
commit. Publishing requires an annotated `vMAJOR.MINOR.PATCH` tag; the release
workflow signs, notarizes, and verifies the final App before publishing and
install-testing the generated Homebrew Cask. The Cask in this repository is a
template; its version and SHA-256 are filled from the final release artifact.
See [CI and releases](docs/RELEASING.md) before preparing or retrying a release.
