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
  <a href="https://tefx.github.io/cito/docs/guide.html">User Guide</a> ·
  <a href="https://tefx.github.io/cito/docs/hosting.html">Host a Hub</a> ·
  <a href="https://tefx.github.io/cito/docs/plugins.html">Plugins</a> ·
  <a href="https://tefx.github.io/cito/docs/architecture.html">Reference</a> ·
  <a href="https://github.com/Tefx/cito/releases/latest/download/Cito-macos-arm64.zip">Download Cito.app</a>
</p>

---

Cito is a symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)` signaling and wake network for coding agents and humans on macOS. It connects **Human Nodes** (`@self` / `@<username>`, reachable via the macOS menu bar and external channels like **Telegram**) and **Agent Session Nodes** (**Pi Coding Agent**, **Codex Desktop (`ChatGPT.app` / `Codex.app`)**, and **Claude Code**) across a built-in zero-config **`local`** hub (`local://`) and remote **`cito hub`** HTTP/SSE hubs (`none` for Tailscale/trusted LAN, `token` for pre-shared key, and `oidc` for team single sign-on).

### Key Capabilities
- **Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`**: Every human user (`harness: "human", session_id: "default"`) and every coding agent session is a host-independent `Node` reachable via $1 \dots K$ attached `Channels` (multiple Macs, the macOS menu bar, and **Telegram**) without evicting other active devices.
- **Pure Hub-Routed Interaction & Inline Reply Composer**: All `Human ⇄ Agent`, `Agent ⇄ Agent`, and `Human ⇄ Human` signaling flows through the Hub. Address people with `@self` or `@<user>` and sessions with `alice/pi#8a4f` or `alice/codex#4219`; prefix non-default Hubs, such as `work:alice/pi#8f1a`. Address chips copy full searchable addresses (long labels may be truncated), search accepts the same address, and prefixes widen automatically on collision. Full canonical listener IDs remain compatible. Use the `[ Directory | Activity ]` feeds and inline reply composer.
- **Two-Scope Human Channels & 1-Click Pairing**: Configure shared Hub credentials (`scope: "hub"`) and personal bindings (`scope: "user"`) in the 3-tab `Settings…` (`⌘,`) window, pair using the live generated 10-character Base32 code (for example, `/start cito-ABCDEF2345`), and reply directly from Telegram. Plain unthreaded replies auto-route only when exactly one distinct sender delivered alerts in the preceding 7 days; otherwise reply to a specific notification.
- **Menu Bar Control Plane**: The menu popover shows **LISTENING SESSIONS**, **MY OTHER HOSTS**, and **AVAILABLE SESSIONS** for the selected Hub, with an **Inbox paused** notice when shared delivery is paused. Manage the shared inbox in **Settings → Human Channels**, independently of this Mac's agent sessions, and switch session visibility between **Self** and **Team** (`workspace`). The menu bar number counts unread signals only. A Human-only setup keeps the hollow icon until a message arrives. Semantic colors adapt to Light and Dark Mode.
- **Sender Auto-Enrollment & Replies**: `cito_send` automatically enrolls a not-yet-enrolled agent sender as a listener so replies can reach it; explicitly stopped sessions remain stopped. Telegram keeps delivery mappings for **7 days**, independently of signal payload pruning, so replies from Telegram or the Mac menu bar **Activity** stream can reach the sender while its session remains available. Pairing codes still expire after ten minutes.
- **Automatic Daemon Handover**: Upgrading Cito automatically replaces any older local daemon (`cito daemon`) on the next CLI, UI, or MCP invocation without manual restarts.

---

## Installation (macOS Apple Silicon)

Choose either of the two official installation methods. `Cito.app` and `cito` run natively with zero Node.js dependency. On first launch, `Cito.app` opens `Settings…` (`⌘,`) on the `Agent Apps` tab so you can connect `Pi Coding Agent`, `Codex`, or `Claude Code` with one click (and disconnect or uninstall cleanly at any time).

### Option 1: Homebrew (Recommended)

```sh
brew install --cask tefx/tap/cito
```

Cito is distributed as a Homebrew cask only. This installs `Cito.app` in
`/Applications` (or your configured Homebrew app directory) and places
`cito` and `cito-mcp` on your `PATH`:

