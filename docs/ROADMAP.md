# Cito Product & Architecture Roadmap

Cito's staged evolution from `v0.11.3` is organized into **six `0.x` stages (`Stage 0` through `Stage 5`)** (preserving `0.x` SemVer progression rather than jumping prematurely to `v1.0.0`):
- **Stage 0 (`v0.12.0` — Completed)**: Unified Plugin Architecture, Built-in Plugin Extraction (`plugins/{pi,codex,claude}`) & Data-Driven macOS UI
- **Stage 1 (`v0.13.0` — Completed)**: NATS-Free Native `HubEngine` (`local://` In-Process + `cito hub` HTTP/SSE Server), Control-Plane Guidance Without Hard Message Caps & `cito setup` CLI Onboarding
- **Stage 2 (`v0.14.0` — Completed)**: Rust Hot-Path Multi-Call Binary (`cito-native`), Surface 3 AppKit `Settings…` (`⌘,`) Window, In-Process LAN Hub Sharing (`:4291`) & `< 5 MB` `Cito.app` Packaging
- **Stage 3 (`v0.15.0`)**: Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `HumanChannelPlugin` (`cito-ui` + `plugins/telegram`), `Directory | Activity` Feed & Inline Hub Reply Composer
- **Stage 4 (`v0.16.0`)**: Project Website, Visual Architecture Showcase, Plugin SDK Authoring Guide & Onboarding Docs
- **Stage 5 (`v0.17.0`)**: Public Distribution, Developer ID Code Signing, Apple Notarization & Release Automation

---

## Architectural Guardrails (Applicable to All Stages)

1. **Zero Concrete Agent or Channel Hardcoding in Core (`src/` & `src/macos/cito-ui.swift`)**:
   - Core modules (`src/shared/`, `src/hub/`, `src/mcp/`, `src/setup/`, `src/ui/`, `src/adapters/`, `src/macos/cito-ui.swift`) MUST NOT contain hardcoded branches, configuration fields, CLI flag names, or visual tables for specific agent apps (`pi`, `codex`, `claude`) or specific external human channels (`telegram`, `webhook`, `slack`, `teams`, `email`, `feishu`, `discord`).
   - All concrete Agent Apps (`kind: "agent"`) and Human Channels (`kind: "human"`) live inside `plugins/<id>/` (bundled) or `${CITO_STATE_DIR}/plugins/<id>/` (external user plugins) and are rendered dynamically from `plugin.json`.
2. **Symmetric `Node (Human | Agent Session) ↔ Hub ↔ Channels (1..K)` Ontology**:
   - Every addressable participant on a Hub is a symmetric **`Node`** (`Human Node` `@user` or `Agent Session Node`), identified without `host_id`.
   - Every physical device or messaging integration (`cito-ui` on Mac A/B, `Telegram`, local process bridges) is a **`Channel`** ($1 \dots K$ per `Node`) connecting that `Node` to the `Hub`.
   - All `Node ⇄ Node` interaction flows strictly through the Hub's regular signaling channel (`cito_send` / `signal_ack`), with zero out-of-band external window-jump bypasses (`jump_url` removed).
3. **Mechanism, Not Policy (Control-Plane Recommendation Without Hard Message Caps)**:
   - MCP tool descriptions recommend Cito primarily as a lightweight control plane while pairing with external channels for extended multi-turn discussions or large artifacts, without enforcing a hard 500-character policy cap (`sanitizeSignalMessage` allows `1..65,536` codepoints).

---

## Stage 0 (`v0.12.0` — Completed): Unified Plugin Architecture, Built-in Plugin Extraction & Data-Driven UI

Completed in Phase `v16` (`1f46ab2`, tag `v0.12.0`).

---

## Stage 1 (`v0.13.0` — Completed): NATS-Free Native `HubEngine`, Control-Plane Guidance & `cito setup` CLI

Completed in Phase `v17` (`e5f92a4`, tag `v0.13.0`).

---

## Stage 2 (`v0.14.0` — Completed): Rust Hot-Path Binary (`cito-native`), Surface 3 `Settings…` Window, In-Process LAN Hub Sharing & `< 5 MB` `Cito.app`

