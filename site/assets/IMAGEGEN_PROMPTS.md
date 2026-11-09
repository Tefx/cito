# Cito Brand & Website Raster Artwork Prompts (OpenAI Images 2.6)

All studio raster assets in `site/assets/art/` and `bin/AppIcon.icns` were generated with **OpenAI Images 2.6** (`gpt-image-1.5` / Codex `image_gen`) using a unified industrial design language inspired by Teenage Engineering, Dieter Rams / Braun, and Apple macOS dark hardware aesthetics.

## 1. `site/assets/art/app-icon-master.png` -> `bin/AppIcon.icns`
- **Target Resolution**: `1024 × 1024` master squircle, downsampled into all 10 standard macOS `.iconset` slices (`16×16` through `512×512@2x`) and compiled via `/usr/bin/iconutil -c icns -o bin/AppIcon.icns`.
- **Prompt**:
  > Macro product photography of a tactile macOS application icon squircle on a pure pitch-black background. Bead-blasted dark graphite anodized aluminum body (#14171F) with a subtle chamfered rim and a recessed matte-obsidian circular instrument dial. Inside the dial: an open C-shaped brushed silver transceiver arc on the left (opening toward the right), a glowing warm-amber central signal core (#F59E0B), and a single warm-amber outbound acoustic wave arc on the right. Soft studio rim lighting, crisp physical materials, zero text, zero neon purple/cyan glow.

## 2. `site/assets/art/hero-stage.png`
- **Target Resolution**: `1672 × 941` (16:9 widescreen hero backdrop).
- **Prompt**:
  > Calm dark-mode industrial desk stage in deep obsidian graphite (#0C0E12) with subtle brushed aluminum edges and soft warm-amber rim light (#F59E0B) casting a gentle horizon glow across a matte dark surface. Minimal architectural composition with generous negative space designed to frame a macOS menu bar and agent signaling stage. Zero text, zero clutter, zero neon gradients.

## 3. `site/assets/art/moment-cross-harness.png`
- **Target Resolution**: `1774 × 887` (2:1 editorial illustration).
- **Prompt**:
  > Minimalist top-down studio hardware sculpture representing three distinct coding agent nodes (brushed titanium, matte slate, and anodized dark bronze modules) resting on a dark graphite surface, connected by a single crisp warm-amber optical light trace (#F59E0B) passing a signal pulse directly from one node to another. Calm studio lighting, high tactile precision, zero text.

## 4. `site/assets/art/moment-human-network.png`
- **Target Resolution**: `1536 × 1024` (3:2 editorial illustration).
- **Prompt**:
  > Minimalist dark-mode studio composition showing a sleek anodized aluminum laptop edge and a pocket handheld transceiver silhouette linked by a warm-amber signal arc (#F59E0B) to a shared central hub ring on a calm matte-obsidian desk. Represents a human staying in the loop across desktop and mobile without friction. Zero text, tactile materials, soft warm lighting.

## 5. `site/assets/og-card.png`
- **Target Resolution**: `1200 × 630` (OpenGraph / social preview card).
- **Composition**: Composited from `app-icon-master.png`, `hero-stage.png`, the Cito brand lockup, headline (*"Where your agents talk—with you, and each other."*), and primary install command (`brew install tefx/tap/cito`).

## 6. `site/assets/art/architecture-topology.png`
- **Target Resolution**: `1408 × 768` (16:9 editorial architecture sculpture).
- **Prompt**:
  > Minimalist dark-mode studio hardware sculpture representing a symmetric signaling architecture on a matte obsidian graphite surface (#0C0E12). In the center sits a circular brushed dark-aluminum Hub ring with a warm amber core (#F59E0B). On the left side, three compact precision-machined metallic compute modules (representing coding agent sessions) connect to the central Hub via warm amber optical light traces. On the right side, a sleek anodized aluminum laptop edge and a pocket handheld transceiver silhouette connect to the same central Hub via warm amber optical light traces. Calm Dieter Rams / Teenage Engineering industrial aesthetic, soft studio rim lighting, zero text, zero labels, zero neon purple/cyan.