```sh
cito --version
# -> cito v0.22.3
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

1. Download **[`Cito-macos-arm64.zip`](https://github.com/Tefx/cito/releases/latest/download/Cito-macos-arm64.zip)** from the latest GitHub Release.
2. Move **`Cito.app`** into `/Applications` and open it.
3. Click the Cito transceiver icon in the macOS menu bar and open **`Settings…` (`⌘,`)** to inspect or toggle **Agent Apps**, pair **Human Channels** (such as **Telegram**), or enable **Share This Mac as a Hub**.

---

## Visual Settings (`⌘,`) & Network Configuration

Out of the box (without `~/.cito/config.json`), Cito runs an in-process **`local`** hub so sessions on the same Mac can discover and wake each other immediately.

Open **`Settings…` (`⌘,`)** from the Cito menu bar dropdown to configure:

- **Tab 1 — `Agent Apps`**: One-click **`Set Up`**, **`Check & Repair`**, and **`Disconnect`** for installed coding harnesses (`Pi Coding Agent`, `Codex`, and experimental `Claude Code`).
- **Tab 2 — `Human Channels`**: Select the target Hub. The compact **Inbox Control Bar** shows **`You (@<user>)`**, active delivery paths, **`Pause Inbox`** / **`Resume Inbox`**, and **`Self`** / **`Team`** visibility on multi-user Hubs. Pause applies across attached Macs and external channels while retaining their bindings. External channels can deliver alerts when this Mac is offline, provided the selected Hub and its channel service remain online elsewhere. Channel controls below it depend on your Hub role and service readiness:
  1. **`Shared Hub Service`** (administrators only): Enter the shared credential (such as a Telegram Bot Token) and click **`Save Gateway Token`**. The active gateway collapses; when the personal account is paired too, its header becomes **`Hub Service`** without a duplicate readiness badge. **`Change Token…`** reopens it and **`Clear Token`** removes only the service credential and preserves personal bindings. A managed Hub provides the gateway for you; ask its administrator if no external channel is enabled.
  2. **`Your Personal Account (@<user>)`**: Once the gateway is ready, click **`Pair Account…`** and follow the displayed instruction, or enter your personal address, then use **`Send Test`** and **`Save`**. Paired accounts collapse to a summary; **`Edit…`** reveals the Chat ID and **`Save`** controls.
- **Tab 3 — `Hubs & Network`**: **`Share This Mac as a Hub`** starts with **`Start Sharing`** and a passive status indicator; enable it to reveal the endpoint, port (`:4291`), authentication controls, and **`Copy Invite Code`**. Use **`Join from Clipboard`** / **`Add Hub…`** to join a LAN, Tailscale, or Team OIDC hub.
- **Window Footer**: Toggle **`Start Cito at Login`** or click **`Disconnect Agent Apps…`**, then confirm with **`Disconnect Apps`** or choose **`Cancel`**. Disconnecting stops agent listeners and removes app registrations while keeping Cito running and staged files available. Selecting **`Also delete local data and Hub settings`** additionally stops human listeners, clears local activity and agent listener records, and resets the configured Hubs to the default local Hub. CLI `cito setup --uninstall [--purge-data]` remains available.
- **Guided setup and repair**: Follow the inline steps in Agent Apps and Human Channels, then use Hubs & Network when you want to share or join a Hub. App cards show reload/restart instructions; **`Repair`** restores missing CLI shims, staged plugins, and individual plugin assets. Cito rediscovers configured built-in integrations through the bundled distribution when staged files or manifests are missing, including after restarting from an installed shim. Daemon startup automatically restores bundled plugin files after an upgrade or reinstall. If staged files are deleted while Cito is running, **`Repair`** restores only the selected app and the shared CLI shims.
- **Channel lifecycle**: Saved secret fields stay blank with a saved-value placeholder. Leave them blank to retain credentials; type a new value to replace them. **`Send Test`** uses unsaved candidate values without saving them. Check the channel status for connection errors before retrying delivery. **`Pause <channel>`** retains that channel's account binding, **`Unpair`** removes it, and **`Clear Token`** removes only service credentials. Using **Pause Inbox** in **Settings → Human Channels** blocks delivery across all attached devices and channels without altering their bindings. Pairing commands expire after ten minutes; starting another pairing, cancelling, or unpairing invalidates earlier commands. Pairing preserves saved account fields and activates the channel only when all required service and account fields are configured.
- **Hub names and invites**: Leave **`Hub Name (Optional)`** blank to derive a name from the URL, or enter a name such as `Office Mac` to save it as `office-mac`. Automatically derived names receive a numeric suffix when needed; an explicit name cannot replace a different Hub. Private IPv4 addresses derive `lan`, Tailscale addresses derive `tailscale`, and other IP literals derive `remote`; hostnames derive their first slug label. Shared endpoints prefer an online Tailscale MagicDNS name when enabled, then a Tailscale IPv4 address, then the LAN address. When both are available, use **`Tailscale`** / **`Local IP`** to choose the address displayed and copied. `CITO_TAILSCALE_HOST` overrides discovery; `CITO_DISABLE_TAILSCALE_PROBE=1` disables the Tailscale CLI probe.
- **Network-scoped menu**: Select a Hub in the top header to manage that network's agent listeners, remote hosts, and available sessions. The menu starts with **LISTENING SESSIONS**; inbox controls live in Settings. A paused inbox adds only **`Inbox paused · Resume in Settings…`**, opening Human Channels for that Hub. The selection persists while the network is offline. **`Auto-Listen`** enrolls eligible, reload-ready sessions on that Hub using its default visibility; it defaults to off (`hubs.<name>.autoListen` in `config.json`). Turning it on clears previous per-session stops and connects eligible sessions after fresh discovery. Turning it off stops unpinned, auto-listened sessions while keeping pinned sessions active. **`+ Listen`** attaches a session as pinned. The **Pin** control appears when Auto-Listen is on or the session is auto-listened: an outline pin follows Auto-Listen, and a filled blue pin stays listening when Auto-Listen is turned off. Codex Auto-Listen enrolls sessions with qualifying `user_turn` or `unknown` activity within 15 minutes and retires unpinned sessions after 60 minutes idle. Pi and Claude remain enrolled while their interactive sessions stay open. Pinning does not keep a closed or disconnected app listening. **Stop** detaches only that session and keeps it stopped without changing Hub Auto-Listen. **Stop All** stops this Mac's agent listeners on the selected Hub and turns off Auto-Listen for that Hub on this Mac, even if nothing is listening. It leaves remote agent listeners and human delivery alone. Open sessions and sessions needing `/reload` remain expanded for setup guidance. Closed transcript-only sessions appear as recent groups, collapsed by default under Auto-Listen and expandable per Hub. More than four active cards scroll in a bounded list. The menu bar number shows unread messages only.
- **Visibility and personal notifications**: Trusted Hubs (`none`) default new registrations to Team visibility (`workspace`); token and Team Sign-In (OIDC) Hubs default to Self visibility (`self`). Self scope belongs to your principal across all your Macs, so your devices can communicate without exposing sessions to other users. Auto-Listen follows these defaults and preserves existing scope. The **`Self`** / **`Team`** control appears for OIDC or when other users share the Hub, with tooltips explaining who can discover and message the session. This Mac attaches automatically on enabled, authenticated Hubs unless its device attachment was explicitly stopped. **Settings → Human Channels → Pause Inbox/Resume Inbox** changes Hub-wide inbox policy; it preserves description, visibility, and attachments, including devices on other Macs. The Inbox Control Bar remains available when this Mac has no local attachment.
- **Hub lifecycle**: Edit hub settings inline without re-entering saved secrets. **`Disable`** hides a Hub from operational views and Human Channels; it remains in Hubs & Network with **`Enable`**. Disconnected enabled hubs remain in the menu with **`Connect`**; OIDC **`Switch Account`** opens the account picker. LAN sharing supports port changes and token-auth controls, retaining the previous listener if a new port cannot bind. Failed sends and configuration saves show an error without discarding newer form edits.

All editable controls in Settings, Directory search, and the composer support the native **Edit** menu: `⌘V` Paste, `⌘A` Select All, `⌘C` Copy, `⌘X` Cut, `⌘Z` Undo, and `⇧⌘Z` Redo. Password fields allow paste, selection, undo, and redo while blocking Copy and Cut. Active field editors remain attached during editing.

You can also inspect status or manage hubs from the CLI:

```sh
# Inspect detected harnesses and hubs:
cito setup --status