Completed in Phase `v18` (`2ade0a7`, tag `v0.14.0`).

---

## Stage 3 (`v0.15.0`): Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `HumanChannelPlugin` (`cito-ui` + `plugins/telegram`), `Directory | Activity` Feed & Inline Hub Reply Composer

1. **Symmetric `Node ↔ Channels (1..K)` Refactoring & `@self` / `@<username>` Addressing**:
   - Strip `host_id` from `listenerId` so each user has one `Human Node` (`harness: "human", session_id: "default"`) and each agent session has one `Agent Session Node` per Hub, with $1 \dots K$ attached `Channels`.
   - Separate stream cursor ack (`consumer_ack`) from business read ack (`signal_ack`) and add atomic `signal_claim_dispatch` for `wake_exclusive` channels.
   - Support `to: "@self"` and `to: "@<username>"` in `cito_send`, and allow per-Hub `Listen / Unlisten` and `🔒 Self / 👥 Team` scope control for the user's `Human Node`.
2. **Generic `HumanChannelPlugin` Architecture & Built-in `plugins/telegram`**:
   - Support `kind: "human"` plugins with two-scope `fields` (`scope: "hub" | "user"`) and generic 6-char pairing codes (`pairing.supported: true`) managed over Hub `/v1/rpc` (`human_channels_list`, `human_channels_configure`, `human_channels_test`, `human_channel_pair_start`) with persistent restart-idempotent delivery ledger (`hub_channel_deliveries`).
   - Ship built-in `plugins/human` (desktop `cito-ui`) and `plugins/telegram` (zero-dependency `fetch` Bot API `sendMessage` + `getUpdates` long-polling with `/start <code>` pairing, swipe-to-reply, and implicit recent-sender reply).
3. **Pure Hub Interaction UI (`Directory | Activity` + Inline `ComposerBar` + 3-Tab `SettingsPanel`, Zero `jump_url`)**:
   - Remove `jumpUrlTemplate` / `jump_url` / `open:<scoped_id>` across the codebase; clicking `WakeBanner` opens `DirectoryPanel` (`Activity` tab) with the bottom `ComposerBar` focused to reply via Hub.
   - Upgrade `DirectoryPanel` with `[ Directory | Activity ]` mode switching (`For Me`, `All Activity`, `Unread`, `✓ Read All`) and the bottom 1-line `ComposerBar` (`[ 💬 Message ]` and `[ ↩ Reply ]`).
   - Upgrade `SettingsPanel` (`⌘,`) to 3 tabs (`Agent Apps`, `Human Channels`, `Hubs & Network`).

---

## Stage 4 (`v0.16.0`): Project Website, Visual Architecture Showcase & Plugin SDK Authoring Guide

1. **Official Landing Page & Visual Showcase**:
   - Interactive showcase of the macOS AppKit surfaces (`cardMenu`, `DirectoryPanel` `Directory & Activity`, `SettingsPanel` `Agent Apps / Human Channels / Hubs`, and `WakeBanner`), `Cito.app` download + 1-line install.
2. **Plugin SDK (`@tefx/cito/plugin-sdk`) Guide for Both Agent Apps (`kind: "agent"`) & Human Channels (`kind: "human"`)**:
   - Step-by-step documentation for authoring custom Agent App plugins and custom Human Channel plugins (`~/.cito/plugins/<id>/`).
3. **Self-Hosted `cito hub` & Team Onboarding Guide**:
   - Documentation for single-Mac (`local`), cross-Mac LAN/Tailscale sharing (`Share Hub on Local Network`), and team OIDC + shared Bot deployment (`deploy/hub/`).

---

## Stage 5 (`v0.17.0`): Public Distribution, Developer ID Signing, Notarization & Release Automation

1. **Apple Developer ID Code Signing & Notarization**:
   - Sign `Cito.app` (`Contents/MacOS/Cito`, `Contents/Helpers/cito-ui`, `Contents/Helpers/cito-native`) with Developer ID Application certificate and Hardened Runtime, and notarize via `xcrun notarytool` + `stapler`.
2. **Public Distribution Channels**:
   - GitHub Releases (`Cito-darwin-arm64.dmg` / `.zip`), Homebrew Cask/Tap formula, and automated release checks.
