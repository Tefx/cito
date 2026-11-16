# Cito Brand & Website Artwork

Cito uses a clean cobalt identity and real AppKit screenshots. Assets must describe the shipped four-surface interface: menu bar, Directory, Activity, and Settings. Use generic users, repositories, and network examples.

## App icon

- **Master:** `site/assets/art/app-icon-master.png`, 1024 × 1024.
- **Design:** A satin cobalt squircle with the gradient `#1B6EF3 → #0057BF → #0048A8`. Center a white open C-shaped transceiver arc and signal core, with a pale `#E0F2FE` outbound wave. Keep the mark legible at 16 pixels and its contrast at least 4.5:1 against the background. No metal bezel, screws, amber light, or text.
- **Outputs:** All ten standard macOS iconset slices (`16×16` through `512×512@2x`), compiled with `iconutil` into `bin/AppIcon.icns`; a 180 × 180 `site/apple-touch-icon.png`.
- **Production:** The current master and derivatives are generated with native CoreGraphics and AppKit, then downsampled with `sips` and compiled with `iconutil`.

## Vector identity

`site/assets/brand/cito-mark.svg`, both `cito-logo-*.svg` wordmarks, and `site/favicon.svg` use the same white C/core and pale outbound wave on cobalt. Wordmarks use plain contrasting type without stroke outlines. Keep contrast above 4.5:1 in both page themes.

## Native interface captures

Capture real headless AppKit views at 2× Retina resolution with the integration test `v40_four_surface_captures_clear_confirmation_and_dynamic_accessibility` and `CITO_TEST_SNAPSHOT_DIR`. Inspect every image before copying it into `site/assets/screenshots/`:

- `menu-dark.png`: Slim menu starting with LISTENING SESSIONS, Stop actions, Pin, Auto-Listen, AVAILABLE SESSIONS, unread navigation, and Stop All. No dedicated inbox card; paused inboxes show one compact notice opening Human Channels.
- `directory-dark.png`: Unified Nodes, readable addresses, All Nodes/Mine filters, and Message actions.
- `activity-dark.png`: Unread messages and an inline one-line reply composer.
- `settings-apps-dark.png`: App setup states, status dots, and Check & Repair.
- `settings-channels-dark.png`: Compact Inbox Control Bar (identity, active delivery paths, Pause/Resume Inbox, and Self/Team on multi-user Hubs), shared gateway, and personal account pairing. Default active and paused single-channel states fit without scrolling.
- `settings-hubs-dark.png`: Sharing controls, connected Hubs, and the Default badge.

Do not generate mock UI artwork or modify labels in an image editor. Screenshot geometry, tooltips, accessibility, and contrast are checked against the rendered native view tree.

## Social preview

`site/assets/og-card.png` is 1200 × 630. Use a neutral slate dot-grid, cobalt Cito lockup, the headline “Signals that wake the right session.”, the subtitle “One inbox. Every device. Your coding apps.”, a cobalt `brew install --cask tefx/tap/cito` pill, and the labels “Menubar badge + Activity replies” and “Human ↔ Hub ↔ Agent”. Composite the current menu and Activity captures at their original aspect ratios. Keep the install command readable at social-preview scale.

## Architecture diagram

`site/assets/art/architecture-topology.svg` is an editable 960 × 420 hand-drawn vector diagram with three columns and clear arrow lanes. Left: **Agent Session Nodes · Mac / Workstation**, with **Pi Coding Agent**, **Codex**, and **Claude Code** flowing through **cito-mcp** and **cito daemon (:4290)**; label **Scope: self | workspace**. Center: **Cito Hub · Embedded local or Shared :4291**, containing **HubEngine (SQLite WAL)**, **Node Registry & Leases (Node 1 ↔ Channels 1..K)**, **Durable Signals & Atomic Wake Claims**, and **Auth: Trusted | Token | OIDC (PKCE) · HTTP / SSE**. Right: **Human Node (@alice) & Attached Channels (1..K)**, with **1 · MacBook Menu Bar & Activity Inbox**, **2 · Mac Studio Menu Bar**, and **3 · Hub-Hosted Telegram Channel (Admin Bot Service + User Pairing)**. Show unified Hub read synchronization as **signal_ack / on_acked**; do not imply passive Telegram reading clears badges. Use Cito Blue for the Hub and primary paths, neutral paper cards, gently irregular strokes, and accessible contrast. Keep every label editable, split long labels across lines, and reserve arrow lanes outside text boxes. Exclude internal transport jargon and reply-retention details; do not substitute a raster illustration.