# Join a Tailscale multi-Mac hub (auth: none) or team OIDC hub:
cito setup --hub tailnet --url http://mac.example.com:4291
cito setup --hub work --url https://hub.example.com --default
# Optionally override the discovered loopback callback port:
cito setup --hub work --url https://hub.example.com --redirect-port 8400
```

For an OIDC Hub, URL-only setup and **Join from Clipboard** / **Add Hub…** discover authentication settings from the Hub's `GET /health`: its `oidc` object supplies `issuer`, `client_id`, and, when configured, `redirect_port`. Explicit values take precedence. If discovery reports token authentication, supply the token; if the Hub reports `none` or is unreachable, an unspecified mode falls back to `none`. Explicit OIDC setup fails if issuer or client ID remains missing.

Client `cito setup --redirect-port` and `redirectPort` accept `0..=65535`; `0` explicitly selects an ephemeral loopback port. When the port is omitted during setup, Cito uses the port advertised by the Hub. If the configuration still has no port at sign-in, Cito uses `CITO_OIDC_REDIRECT_PORT` (`1..=65535`) or selects an ephemeral port. Configure the identity provider to allow the selected callback.

Or edit `~/.cito/config.json` directly:

```json
{
  "default_hub": "work",
  "daemonPort": 4290,
  "host_hub": {
    "enabled": false,
    "port": 4291,
    "bind": "0.0.0.0",
    "auth_mode": "token",
    "token": "<strong-secret-token>"
  },
  "hubs": {
    "work": {
      "url": "https://cito.example.com",
      "authMode": "oidc",
      "issuer": "https://id.example.com",
      "clientId": "<your-oidc-client-id>",
      "redirectPort": 8400,
      "scopes": ["openid", "profile", "email"],
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

### Pi Coding Agent (`~/.pi/agent/settings.json`)

```json
{
  "packages": ["/Users/<you>/.cito/runtime/plugins/pi"]
}
```

The installed Pi package registers `/Users/<you>/.cito/bin/cito-mcp`, live session wake delivery, a footer status badge, and the `/cito` slash command automatically on package load. Before a session hook supplies context, the bridge uses a process-scoped identity.

### Codex Desktop (`~/.codex/config.toml`)

```toml
[mcp_servers.cito]
command = "/Users/<you>/.cito/bin/cito-mcp"
args = []
enabled = true
```

Codex Auto-Listen enrolls sessions active in the last 15 minutes; unpinned sessions retire after 60 minutes idle. Pinned sessions remain active until the app closes or disconnects.

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
   - **Menu Bar UI**: Under **`AVAILABLE SESSIONS`**, select the target hub pill (when multiple hubs are configured) and click **`+ Listen`** next to an open Pi, Codex, or Claude Code session. **Settings → Human Channels** separately controls shared Human delivery on the selected Hub.
   - **Visibility (`Self` vs `Team`) & Hub Switching**: On an active card, click **`🔒 Self`** to switch to **`👥 Team`** (`workspace`, visible to teammates on that Hub). The header Hub selector changes which registrations the menu displays; each Hub keeps its own enrollment and visibility. **Stop** detaches only the displayed session and keeps it stopped without changing Hub Auto-Listen. **Stop All** applies to this Mac on the selected Hub; it does not stop sessions on your other Macs or pause the human inbox. Turning **Auto-Listen** off stops unpinned auto-listened sessions; pinned sessions stay listening. Use the outline/filled **Pin** control to switch ownership while Auto-Listen is on; **`+ Listen`** attaches as pinned.
   - **Pi Slash Command**:
     - `/cito` (toggle listening on the default hub)
     - `/cito on [--hub <hub>] [about...]` (private `self` scope)
     - `/cito team [--hub <hub>] [about...]` (shared `workspace` scope)
     - `/cito off [--hub <hub>]` (stop listening on one or all hubs)
   - **Terminal CLI**:
     ```sh
     cito rename <title> [--harness <id> [--session <sid>]] [--pi[=<sid>]] [--codex[=<sid>]] [--claude[=<sid>]] [--json]
     cito status [--json]
     cito listen [--title|-t <title>] [--scope <self|workspace>] [--team] [--hub <hub>] [--tag <tag>] [--harness <id> [--session <sid>]] [--pi[=<sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json] [about...]
     cito unlisten [--all] [--hub <hub>] [--harness <id> [--session <sid>]] [--pi[=<sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json]
     cito find [--hub <hub>] [--user|-u <user>] [--host <host>] [--repo <repo>] [--query|-q <query>] [--harness <id>|--pi|--codex|--claude] [--limit <1..50>] [--json] [query...]
     cito send --to <target> --message|-m <text> [--hub <hub>] [--reply-to|--in-reply-to <signal_id>] [--idempotency-key <key>] [--harness <id> [--session <sid>]] [--pi[=<sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json]
     cito inbox pause|resume [--hub <hub>] [--scope <self|workspace>] [--team] [--about <text>]
     cito inbox [--hub <hub>] [--scope <self|workspace>] [--team] [--about <text>]
     cito listen --harness human [--hub <hub>]
     cito unlisten --harness human [--hub <hub>]
     cito inbox [--hub <hub>] [--unread|--peek|--no-ack] [--ack-all] [--ack <id>]... [--limit <1..1000>] [--harness <id> [--session <sid>]] [--pi[=<sid>]] [--codex[=<thread_id>]] [--claude[=<session_id>]] [--json]
     ```
   - **Session names**: Click the pencil beside an active agent session title; Return saves and Escape cancels. Names contain 1–128 characters and persist across discovery, heartbeats, and daemon restarts on all enrolled hubs. `cito listen --title` names the calling session; positional `about` also becomes its name when the default was a folder/home basename. `cito unlisten --all` applies the same local stop across configured Hubs; add `--hub <name>` to limit it to one Hub, as the menu does.
   - **Service and setup commands**: `cito daemon` runs the local service; `cito hub` runs a shared HTTP/SSE Hub; `cito mcp-stdio` serves MCP over stdio; `cito --version` / `cito -v` reports the release. `cito setup` supports `--status`, `--dry-run`, `--force`, `--enable` / `--disable <harness>`, `--build-app[=<path>]`, `--enable-hub` / `--disable-hub` / `--remove-hub <name>`, `--share-hub` / `--no-share-hub`, `--share-port <port>`, `--hub <name> --url <url> [--user <user>]`, `--token <token>`, `--login-item` / `--no-login-item`, and `--json`.
   - **Setup shortcuts**: `cito setup --invite <code-or-url>` joins another Mac's shared hub; self-invites are rejected. `cito setup --print-invite` prints the invite when sharing is enabled. `cito setup --disconnect-all [--purge-data]` removes agent integrations without stopping the daemon.
3. **Discover & Message Agents**:
   - **Spotlight Directory & Activity Feed**: Use the side-by-side **`Directory…`** and **`Activity…`** menu buttons to choose a view directly. Directory browses online Nodes (filter by `All Nodes` / `Mine` or `All Apps`, or search with `@user`, `repo:`, `host:`, `is:`, or `#id`). Address chips copy the full address, even when a long label is truncated: `@alice` for a person or `alice/agent#abcd` for a session; a non-default Hub adds its name, such as `work:alice/agent#abcd`. The Directory opens with `⌘K`. Activity filters before paging (`For Me`, `All Activity`, `Unread`), expands selected messages, and copies the full message even when the preview is shortened. `Read All` and `Clear…` obey the current Activity filter and search query. **For Me** affects only messages addressed to your Human Node. `Clear…` opens an inline confirmation showing the affected count; `Cancel` or Escape leaves history unchanged. Confirming `Clear` hides history while preserving pending agent delivery and retry checkpoints; it does not acknowledge pending agent work. Click **`Message`** on a peer or **`Reply`** on activity addressed to your Human Node to open the inline composer. Incoming messages increment the unread menu bar badge and appear in Activity; opening Reply marks that message read. External channels can notify you when you are away from the Mac. **Settings → Human Channels → Pause Inbox** pauses Human delivery on that Hub across all attached Macs and external channels. `cito inbox pause/resume` changes the same shared policy; `cito listen/unlisten --harness human` only attaches or detaches this Mac. Resume preserves the inbox description unless `--about` is explicitly supplied. Scope and description can also be changed without pausing or resuming; policy flags cannot be combined with inbox read/ack flags or session selectors.
   - **Agent MCP Tools**: Ask your agent in natural language to find peers (`cito_find`) or send a message (`cito_send` to `@self`, `@<username>`, a copied readable address, or any canonical listener ID). Sending from an unenrolled agent session registers its return address automatically; explicitly stopped sessions remain stopped. The username `self` is reserved. `cito_listen({about, title?})` sets only the calling session's name. `cito_status({hub?})` reports that session's registration, address, title, scope, unread count, and hub connection metadata without credentials. Setup, hub/channel administration, stopping all sessions, and renaming other sessions remain CLI/GUI-only.

---

## MCP Tools Reference

| Tool | Description |
| --- | --- |
| `cito_listen({ about, title?, tag?, hub?, scope? })` | Enroll the calling agent session on the default or specified `hub` (`scope`: `"self"` or `"workspace"`; omission follows the Hub default). Human enrollment is forbidden over MCP. |
| `cito_status({ hub? })` | Inspect the calling session's registration, address, title, visibility, unread count, and Hub health without exposing credentials. |
| `cito_unlisten({ hub? })` | Revoke listening for the calling agent session on one `hub` or all active hubs. Human enrollment is forbidden over MCP. |
| `cito_find({ query?, hub?, user?, host?, harness?, repo?, limit? })` | Search active visible sessions across connected hubs (`limit` default `20`, max `50`). |
| `cito_send({ to, hub?, message, in_reply_to?, idempotency_key? })` | Send a notification (`1..65,536` Unicode code points) to `@self`, `@<username>`, a readable session address (`[<hub>:]<user>/<harness>#<prefix>`), or a canonical listener ID. Ambiguous short prefixes require a longer prefix or full ID. |
| `cito_inbox({ hub?, ack_ids?, ack_all?, limit? })` | Read and acknowledge unread signals (`limit` default `50`, max `1000`; `ack_all` defaults to `true` when `ack_ids` is omitted). |

---

## Environment Variables & Optional Auto-Start

These are production environment controls read by the native runtime, macOS UI, or bundled plugins. CLI flags override their standalone Hub counterparts. Client settings normally live in `config.json`; variables must be present in the process that reads them (restarting an existing daemon is necessary to change its environment). The tables include local developer overrides; leave diagnostic controls unset for normal use.

### Hub Server

| Variable | Default / behavior |
| --- | --- |
| `CITO_HUB_PORT` | `4291`; standalone `--port` (`0` requests an ephemeral port). |
| `CITO_HUB_NAME` | Optional advertised name; standalone `--name` overrides it. Compose forwards this variable. |
| `CITO_HUB_BIND` | `127.0.0.1` with `none`, `0.0.0.0` with `token`/`oidc`; standalone `--bind`, an IP address. |
| `CITO_HUB_DATA_DIR` | `~/.cito/hub`; standalone `--data-dir`. Docker uses `/data`. |
| `CITO_AUTH_MODE` | `none`; standalone `--auth`: `none`, `token`, or `oidc`. Client config validation also recognizes this variable; configure per-Hub client authentication in `config.json`. |
| `CITO_HUB_TOKEN` | Required shared bearer credential in token mode; standalone `--token`. |
| `CITO_OIDC_ISSUER` | Required HTTPS issuer in OIDC mode; standalone `--issuer`. |
| `CITO_OIDC_CLIENT_ID` | Required public client ID in OIDC mode; standalone `--client-id`. |
| `CITO_OIDC_REDIRECT_PORT` | Optional advertised OIDC callback port, `1..=65535`; standalone `--redirect-port` takes precedence. |
| `CITO_OIDC_ADMIN_USERS` | Empty by default; comma-separated verified owner usernames or OIDC subjects allowed to administer shared channel fields. |
| `CITO_JWKS_COOLDOWN_MS` | `60000`; minimum interval between JWKS refresh attempts, including after an unknown key ID or an invalid signature with a cached key. |
| `CITO_PLUGIN_<ID>_<KEY>` | No default; seeds declared Hub-scoped channel fields at startup, including `CITO_PLUGIN_TELEGRAM_BOT_TOKEN`. |

### Client & Daemon

| Variable | Default / behavior |
| --- | --- |
| `CITO_STATE_DIR` | `~/.cito`; local state, sockets, staged runtime, and plugins. |
| `CITO_CONFIG_PATH` | `<state-dir>/config.json`; explicit multi-Hub configuration path. |
| `CITO_HUB` | Overrides the configured default Hub name (otherwise `local`). |
| `CITO_HUB_URL` | Overrides the active Hub URL; use `local://`, `http://`, or `https://`. |
| `CITO_NATS_URL` | Compatibility alias for `CITO_HUB_URL`, used only when the latter is absent; no NATS broker is involved. |
| `CITO_USER` | Local username override; otherwise configured user or OS username. OIDC identity comes from verified tokens. |
| `CITO_HOST_NAME` | Overrides the host display name; otherwise the system hostname. |
| `CITO_DAEMON_PORT` | `4290`; loopback daemon HTTP port, overrides `daemonPort` in config. |
| `CITO_OIDC_REDIRECT_PORT` | Loopback callback fallback when `redirectPort` is absent; integer `1..=65535`. An explicit `redirectPort: 0` selects an ephemeral port. |
| `CITO_BROWSER_BIN` | Browser opener used for OIDC login; platform default (`open` on macOS). |
| `CITO_TAILSCALE_HOST` | Overrides advertised Tailscale host discovery for Hub invitations. |
| `CITO_DISABLE_TAILSCALE_PROBE` | Set `1` to skip the Tailscale CLI probe. |
| `CITO_DISABLE_UI` | Set `1` to suppress automatic macOS companion launch. |
| `CITO_UI_BINARY` | Overrides the discovered macOS companion executable path. |
| `CITO_UI_HEADLESS` | Set `1` for headless UI diagnostics and a private pasteboard; also disables Tailscale probing. Leave unset for everyday use. |
| `CITO_CODESIGN_IDENTITY` | Signing identity for `cito setup --build-app`; falls back to `APPLE_SIGNING_IDENTITY`, then ad-hoc signing for local development. |

### Agent & Channel Plugins

| Variable | Default / behavior |
| --- | --- |
| `CITO_PLUGIN_PATH` | Colon-separated plugin-root search directories, highest priority, left-to-right. |
| `CITO_PLUGIN_DIR` | Legacy user plugin-root override, below `CITO_PLUGIN_PATH`. |
| `CITO_BUILTIN_PLUGINS_DIR` | Overrides bundled plugin-root discovery; Docker sets `/app/plugins`. |
| `CITO_PI_HOME` | `~/.pi/agent`; Pi configuration and session root. |
| `CITO_CODEX_HOME` | `~/.codex`; Codex configuration and thread database root. |
| `CITO_CLAUDE_HOME` | `~/.claude`; Claude configuration and transcript root. |
| `CITO_CODEX_BIN` | Overrides Codex executable discovery for wake delivery. |
| `CITO_OPEN_BIN` | Overrides `/usr/bin/open` for the Codex macOS open action. |
| `CITO_CLAUDE_CHANNEL` | Set `1` in Claude's MCP environment to enable its streaming wake sidecar; setup writes this automatically. |
| `CITO_ENDPOINT_PID` | Endpoint attribution hint set by MCP launch shims; live ancestry checks determine the enclosing harness. Do not set a global unrelated PID. |
| `CITO_PI_PROCESS_DISCOVERY` | `0` disables process-only hints; `1` permits process-only resolution when the bridge is not required. Normal discovery still prefers live bridges. |
| `CITO_PI_REQUIRE_BRIDGE` | Set `1` to require a live Pi bridge even with process-only resolution enabled. |
| `CITO_PI_ALLOW_HEADLESS_DISCOVERY` | Set `1` only in controlled synthetic fixtures to bypass the interactive-TTY discovery gate; leave unset for normal sessions. |

For plugin protocol fields and operation budgets, see [Plugin Authoring](https://tefx.github.io/cito/docs/plugins.html). For Node identity, signaling, and CLI/MCP interfaces, see [Architecture & Reference](https://tefx.github.io/cito/docs/architecture.html).

**Optional Login LaunchAgent**: Enable **Start Cito at Login** in **Settings…** (`⌘,`) or run `cito setup --login-item`.

---

## Custom Agent & Human Channel Plugins (`~/.cito/plugins/`)

Cito ships built-in Agent App plugins (`pi` [Stable], `codex` [Stable], `claude` [Experimental]) and external Human Channel plugins (`telegram` [Telegram Bot API]). The native macOS companion is a built-in app helper, separate from plugins. To integrate another coding agent (`kind: "agent-harness"`) or external human messaging channel (`kind: "human-channel"`), create `~/.cito/plugins/<id>/plugin.json` and provide an executable that exchanges NDJSON requests and responses over `stdin`/`stdout`. Agent setup requests omit the optional `env` field; setup plugins use their own process environment for values such as `HOME`. The pairing card provides a link to the service and can collapse manual account fields. The UI auto-copies the correlated pairing command; **Cancel** invalidates the pending code. See the **[Subprocess Plugin Guide](https://tefx.github.io/cito/docs/plugins.html)** for protocol details and examples:

Discovery chooses one plugin per ID in this order, highest priority first:

1. **`CITO_PLUGIN_PATH`** roots, left-to-right.
2. **`${CITO_STATE_DIR}/plugins/<id>/`** (default **`~/.cito/plugins/<id>/`**), reserved exclusively for user-authored plugins and same-ID overrides of built-ins. Remove an override to fall back to its bundled default automatically.
3. **Bundled defaults**, including `Cito.app/Contents/Resources/plugins/<id>/`, with internal CLI staging under **`~/.cito/runtime/plugins/<id>/`**. Staging never overwrites user plugins.

The macOS companion is packaged as the nested `Cito.app/Contents/Helpers/Cito.app` with its own executable and app metadata. Development builds also resolve `bin/cito-ui-darwin-arm64`; `CITO_UI_BINARY` overrides discovery for testing. During upgrade, Cito recognizes an old staged copy in `~/.cito/plugins/<id>/` only when its version differs from the bundled manifest and every other manifest field matches. A different command, field, or other customization in the manifest preserves the override. Cleanup can unlink an alias of the bundled plugin without deleting its target.

Both Homebrew command links resolve through the App launcher. Invoking `cito-mcp` without arguments starts MCP stdio; invoking `cito` without arguments starts the daemon.

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

## Documentation by Audience

- **For Users**: [User Guide](https://tefx.github.io/cito/docs/guide.html) — installation, one-click Agent Apps setup, daily Menu Bar / Directory / Activity workflows, personal pairing, and joining a Hub.
- **For Hub Operators**: [Host a Hub](https://tefx.github.io/cito/docs/hosting.html) — standalone server, Docker, Linux services, authentication, channels, and backups.
- **For Developers**: [Plugin Authoring](https://tefx.github.io/cito/docs/plugins.html) and [Architecture & Reference](https://tefx.github.io/cito/docs/architecture.html).

### Host a Hub

The [deployment guide](deploy/hub/README.md) covers Linux prerequisites, standalone and Compose token setup without putting secrets in shell history, and a `systemd` unit. Every proxy must stream SSE without buffering and preserve `Authorization`, `Content-Type`, and the public `Host`. For Nginx, set `client_max_body_size 8m` and `proxy_buffering off`; for Caddy, set `flush_interval -1`. Publish the Hub on `:4291`, never the loopback daemon on `:4290`.

For OIDC PKCE, register a public client and a loopback callback such as `http://127.0.0.1:8400/callback`, configure `redirectPort: 8400` on clients, and set `CITO_OIDC_ADMIN_USERS` on the server for shared channel administration. Create a Telegram bot with `@BotFather`, seed its shared credential with `CITO_PLUGIN_TELEGRAM_BOT_TOKEN` (the `CITO_PLUGIN_<ID>_<KEY>` startup mechanism), then let each user pair a private chat. One Bot Token permits one poller; a duplicate produces `409 Conflict`. Back up SQLite WAL databases with SQLite's backup API, or stop the Hub before snapshotting the whole data directory/volume; restore only while stopped and never mix a snapshot with stale WAL files. See the [operator guide](https://tefx.github.io/cito/docs/hosting.html) for complete procedures.

### Repository References

- **Product Website & Guides**: **[https://tefx.github.io/cito/](https://tefx.github.io/cito/)** (**[User Guide](https://tefx.github.io/cito/docs/guide.html)** · **[Host a Hub](https://tefx.github.io/cito/docs/hosting.html)** · **[Plugins](https://tefx.github.io/cito/docs/plugins.html)** · **[Reference](https://tefx.github.io/cito/docs/architecture.html)**)
- **Hub Server Deployment (`cito hub`)**: See **[`deploy/hub/README.md`](deploy/hub/README.md)** for standalone CLI, Docker Compose, Linux systemd, reverse-proxy SSE/Host requirements, OIDC PKCE, Telegram service setup, SQLite WAL backup/restore, and the full environment reference.
- **Architecture & Protocol Specification**: See **[`docs/DESIGN.md`](docs/DESIGN.md)**.
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
cargo test --workspace --locked -- --test-threads=2
RUSTFLAGS='-D warnings' cargo build --release --locked
/usr/bin/ruby -c packaging/homebrew/cito.rb
```

To retain 2× Retina AppKit snapshots in both Light and Dark appearances:

```sh
CITO_TEST_SNAPSHOT_DIR=/tmp/cito-snapshots cargo test --workspace --locked -- --test-threads=1
```

The snapshot matrix covers the menu, Directory and Activity (including expanded rows and Composer), all Settings tabs and Hub forms, the uninstall overlay, and inline Clear confirmation. Every exported state passes the six real-view checks for truncation, overlap, clipping, contrast, jargon, and address consistency before and after rendering. The separate extreme-length stress test requires detected truncation and complete cleaned text/tooltips; it exports no snapshots.

Direct pushes to `main` run CI and retain an ad-hoc candidate tied to the exact
source commit. Publishing requires an annotated `vMAJOR.MINOR.PATCH` tag. The
release workflow verifies the ZIP checksum, bundle signature, Apple Team ID, and
stapled notarization ticket before running the archived CLI. It verifies a
public Homebrew install and uninstall before updating the tap. See
[CI and releases](docs/RELEASING.md) for the full gates and retry rules.
