import { createServer, type Server } from "node:http";
import { existsSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize, resolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { runInNewContext } from "node:vm";

const SITE_DIR = resolve("site");

async function copyFixture(writeText?: (command: string) => Promise<void>) {
  const badge = { textContent: "Copy", setAttribute() {} };
  let click: (() => Promise<void>) | undefined;
  const button = {
    disabled: false,
    getAttribute: () => "brew install tefx/tap/cito",
    querySelector: () => badge,
    addEventListener(_event: string, handler: () => Promise<void>) { click = handler; },
  };
  const source = await readFile(join(SITE_DIR, "scripts/hero-preview.js"), "utf8");
  runInNewContext(source, {
    document: {
      querySelectorAll: (selector: string) => selector === "[data-copy-command]" ? [button] : [],
      querySelector: () => null,
    },
    navigator: { clipboard: writeText ? { writeText } : undefined },
  });
  return { badge, button, click: () => click!() };
}

describe("Copy command feedback", () => {
  it("waits for the actual Clipboard API promise and prevents duplicate pending writes", async () => {
    let resolveWrite!: () => void;
    const writes: string[] = [];
    const fixture = await copyFixture(command => {
      writes.push(command);
      return new Promise<void>(resolve => { resolveWrite = resolve; });
    });
    const pending = fixture.click();
    expect(fixture.badge.textContent).toBe("Copy");
    expect(fixture.button.disabled).toBe(true);
    await fixture.click();
    expect(writes).toEqual(["brew install tefx/tap/cito"]);
    resolveWrite();
    await pending;
    expect(fixture.badge.textContent).toBe("Copied ✓");
    expect(fixture.button.disabled).toBe(false);
  });

  it("reports permission rejection and allows a successful retry", async () => {
    let attempt = 0;
    const fixture = await copyFixture(async () => {
      if (++attempt === 1) throw new Error("Permission denied");
    });
    await fixture.click();
    expect(fixture.badge.textContent).toBe("Copy failed");
    expect(fixture.button.disabled).toBe(false);
    await fixture.click();
    expect(fixture.badge.textContent).toBe("Copied ✓");
  });

  it("reports an unavailable Clipboard API", async () => {
    const fixture = await copyFixture();
    await fixture.click();
    expect(fixture.badge.textContent).toBe("Copy failed");
    expect(fixture.button.disabled).toBe(false);
  });
});

const MIME_BY_EXT: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".md": "text/markdown; charset=utf-8",
};

function parsePngDimensions(buf: Buffer): { width: number; height: number } {
  // PNG signature (8 bytes) + IHDR length (4 bytes) + "IHDR" (4 bytes) + width (4 bytes BE) + height (4 bytes BE)
  expect(buf.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))).toBe(true);
  return {
    width: buf.readUInt32BE(16),
    height: buf.readUInt32BE(20),
  };
}

function extractLocalRefs(html: string): string[] {
  const refs: string[] = [];
  const attrRegex = /\b(?:href|src)=["']([^"']+)["']/gi;
  let match: RegExpExecArray | null;
  while ((match = attrRegex.exec(html)) !== null) {
    const raw = match[1]!;
    if (
      raw.startsWith("http://") ||
      raw.startsWith("https://") ||
      raw.startsWith("mailto:") ||
      raw.startsWith("#")
    ) {
      continue;
    }
    refs.push(raw);
  }
  return refs;
}

