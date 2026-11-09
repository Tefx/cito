# Cito Product & Architecture Roadmap

Cito's evolution from `v0.11.3` through `v0.20.0` is organized into **nine `0.x` stages (`Stage 0` through `Stage 8`)**:
- **Stage 0 (`v0.12.0` — Completed)**: Unified Plugin Architecture, Built-in Plugin Extraction (`plugins/{pi,codex,claude}`) & Data-Driven macOS UI
- **Stage 1 (`v0.13.0` — Completed)**: NATS-Free Native `HubEngine` (`local://` In-Process + `cito hub` HTTP/SSE Server), Control-Plane Guidance Without Hard Message Caps & `cito setup` CLI Onboarding
- **Stage 2 (`v0.14.0` — Completed)**: Rust Hot-Path Multi-Call Binary (`cito-native`), Surface 3 AppKit `Settings…` (`⌘,`) Window, In-Process LAN Hub Sharing (`:4291`) & `< 5 MB` `Cito.app` Packaging
- **Stage 3 (`v0.15.0` — Completed)**: Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `HumanChannelPlugin` (`cito-ui` + `plugins/telegram`), `Directory | Activity` Feed & Inline Hub Reply Composer
- **Stage 4 (`v0.16.0` — Completed)**: Project Website, Visual Architecture Showcase, Plugin SDK Authoring Guide & Onboarding Docs
- **Stage 5 (`v0.17.0` — Completed)**: Initial Native Packaging, First-Launch Consent & Surgical Disconnect/Uninstall
- **Stage 6 (`v0.18.0` — Completed)**: Pure Rust Core (`libsqlite3`), Symmetric Out-of-Process Plugin Protocol (`stdin`/`stdout` NDJSON), Relocation of `cito-claude-hook` to `plugins/claude/`, Standard Pi Package Mode, Retirement of Duplicate `src/*.ts`, Complete `F1`–`F64` Remediation & Live GitHub Pages / Release / Homebrew Tap Closure
- **Stage 7 (`v0.19.0` — Completed / Phase `v23`)**: Pure Rust Workspace Consolidation, Complete Removal of Root Node/Vitest Scaffolding (`package.json`, `package-lock.json`, `tsconfig.json`, `vitest.config.ts`, `node_modules/`, `tests/**/*.ts`) & Unified `cargo test` Native Test Suite

---

## Stage 8 (`v0.20.0` — Completed / Phase `v24`)

