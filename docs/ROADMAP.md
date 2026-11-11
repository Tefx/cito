# Cito Product & Architecture Roadmap

Cito's evolution from `v0.11.3` through `v0.21.2` is organized into **fourteen `0.x` stages (`Stage 0` through `Stage 13`)**:
- **Stage 0 (`v0.12.0` — Completed)**: Unified Plugin Architecture, Built-in Plugin Extraction (`plugins/{pi,codex,claude}`) & Data-Driven macOS UI
- **Stage 1 (`v0.13.0` — Completed)**: NATS-Free Native `HubEngine` (`local://` In-Process + `cito hub` HTTP/SSE Server), Control-Plane Guidance Without Hard Message Caps & `cito setup` CLI Onboarding
- **Stage 2 (`v0.14.0` — Completed)**: Rust Hot-Path Multi-Call Binary (`cito-native`), Surface 3 AppKit `Settings…` (`⌘,`) Window, In-Process LAN Hub Sharing (`:4291`) & `< 5 MB` `Cito.app` Packaging
- **Stage 3 (`v0.15.0` — Completed)**: Symmetric `Node (Human | Agent) ↔ Hub ↔ Channels (1..K)`, `HumanChannelPlugin` (`cito-ui` + `plugins/telegram`), `Directory | Activity` Feed & Inline Hub Reply Composer
- **Stage 4 (`v0.16.0` — Completed)**: Project Website, Visual Architecture Showcase, Plugin SDK Authoring Guide & Onboarding Docs
- **Stage 5 (`v0.17.0` — Completed)**: Initial Native Packaging, First-Launch Consent & Surgical Disconnect/Uninstall
- **Stage 6 (`v0.18.0` — Completed)**: Pure Rust Core (`libsqlite3`), Symmetric Out-of-Process Plugin Protocol (`stdin`/`stdout` NDJSON), Relocation of `cito-claude-hook` to `plugins/claude/`, Standard Pi Package Mode, Retirement of Duplicate `src/*.ts`, Complete `F1`–`F64` Remediation & Live GitHub Pages / Release / Homebrew Tap Closure
- **Stage 7 (`v0.19.0` — Completed / Phase `v23`)**: Pure Rust Workspace Consolidation, Complete Removal of Root Node/Vitest Scaffolding (`package.json`, `package-lock.json`, `tsconfig.json`, `vitest.config.ts`, `node_modules/`, `tests/**/*.ts`) & Unified `cargo test` Native Test Suite
- **Stage 8 (`v0.20.0` — Completed / Phase `v24`)**: Full Lifecycle UX State Machine & Reconfiguration Contract (`L1`–`L17`)
- **Stage 9 (`v0.20.1` — Completed / Phase `v25`)**: Theory-Grounded UI/UX Redesign, Two-Stratum Human Channel Architecture (`Hub Gateway` vs. `Personal Human Node`), Configured-Only Session Discovery, Live `Pi` Process Discovery & Multimodal Visual E2E Verification (`U1`–`U11`)
- **Stage 10 (`v0.20.2` — Phase `v26`)**: Root-Cause Equivalence-Class Closure (`R1`–`R5`), True Progressive Disclosure, Full Mechanical Geometry Invariants (`truncated_labels`, `overlapping_siblings`, `clipped_views`), Closed-Loop Error Surfacing, Bridge Readiness & `Daemon<->UI` Closed-Loop Verification

- **Stage 11 (`v0.20.3` — Phase `v28`)**: Light/Dark semantic contrast, agent-only menu counter, normalized Human Node cards, Human-only Activity replies, and explicit Directory / Activity menu entries (`C1`–`C5`).
- **Stage 12 (`v0.21.1` — Phases `v29` / `v30`)**: Holistic UI/UX and cross-layer pathway remediation (`D01`–`D20`).
- **Stage 13 (`v0.21.2` — Phase `v31`)**: Declarative bar/strip layout, single-authority personal notification lifecycle, and bounded external delivery retries (`M1`–`M5`).

---

## Stage 13 (`v0.21.2` — Phase `v31`)