describe("Website assets, behavior, and /cito/ subpath bundle", () => {
  let server: Server;
  let baseUrl = "";

  beforeAll(async () => {
    server = createServer(async (req, res) => {
      const reqUrl = new URL(req.url ?? "/", "http://127.0.0.1");
      if (!reqUrl.pathname.startsWith("/cito/")) {
        res.statusCode = 404;
        res.end("Not under /cito/ subpath");
        return;
      }
      const relPath = reqUrl.pathname === "/cito/" ? "index.html" : reqUrl.pathname.slice("/cito/".length);
      const safeRel = normalize(relPath).replace(/^(\.\.(\/|\\|$))+/, "");
      const filePath = join(SITE_DIR, safeRel);
      if (!filePath.startsWith(SITE_DIR) || !existsSync(filePath)) {
        res.statusCode = 404;
        res.end("Not Found: " + reqUrl.pathname);
        return;
      }
      try {
        const st = await stat(filePath);
        const actualPath = st.isDirectory() ? join(filePath, "index.html") : filePath;
        const body = await readFile(actualPath);
        res.statusCode = 200;
        res.setHeader("content-type", MIME_BY_EXT[extname(actualPath)] ?? "application/octet-stream");
        res.end(body);
      } catch {
        res.statusCode = 404;
        res.end("Read error");
      }
    });

    await new Promise<void>(resolveListen => {
      server.listen(0, "127.0.0.1", () => resolveListen());
    });
    const addr = server.address();
    if (!addr || typeof addr === "string") throw new Error("Expected TCP address");
    baseUrl = `http://127.0.0.1:${addr.port}`;
  });

  afterAll(async () => {
    await new Promise<void>(resolveClose => server.close(() => resolveClose()));
  });

  it("verifies canonical transceiver SVG geometry, OpenAI Images 2.6 art assets, and GitHub Pages workflow", async () => {
    expect(existsSync(resolve("site/CNAME"))).toBe(false);
    expect(existsSync(resolve("site/.nojekyll"))).toBe(true);
    expect(existsSync(resolve(".github/workflows/pages.yml"))).toBe(true);

    const pagesWorkflow = await readFile(resolve(".github/workflows/pages.yml"), "utf8");
    expect(pagesWorkflow).toContain("actions/upload-pages-artifact@v3");
    expect(pagesWorkflow).toContain("actions/deploy-pages@v4");

    const svgFiles = [
      "site/assets/brand/cito-mark.svg",
      "site/assets/brand/cito-logo-dark.svg",
      "site/assets/brand/cito-logo-light.svg",
      "site/favicon.svg",
    ];
    for (const rel of svgFiles) {
      const svg = await readFile(resolve(rel), "utf8");
      expect(svg).toContain("M 12.09 5.32 A 5.50 5.50 0 1 0 12.09 12.68");
      expect(svg).toContain('cx="8" cy="9" r="1.65"');
      expect(svg).toContain("M 14.84 5.36 A 7.75 7.75 0 0 1 14.84 12.64");
      expect(svg).toContain("#F3F4F6");
      expect(svg).toContain("#F59E0B");
      if (rel.includes("cito-logo")) {
        expect(svg).toMatch(/<rect[^>]*width="130"[^>]*height="34"[^>]*rx="9"[^>]*fill="#14171F"/);
        expect(svg).toMatch(/<text[^>]*fill="#F3F4F6"/);
      }
      if (rel.endsWith("cito-mark.svg")) {
        expect(svg).toMatch(/<rect[^>]*width="34"[^>]*height="34"[^>]*rx="8"[^>]*fill="#14171F"/);
      }
    }

    const touchIcon = await readFile(resolve("site/apple-touch-icon.png"));
    expect(parsePngDimensions(touchIcon)).toEqual({ width: 180, height: 180 });

    const ogCard = await readFile(resolve("site/assets/og-card.png"));
    expect(parsePngDimensions(ogCard)).toEqual({ width: 1200, height: 630 });

    const artAssets = [
      "site/assets/art/hero-stage.png",
      "site/assets/art/moment-cross-harness.png",
      "site/assets/art/moment-human-network.png",
      "site/assets/art/app-icon-master.png",
    ];
    for (const art of artAssets) {
      const st = await stat(resolve(art));
      expect(st.size).toBeGreaterThan(50_000);
    }

    const promptsDoc = await readFile(resolve("site/assets/IMAGEGEN_PROMPTS.md"), "utf8");
    expect(promptsDoc).toContain("app-icon-master.png");
    expect(promptsDoc).toContain("hero-stage.png");
    expect(promptsDoc).toContain("moment-cross-harness.png");
    expect(promptsDoc).toContain("moment-human-network.png");
  });

  it("serves /cito/ and /cito/docs/{guide,plugins,architecture}.html with HTTP 200 on every linked asset and zero root-absolute links", async () => {
    const pages = [
      "/cito/",
      "/cito/docs/guide.html",
      "/cito/docs/plugins.html",
      "/cito/docs/architecture.html",
    ];

    for (const pagePath of pages) {
      const pageUrl = new URL(pagePath, baseUrl);
      const res = await fetch(pageUrl);
      expect(res.status, `Expected 200 for ${pagePath}`).toBe(200);
      const html = await res.text();

      // Zero root-absolute href="/..." or src="/..."
      expect(html).not.toMatch(/\b(?:href|src)=["']\//i);

      // Canonical & OpenGraph URLs use https://tefx.github.io/cito/...
      expect(html).toContain("https://tefx.github.io/cito/");
      expect(html).toContain("https://tefx.github.io/cito/assets/og-card.png");

      const refs = extractLocalRefs(html);
      expect(refs.length).toBeGreaterThan(4);
      for (const ref of refs) {
        const assetUrl = new URL(ref, pageUrl);
        expect(assetUrl.pathname.startsWith("/cito/"), `Ref ${ref} in ${pagePath} resolved outside /cito/: ${assetUrl.pathname}`).toBe(true);
        const assetRes = await fetch(assetUrl);
        expect(assetRes.status, `Broken asset ${ref} (resolved to ${assetUrl.pathname}) on ${pagePath}`).toBe(200);
      }
    }
  });

  it("keeps official installation links and technical documentation references", async () => {
    const indexHtml = await readFile(resolve("site/index.html"), "utf8");
    const guideHtml = await readFile(resolve("site/docs/guide.html"), "utf8");
    const pluginsHtml = await readFile(resolve("site/docs/plugins.html"), "utf8");
    const archHtml = await readFile(resolve("site/docs/architecture.html"), "utf8");
    const readmeMd = await readFile(resolve("README.md"), "utf8");

    // 4 homepage sections & key product copy
    expect(indexHtml).toContain("Where your agents talk—with you, and each other.");
    expect(indexHtml).toContain("⚡ Agent → Agent");
    expect(indexHtml).toContain("💬 Agent ↔ You");
    expect(indexHtml).toContain("Stop playing courier between your agents");
    expect(indexHtml).toContain("You are a node on the same network");
    expect(indexHtml).toContain("From one Mac to your whole team");
    expect(indexHtml).toContain("Built for your favorite harnesses.");
    expect(indexHtml).toContain("Ready in under a minute.");

    // Strict homepage restraint: zero pixel/point dimensions, ports, SQLite internals, AppKit classes, or AI-slop buzzwords
    const bannedHomepageTokens = [
      /\b380pt\b/i,
      /560×440pt/i,
      /\bpx\b/i,
      /\d+px/i,
      /:4290\b/,
      /:4291\b/,
      /consumer_ack/i,
      /signal_claim_dispatch/i,
      /hub_channel_deliveries/i,
      /SQLite/i,
      /\bWAL\b/,
      /NSMenu/i,
      /KeyPanel/i,
      /\bseamless(?:ly)?\b/i,
      /\bempower(?:ing|s|ed)?\b/i,
      /\bsupercharg(?:e|ing|ed)\b/i,
      /next-generation/i,
      /reimagined/i,
      /game-changer/i,
      /\bsynergy\b/i,
      /\bnot\s+[^,.]+,\s*but\s+/i,
    ];
    for (const pattern of bannedHomepageTokens) {
      expect(indexHtml).not.toMatch(pattern);
    }

    // Official installation channels only (Homebrew + Cito.app; zero npm install or && cito setup) & First-Launch Explicit Consent copy
    for (const doc of [indexHtml, guideHtml, readmeMd]) {
      expect(doc).toContain("brew install tefx/tap/cito");
      expect(doc).toContain("Cito.app");
      expect(doc).not.toMatch(/npm\s+install/i);
      expect(doc).not.toMatch(/&&\s*cito\s+setup/i);
      expect(doc).not.toContain("Auto-configures detected coding harnesses");
      expect(doc).not.toContain("automatically bootstraps your local hub and detected coding harnesses");
    }

    // Both logo variants contain their own backing; README uses a theme-independent image.
    expect(readmeMd).toMatch(/<img alt="Cito" src="site\/assets\/brand\/cito-logo-dark\.svg"/);
    expect(readmeMd).toContain("https://tefx.github.io/cito/");

    // Technical docs coverage
    expect(pluginsHtml).toContain("@tefx/cito/plugin-sdk");
    expect(pluginsHtml).toContain("HarnessAdapter");
    expect(pluginsHtml).toContain("HumanChannelPlugin");
    expect(archHtml).toContain("@self");
    expect(archHtml).toContain("cito_listen");
    expect(archHtml).toContain("cito_send");
  });
});
