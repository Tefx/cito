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

- `menu-dark.png`: Shared YOUR INBOX, attached channels, agent sessions, and unread navigation.
- `directory-dark.png`: Unified Nodes, readable addresses, All Nodes/Mine filters, and Message actions.
- `activity-dark.png`: Unread messages and an inline one-line reply composer.
- `settings-apps-dark.png`: App setup states, status dots, and Check & Repair.
- `settings-channels-dark.png`: Shared gateway and personal account pairing.
- `settings-hubs-dark.png`: Sharing controls, connected Hubs, and the Default badge.

Do not generate mock UI artwork or modify labels in an image editor. Screenshot geometry, tooltips, accessibility, and contrast are checked against the rendered native view tree.

## Social preview

`site/assets/og-card.png` is 1200 × 630. Use a neutral slate dot-grid, cobalt Cito lockup, the headline “Where your agents talk—with you, and each other.”, and `brew install --cask tefx/tap/cito`. Composite the current menu and Activity captures at their original aspect ratios. Keep the install command readable at social-preview scale.

## Architecture diagram

`site/assets/art/architecture-topology.svg` is an editable 960 × 420 vector diagram. Show symmetric Human and Agent Session Nodes, attached Channels, the single-authority HubEngine, and remote Hubs. Use Cito Blue for the Hub and primary paths, readable neutral cards, and semantic plugin colors with accessible contrast. Label the Mac interaction `Unread badge + 1-line reply`. Keep protocol labels editable; do not substitute a raster illustration.