Stage 8 closes the lifecycle contracts in [DESIGN §10](DESIGN.md#10-stage-8-v0200--phase-v24-full-lifecycle-ux-state-machine--reconfiguration-contract-l1l17):
- **L1–L8**: Declarative channel guidance, independent field visibility and secret redaction, retain/replace/explicit-clear patches, non-persisting tests, readiness-gated attachments, pause/unpair/service reset, superseded/cancelled pairing, selected-Hub refresh, token-rotation process/cursor reset, and remote-only channel schemas.
- **L9–L13**: Guided setup, app-specific lifecycle hints, missing-file repair, listener/cache cleanup on disconnect, and separate Disconnect All versus full uninstall actions.
- **L14–L17**: Interactive OIDC account switching, disabled-Hub sign-in, inline Hub edits, visible disconnected Hubs, LAN port/auth controls with bind-failure rollback, and actionable Directory/Activity empty states.

Verified on the target Mac using isolated HOME and Cito state directories. Release publication, the published archive checksum, the Homebrew tap, and live website deployment remain separate distribution tasks.

## Architectural Guardrails (Applicable to All Stages)

1. **Zero Concrete Agent or Channel Hardcoding in Core (`crates/cito-native/src/core/` & `src/macos/cito-ui.swift`)**:
   - Core modules (`crates/cito-native/src/core/` and `src/macos/cito-ui.swift`) MUST NOT contain hardcoded branches, configuration fields, CLI flag names, or visual tables for specific agent apps (`pi`, `codex`, `claude`) or specific external human channels (`telegram`, `webhook`, `slack`, `teams`, `email`, `feishu`, `discord`).
   - Mechanical gate: `grep -rnE -i '\b(pi|codex|claude|telegram|webhook|slack|teams|email|discord|feishu)\b' crates/cito-native/src/core/ src/macos/` MUST return `0` matches.
   - All concrete Agent Apps (`kind: "agent-harness"`) and Human Channels (`kind: "human-channel"`) live inside `plugins/<id>/` (bundled) or `${CITO_STATE_DIR}/plugins/<id>/` (external user plugins) and execute as out-of-process subprocesses declared by `plugin.json`.
2. **Symmetric `Node (Human | Agent Session) ↔ Hub ↔ Channels (1..K)` Ontology**:
   - Every addressable participant on a Hub is a symmetric **`Node`** (`Human Node` `@user` with `session_id: "default"` or `Agent Session Node`), identified without `host_id`.
   - Every physical device or messaging integration (`cito-ui` on Mac A/B, `Telegram`, local process bridges) is a **`Channel`** ($1 \dots K$ per `Node`) connecting that `Node` to the `Hub`.
   - All `Node ⇄ Node` interaction flows strictly through the Hub's regular signaling channel (`cito_send` / `signal_ack`), with zero out-of-band external window-jump bypasses (`jump_url` removed).
3. **Mechanism, Not Policy (Control-Plane Recommendation Without Hard Message Caps)**:
   - MCP tool descriptions recommend Cito primarily as a lightweight control plane while pairing with external channels for extended multi-turn discussions or large artifacts, without enforcing a hard 500-character policy cap (`sanitize_signal_message` allows `1..65,536` code points).

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

## Stage 3 (`v0.15.0` — Completed): Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `HumanChannelPlugin` (`cito-ui` + `plugins/telegram`), `Directory | Activity` Feed & Inline Hub Reply Composer

1. **Symmetric `Node ↔ Channels (1..K)` Refactoring & `@self` / `@<username>` Addressing**:
   - Strip `host_id` from `listenerId` so each user has one `Human Node` (`harness: "human", session_id: "default"`) and each agent session has one `Agent Session Node` per Hub, with $1 \dots K$ attached `Channels`.
   - Separate stream cursor ack (`consumer_ack`) from business read ack (`signal_ack`) and add atomic `signal_claim_dispatch` for `wake_exclusive` channels.
   - Support `to: "@self"` and `to: "@<username>"` in `cito_send`, and allow per-Hub `Listen / Unlisten` and `🔒 Self / 👥 Team` scope control for the user's `Human Node`.
2. **Generic `HumanChannelPlugin` Architecture & Built-in `plugins/telegram`**:
   - Support `kind: "human-channel"` plugins with two-scope `fields` (`scope: "hub" | "user"`) and 10-character Base32 (48-bit) pairing codes (`cito-a1b2c3d4e5`, `pairing.supported: true`) managed over Hub `/v1/rpc` (`human_channels_list`, `human_channels_configure`, `human_channels_test`, `human_channel_pair_start`) with persistent restart-idempotent delivery ledger (`hub_channel_deliveries`).
   - Ship built-in `plugins/human` (desktop `cito-ui`) and `plugins/telegram` (Bot API `sendMessage` + `getUpdates` long-polling with `/start <code>` pairing, swipe-to-reply, and implicit recent-sender reply).
3. **Pure Hub Interaction UI (`Directory | Activity` + Inline `ComposerBar` + 3-Tab `SettingsPanel`, Zero `jump_url`)**:
   - Remove `jumpUrlTemplate` / `jump_url` / `open:<scoped_id>` across the codebase; clicking `WakeBanner` opens `DirectoryPanel` (`Activity` tab) with the bottom `ComposerBar` focused to reply via Hub.
   - Upgrade `DirectoryPanel` with `[ Directory | Activity ]` mode switching (`For Me`, `All Activity`, `Unread`, `✓ Read All`) and the bottom 1-line `ComposerBar` (`[ 💬 Message ]` and `[ ↩ Reply ]`).
   - Upgrade `SettingsPanel` (`⌘,`) to 3 tabs (`Agent Apps`, `Human Channels`, `Hubs & Network`).

---

## Stage 4 (`v0.16.0` — Completed): Project Website, Visual Architecture Showcase & Plugin SDK Authoring Guide

1. **Official Landing Page & Visual Showcase**:
   - Interactive showcase of the macOS AppKit surfaces (`cardMenu`, `DirectoryPanel` `Directory & Activity`, `SettingsPanel` `Agent Apps / Human Channels / Hubs`, and `WakeBanner`), `Cito.app` download + 1-line install.
2. **Subprocess Plugin Protocol Guide for Both Agent Apps (`kind: "agent-harness"`) & Human Channels (`kind: "human-channel"`)**:
   - Step-by-step documentation for authoring custom Agent App plugins and custom Human Channel plugins (`~/.cito/plugins/<id>/`) in any language over `stdin`/`stdout` NDJSON.
3. **Self-Hosted `cito hub` & Team Onboarding Guide**:
   - Documentation for single-Mac (`local`), cross-Mac LAN/Tailscale sharing (`Share Hub on Local Network`), and team OIDC + shared Bot deployment (`deploy/hub/`).

---

## Stage 5 & Stage 6 (`v0.18.0` — Completed): Pure Rust Core (`libsqlite3`), Symmetric Subprocess Plugin Protocol, Relocation of `cito-claude-hook` to `plugins/claude/`, Standard Pi Package Mode, Retirement of `src/*.ts` & Complete `F1`–`F64` Remediation

1. **Pure Rust Core (`libsqlite3`) & Complete Retirement of Duplicate TypeScript Engine (`src/*.ts`)**:
   - `bin/cito` and `bin/cito-mcp` run `daemon`, `hub`, `mcp-stdio`, `setup`, and all CLI subcommands natively in Rust backed by system `libsqlite3` (`state.sqlite` and `hub.sqlite`), real OIDC JWKS (`RS256`/`ES256`) verification, `principal_id` RLS, SSE auto-reconnect, atomic `signal_claim_dispatch`, and all 27 UI actions (`F1, F6–F19, F24, F30, F31, F37, F39–F41, F52, F54, F55, F60, F61, F63, F64`).
   - Retire and delete the duplicate TypeScript core (`src/mcp/`, `src/hub/`, `src/setup/`, `src/ui/`, `src/shared/`, `src/adapters/`, `src/auth-callout/`) and `bin/loader.js`; run all 33 Vitest test suites against the compiled native binaries via `tests/support/native-harness.ts` (`F43, F50`).
2. **Symmetric Out-of-Process Plugin Protocol (`stdin`/`stdout` NDJSON) & Relocation of `cito-claude-hook`**:
   - **Zero concrete agent or channel code in Core**: both Agent Harness plugins (`kind: "agent-harness"`) and Human Channel plugins (`kind: "human-channel"`, including `plugins/human` `cito-ui`, `plugins/telegram`, and external `~/.cito/plugins/<id>/`) execute as subprocesses communicating over `stdin`/`stdout` NDJSON (`stdin EOF` instant self-exit within `< 100 ms` prevents orphaned channel pollers).
   - **Pi (`plugins/pi/`)**: uses standard Pi Package mode (`~/.cito/plugins/pi` with `package.json` `"pi": { "extensions": ["./cito-bridge.ts"] }` registered in `~/.pi/agent/settings.json` `"packages"`, calling `pi.registerMcpServer("cito", ...)` on package init, `F2, F29, F58`).
   - **Claude Code (`plugins/claude/`)**: `cito-claude-hook` is relocated out of Core/top-level `bin/` into `plugins/claude/bin/cito-claude-hook` with `flock` queue serialization and torn-line isolation (`F35, F36, F44, F62`).
3. **UI/UX Polish, High-Contrast Brand Assets & Live GitHub Pages / Release / Homebrew Tap (`v0.18.0`)**:
   - Resolve all `cito-ui.swift`, `README.md`, `site/assets/brand/`, and `site/` issues (`F3, F5, F26, F27, F32, F42, F47–F51, F57`), enable and verify live GitHub Pages (`https://tefx.github.io/cito/`), publish GitHub Release `v0.18.0` (`Cito-macos-arm64.zip` on `macos-15`), and sync the verified SHA-256 to `packaging/homebrew/cito.rb` and `Tefx/homebrew-tap` (`F4`).

---

## Stage 7 (`v0.19.0` — Completed / Phase `v23`): Pure Rust Workspace Consolidation, Removal of Root Node/Vitest Scaffolding & `cargo test` Unification

1. **Remove Root Node.js / Vitest Scaffolding**:
   - Delete root `package.json`, `package-lock.json`, `tsconfig.json`, `vitest.config.ts`, `node_modules/`, and `tests/**/*.ts` (`tests/support/native-harness.ts`), keeping only `plugins/pi/{package.json, cito-bridge.ts}` for the Pi host.
2. **Promote Root `Cargo.toml` Workspace & Migrate Full E2E/Integration Suite to `cargo test`**:
   - Establish root `Cargo.toml` workspace and migrate all behavioral, protocol, SQLite, plugin subprocess, headless `cito-ui`, website, and `F1`–`F64` regression tests into native Rust tests (`crates/cito-native/tests/*.rs`) executed via `cargo test --workspace`. Run `cargo test --workspace && cargo build --release` from the root; the Swift build script and post-link staging hook retain signed package binaries independently of `target/`.
3. **Pure-Rust CI & `v0.19.0` Release**:
   - Remove `actions/setup-node` / `npm` steps from `.github/workflows/release.yml`, bump version to `v0.19.0`, run iterative `code-hygiene` convergence, and publish `v0.19.0` to GitHub Releases and `Tefx/homebrew-tap`.