Declarative bar and pill-strip layout replaces conditional coordinate tables. A single lease reconciler preserves explicit personal notification pauses across channel attachment and restart. External delivery retains confirmed multipart progress, bounds retries with exponential backoff, and quarantines uncertain effects. SSE uses read-idle deadlines, while transient agent wake failures recover after their retry deadline. See `DESIGN.md` Section 15.

## Stage 12 (`v0.21.1` — Phases `v29` / `v30`)

The `v0.21.1` patch resolves Homebrew App launcher symlinks, preserves `cito-mcp` no-argument stdio dispatch, normalizes macOS temporary-directory aliases in Cask smoke checks, and migrates legacy user-root staged built-ins without removing custom overrides. The internal `human` companion remains bundled-only.

[DESIGN §14](DESIGN.md#14-v29--stage-12-v0211-holistic-uiux--cross-layer-pathway-remediation-d01d20) defines readable Node addresses, sender auto-enrollment and reply closure, presentation-only error formatting, progressive Settings layouts, correlated Directory queries, logical-send idempotency, Activity filtering/clearing/full-message copy, interactive session discovery, and four-surface Light/Dark inspection. Canonical IDs and SQLite schema versions remain unchanged. Release publication and archive checksum synchronization remain separate distribution work.

## Stage 11 (`v0.20.3` — Phase `v28`)

[DESIGN §13](DESIGN.md#13-v28--stage-11-v0203-lightdark-appearance-contrast-citopalette-agent-only-menu-bar-counter-human-node-card-title-normalization-agent-vs-human-reply-boundary--explicit-menu-bar-activity-entry-c1c5) defines the binding `C1`–`C5` contract:
- **C1**: Count distinct agent sessions only; show Human readiness separately and show unread signals independently of session count.
- **C2**: Normalize blank Human enrollment titles in daemon and shared registration, preserving compact `64pt` Human cards through Disable → Enable and retaining custom titles/descriptions.
- **C3**: Resolve semantic text, icons, and chip colors dynamically for `aqua` / `darkAqua`; enforce WCAG AA contrast using the real AppKit inspection tree and capture 2x Retina PNGs under deterministic Light/Dark offscreen backdrops.
- **C4**: Route Human-targeted wake clicks to reply; route agent-targeted wake clicks to Activity without a composer. Hide and guard replies on activity that is not For Me.
- **C5**: Place `Directory…` and `Activity…` side by side in the menu and always open the explicitly selected mode.

Release metadata, plugin manifests, the Homebrew formula, README, and website align at `0.20.3`. Validation includes real daemon/UI re-enrollment, both appearances, existing geometry invariants, workspace tests, and a warning-free release build. Product version, phase ID, and SQLite schema remain independent.

## Stage 10 (`v0.20.2` — Completed / Phase `v26`)

Stage 10 closes the five root-cause equivalence classes (`R1`–`R5`) specified in [DESIGN §12](DESIGN.md#12-stage-10-v0202--phase-v26-root-cause-equivalence-class-closure-r1r5-true-progressive-disclosure-full-mechanical-geometry-invariants-closed-loop-error-surfacing-bridge-readiness--daemon-ui-closed-loop-verification):
- **`R1` — True Two-Stratum Progressive Disclosure, Independent Scope Saving & Prerequisite Gating**: Scope-filtered `collectValues(scopeFilter: "hub" | "user")`, dedicated `"save_hub_service"` RPC routing (`params["hub_config"]` only, without creating/resuming a personal row in `hub_human_channels`), progressive collapse of `stratum_hub:<id>` (`36pt`/`50pt` summary bar when `gatewayActive` with `edit_hub_service:<id>` `"Change Token…"` and `clear_channel_service:<id>`), and genuine `MenuControl.isEnabled` prerequisite gating on Step 2 (`stratum_user:<id>`) when `!gatewayActive`.
- **`R2` — Full Mechanical Geometry Invariants & Coordinate Fixes (`G1`–`G8`)**: Upgrade `inspectMenu()` to compute and assert `truncated_labels == []`, `overlapping_siblings == []`, and `clipped_views == []` across `cardMenu`, `SettingsPanel`, and `DirectoryPanel`, resolving all 8 coordinate and clipping defects (`G1`–`G8`).
- **`R3` — Closed-Loop UI Actions, Visible Error Surfacing & Invite/Form Validation**: Track `last_hub_errors` on `DaemonService`, emit `{"op": "error", ...}` frames on action failures, unify `"paste_hub_invite"` (`JSON`, `http(s)://`, `cito setup --hub ...`), and surface errors across `add_hub_error`, `settings_error_banner` (with `36pt` scroll viewport shift), `menu_error_row`, and `composerErrorLabel` while keeping forms open until validated and acknowledged.
- **`R4` — Process Discovery vs. Bridge Socket Readiness, Vendor-Neutral `needs_reload` UX & Bounded Exponential Retry Backoff**: Pre-`session_start` `identify` fallback in `plugins/pi/cito-bridge.ts`, `needs_reload: true` candidate reporting for unbridged processes in `plugins/pi/native.rs` (blocking unbridged enrollment in `resolve_caller`/`probe`), vendor-neutral `46pt` `needs_reload` row with `reload:<harness>:<sid>` (`"/reload"`) in `cardMenu`, and bounded exponential badge retry backoff (`250ms..15s`) in `SessionActivity`.
- **`R5` — Real `DaemonService <-> Headless cito-ui` Closed-Loop Integration Suite & Multimodal Retina PNG Verification**: Wire `cito-ui` stdout actions directly into a real `DaemonService` and feed real daemon response frames back into `cito-ui` stdin, asserting all 3 geometry invariants on every step and capturing 2x Retina PNG snapshots across all core states.

---

## Stage 9 (`v0.20.1` — Completed / Phase `v25`)

Stage 9 implements the holistic UI/UX and session-discovery contracts in [DESIGN §11](DESIGN.md#11-stage-9-v0201--phase-v25-theory-grounded-uiux-redesign-two-stratum-human-channel-architecture-hub-gateway-vs-personal-human-node-holistic-layout-grid--live-visual-e2e-verification-u1u11):
- **U1–U3**: Configured-only session discovery (`configured == true` gate in `daemon.rs` `snapshot()`), self-healing `pi-pid-<pid>.sock` extension bridge plus live `Pi` process & `.jsonl` session discovery (`CITO_PI_PROCESS_DISCOVERY` hermetic sandbox isolation and `pid-<p>` collision fallback), and per-Hub `human_paused_card` (`listen_human`) re-enrollment in `cardMenu`.
- **U4–U6 & U11**: Vendor-neutral `"Human Channels"` terminology (`zero` concrete agent/channel hardcoding in `src/macos/cito-ui.swift`), user-controlled `?` `onboarding_banner` across all 3 tabs with non-overlapping `300pt` tab scroll viewport, `skip_channel:<id>` (`"Skip for Now"`), `"TARGET HUB:"` label, and Two-Stratum Human Channel cards separating `stratum_hub:<id>` (`1. HUB GATEWAY · ONCE PER HUB`, `scope: "hub"`) from `stratum_user:<id>` (`2. YOUR PERSONAL NODE · EACH HUMAN ON HUB`, `scope: "user"`).
- **U7–U10**: Right-aligned `CONNECTED HUBS` row controls (`x = 468`) with full untruncated button/badge frames, visible/copyable LAN Hub token (`copy_host_hub_token`) with validated port `"✓ Saved"` feedback, persistent field labels and protocol descriptions in `add_hub_form` (`edit_hub:local` vs. remote Hub), mechanical `truncated_labels` inspection, and `test_render_png` 8-state Retina PNG visual gate.

The native regression suite covers these contracts in isolated homes and state directories, including all eight core Retina snapshots, scrolled personal-node cards, and Directory/Activity rendering. Snapshot generation restores live menu ownership and geometry after rendering. Independent visual acceptance and release publication (including the published archive checksum, Homebrew tap, and website deployment) remain separate follow-up gates.

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
