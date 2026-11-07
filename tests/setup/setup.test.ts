import { afterEach, describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { inspectSetup, previewSetup, applySetup, ensureCliShims, inspectLaunchAtLogin, setLaunchAtLogin, buildMacAppBundle } from "../../src/setup/index.js";
import { CitoService } from "../../src/mcp/service.js";
import { loadConfig, toCanonicalListenerId } from "../../src/shared/index.js";
import { ensureUiCompiled, UiBridge, type UiListenerSummary } from "../../src/ui/index.js";
import { startHubServer } from "../../src/hub/index.js";
import { call, piBridge, waitFor } from "../mcp/support.js";
import { signingKey, startOidc, browser } from "../auth-callout/oidc-fixture.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => {
  for (const fn of cleanups.splice(0).reverse()) await fn();
});

async function makeTempHome() {
  const root = await mkdtemp(join(tmpdir(), "cito-setup-"));
  cleanups.push(() => rm(root, { recursive: true, force: true }));
  const env: NodeJS.ProcessEnv = {
    HOME: root,
    CITO_STATE_DIR: join(root, ".cito"),
    CITO_CODEX_HOME: join(root, ".codex"),
    CITO_PI_HOME: join(root, ".pi/agent"),
    CITO_CLAUDE_HOME: join(root, ".claude"),
    CITO_USER: "alice",
  };
  return { root, env };
}

describe("cito setup engine and plugin setup descriptors", () => {
  it("previews (--dry-run) without writing files, applies idempotently, preserves TOML comments, detects conflicts, and creates .bak.<timestamp> only on actual mutation", async () => {
    const { root, env } = await makeTempHome();
    const codexDir = join(root, ".codex");
    const piDir = join(root, ".pi/agent");
    const claudeDir = join(root, ".claude");
    await mkdir(codexDir, { recursive: true });
    await mkdir(piDir, { recursive: true });
    await mkdir(claudeDir, { recursive: true });

    // Pre-populate ~/.codex/config.toml with user comments (including a commented-out [mcp_servers.cito])
    const initialToml = [
      "# Top-level user comment",
      'model = "gpt-5"',
      "# [mcp_servers.cito]",
      '# command = "commented-out"',
      "",
      "[mcp_servers.other]",
      'command = "other-mcp"',
      "# Trailing comment",
    ].join("\n");
    const codexTomlPath = join(codexDir, "config.toml");
    await writeFile(codexTomlPath, initialToml, "utf8");

    // Pre-populate ~/.claude/settings.json with an existing user hook
    const claudeSettingsPath = join(claudeDir, "settings.json");
    const initialClaude = {
      theme: "dark",
      hooks: {
        Stop: [{ hooks: [{ type: "command", command: "my-custom-stop-hook" }] }],
      },
    };
    await writeFile(claudeSettingsPath, JSON.stringify(initialClaude, null, 2) + "\n", "utf8");

    // 1. Inspect before setup
    const beforeInspect = await inspectSetup({ env });
    expect(beforeInspect.configExists).toBe(false);
    expect(beforeInspect.defaultHub).toBe("local");
    expect(beforeInspect.harnesses.find(h => h.id === "codex")).toMatchObject({
      installed: true,
      configured: false,
    });
    expect(beforeInspect.harnesses.find(h => h.id === "claude")).toMatchObject({
      tier: "experimental",
      installed: true,
      configured: false,
    });

    // 2. previewSetup (--dry-run) computes actions without writing or creating backups
    const preview = await previewSetup({ env });
    expect(preview.ok).toBe(true);
    expect(preview.dryRun).toBe(true);
    expect(preview.actions.find(a => a.target === "cito")?.status).toBe("created");
    expect(preview.actions.find(a => a.target === "codex")?.status).toBe("updated");
    expect(preview.actions.filter(a => a.target === "pi").map(a => a.status)).toEqual(["created", "created"]);
    expect(preview.actions.find(a => a.target === "claude")?.status).toBe("skipped");
    expect(await readFile(codexTomlPath, "utf8")).toBe(initialToml);
    expect((await readdir(codexDir)).filter(f => f.includes(".bak."))).toEqual([]);

    // 3. applySetup configures cito, codex, and pi (while skipping experimental claude by default)
    const applied1 = await applySetup({ env });
    expect(applied1.ok).toBe(true);
    const citoAction1 = applied1.actions.find(a => a.target === "cito")!;
    expect(citoAction1.status).toBe("created");
    expect(citoAction1.backupPath).toBeUndefined();
    expect((await stat(citoAction1.path)).mode & 0o777).toBe(0o600);

    const codexAction1 = applied1.actions.find(a => a.target === "codex")!;
    expect(codexAction1.status).toBe("updated");
    expect(codexAction1.backupPath).toBeDefined();
    expect(await readFile(codexAction1.backupPath!, "utf8")).toBe(initialToml);

    const updatedToml = await readFile(codexTomlPath, "utf8");
    expect(updatedToml).toContain("# Top-level user comment");
    expect(updatedToml).toContain("# [mcp_servers.cito]");
    expect(updatedToml).toContain("# Trailing comment");
    expect(updatedToml).toContain('[mcp_servers.cito]\ncommand = "cito-mcp"\nargs = []\n');

    const piSettings = JSON.parse(await readFile(join(piDir, "settings.json"), "utf8"));
    expect(piSettings.packages).toContain("npm:@tefx/cito");
    const piMcp = JSON.parse(await readFile(join(piDir, "mcp/global.mcp.json"), "utf8"));
    expect(piMcp.mcpServers.cito).toEqual({ command: "cito-mcp", args: [] });

    // 4. Re-running applySetup is 100% idempotent ("unchanged") and creates zero new .bak.<timestamp> files
    const codexBackupsAfter1 = (await readdir(codexDir)).filter(f => f.includes(".bak."));
    expect(codexBackupsAfter1).toHaveLength(1);

    const applied2 = await applySetup({ env });
    expect(applied2.ok).toBe(true);
    expect(applied2.actions.find(a => a.target === "cito")?.status).toBe("unchanged");
    expect(applied2.actions.find(a => a.target === "codex")?.status).toBe("unchanged");
    expect(applied2.actions.filter(a => a.target === "pi").map(a => a.status)).toEqual(["unchanged", "unchanged"]);
    expect((await readdir(codexDir)).filter(f => f.includes(".bak."))).toHaveLength(1);
    expect((await readdir(join(root, ".cito"))).filter(f => f.includes(".bak."))).toEqual([]);

    // 5. Conflict detection in ~/.codex/config.toml and in-place replacement with --force
    const conflictingToml = [
      "# Leading comment",
      "[mcp_servers.cito]",
      'command = "/custom/old-cito-bin"',
      'args = ["--legacy"]',
      "",
      "# Comment after cito section",
      "[mcp_servers.another]",
      'command = "another-bin"',
    ].join("\n") + "\n";
    await writeFile(codexTomlPath, conflictingToml, "utf8");

    const conflictResult = await applySetup({ env });
    expect(conflictResult.ok).toBe(false);
    expect(conflictResult.actions.find(a => a.target === "codex")?.status).toBe("conflict");
    expect(await readFile(codexTomlPath, "utf8")).toBe(conflictingToml);
    expect((await readdir(codexDir)).filter(f => f.includes(".bak."))).toHaveLength(1);

    const forcedResult = await applySetup({ env, force: true });
    expect(forcedResult.ok).toBe(true);
    expect(forcedResult.actions.find(a => a.target === "codex")?.status).toBe("updated");
    const replacedToml = await readFile(codexTomlPath, "utf8");
    expect(replacedToml).toContain("# Leading comment");
    expect(replacedToml).toContain("# Comment after cito section");
    expect(replacedToml).toContain('[mcp_servers.another]\ncommand = "another-bin"');
    expect(replacedToml).toContain('[mcp_servers.cito]\ncommand = "cito-mcp"\nargs = []');
    expect(replacedToml).not.toContain("/custom/old-cito-bin");

    // 6. Opt-in --enable claude configures ~/.claude/settings.json and deduplicates hooks without clobbering user hooks
    const claudeApplied1 = await applySetup({ env, enableHarnesses: ["claude"] });
    expect(claudeApplied1.ok).toBe(true);
    const claudeAction1 = claudeApplied1.actions.find(a => a.target === "claude")!;
    expect(claudeAction1.status).toBe("updated");
    expect(claudeAction1.backupPath).toBeDefined();

    const claudeJson1 = JSON.parse(await readFile(claudeSettingsPath, "utf8"));
    expect(claudeJson1.theme).toBe("dark");
    expect(claudeJson1.mcpServers.cito).toEqual({
      command: "cito-mcp",
      args: [],
      env: { CITO_CLAUDE_CHANNEL: "1" },
    });
    expect(claudeJson1.hooks.Stop).toHaveLength(2);
    expect(claudeJson1.hooks.SessionStart).toHaveLength(1);
    expect(claudeJson1.hooks.UserPromptSubmit).toHaveLength(1);

    // Re-running with enableHarnesses: ["claude"] is unchanged and does not duplicate hooks
    const claudeApplied2 = await applySetup({ env, enableHarnesses: ["claude"] });
    expect(claudeApplied2.actions.find(a => a.target === "claude")?.status).toBe("unchanged");
    const claudeJson2 = JSON.parse(await readFile(claudeSettingsPath, "utf8"));
    expect(claudeJson2.hooks.Stop).toHaveLength(2);
  });

  it("manages hubs via cito setup CLI (--hub, --enable-hub, --disable-hub), filters disabled hubs in CitoService, and resets scope to self when migrating to an OIDC hub", async () => {
    const { root, env } = await makeTempHome();
    const bin = resolve("bin/cito-mcp");

    // Add a remote tailnet hub via CLI
    const addTailnet = spawnSync(
      process.execPath,
      [bin, "setup", "--json", "--hub", "tailnet", "--url", "http://127.0.0.1:4291"],
      { env: { ...process.env, ...env }, encoding: "utf8", timeout: 5000 },
    );
    expect(addTailnet.status).toBe(0);
    const parsedTailnet = JSON.parse(addTailnet.stdout);
    expect(parsedTailnet.ok).toBe(true);
    expect(parsedTailnet.defaultHub).toBe("tailnet");

    // Add a second hub and toggle --disable-hub / --enable-hub
    const key = await signingKey("ES256", "setup-oidc");
    const oidc = await startOidc([key.jwk]);
    cleanups.push(() => oidc.stop());
    const teamBroker = await startHubServer({ authMode: "oidc", oidcIssuer: oidc.issuer, oidcClientId: "cito" });
    cleanups.push(() => teamBroker.stop());
    const tailnetBroker = await startHubServer({ authMode: "none" });
    cleanups.push(() => tailnetBroker.stop());

    await applySetup({
      env,
      hub: { name: "tailnet", url: tailnetBroker.url, authMode: "none", defaultScope: "workspace", setDefault: true },
    });
    await applySetup({
      env,
      hub: {
        name: "team",
        url: teamBroker.url,
        authMode: "oidc",
        oidcIssuer: oidc.issuer,
        oidcClientId: "cito",
      },
      disableHubs: ["local"],
    });

    const statusCli = spawnSync(
      process.execPath,
      [bin, "setup", "--status", "--json"],
      { env: { ...process.env, ...env }, encoding: "utf8", timeout: 5000 },
    );
    expect(statusCli.status).toBe(0);
    const statusJson = JSON.parse(statusCli.stdout);
    expect(statusJson.hubs.local.enabled).toBe(false);
    expect(statusJson.hubs.tailnet.enabled).toBe(true);
    expect(statusJson.hubs.team.enabled).toBe(true);

    // Verify CitoService omits disabled "local" hub from slots and enforces fail-closed scope: "self" on switch_hub to OIDC "team"
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims()), refresh_token: "r1" };
    const cfg = loadConfig(env);
    const pi = await piBridge(cfg.stateDir);
    cleanups.push(() => pi.stop());
    const service = await CitoService.create(cfg, { browserOpener: browser });
    cleanups.push(() => service.close());

    expect([...service.slots.keys()]).toEqual(["tailnet", "team"]);
    expect(service.slots.has("local")).toBe(false);

    // Enroll on tailnet with workspace scope
    const enrolled = await call(service, "cito_listen", { hub: "tailnet", about: "Tailnet work", scope: "workspace" }, pi.meta);
    const canonId = toCanonicalListenerId(String(enrolled.listener_id).split(":").at(-1)!);
    expect(service.state.listener(canonId, "tailnet")?.scope).toBe("workspace");

    // Re-listening on tailnet without specifying scope preserves existing "workspace" scope
    const rebound = await call(service, "cito_listen", { hub: "tailnet", about: "Tailnet rebound" }, pi.meta);
    expect(rebound.scope).toBe("workspace");
    expect(service.state.listener(canonId, "tailnet")?.scope).toBe("workspace");

    // Migrating from non-OIDC "tailnet" to OIDC "team" via switch_hub resets scope to "self" (fail-closed privacy protection)
    await service.handleUiAction({ op: "switch_hub", from_hub: "tailnet", to_hub: "team", listener_id: canonId });
    const teamRow = service.state.db.prepare("SELECT * FROM local_listeners WHERE hub = 'team' AND status = 'ACTIVE'").get() as {
      listener_id: string;
      scope: string;
    } | undefined;
    expect(teamRow).toBeDefined();
    expect(teamRow!.scope).toBe("self");
  });

  it("supports zero-config local:// end-to-end signaling with >500 character messages and omits scope pill on local hub in cito-ui", async () => {
    const { root, env } = await makeTempHome();
    const cfg = loadConfig(env);
    expect(cfg.defaultHub).toBe("local");
    expect(cfg.natsUrl).toBe("local://");

    const pi = await piBridge(cfg.stateDir);
    cleanups.push(() => pi.stop());
    const service = await CitoService.create(cfg);
    cleanups.push(() => service.close());

    // 1. cito_listen on local:// defaults to scope: "workspace" and unprefixed listener_id
    const listened = await call(service, "cito_listen", { about: "Zero-config local session", tag: "local-dev" }, pi.meta);
    expect(listened.hub).toBe("local");
    expect(listened.scope).toBe("workspace");
    expect(listened.listener_id).toMatch(/^local-dev-[a-z2-7]{12}$/);

    // 2. cito_find discovers the local session
    const found = await call(service, "cito_find", { query: "Zero-config" });
    expect(found.listeners).toMatchObject([
      {
        hub: "local",
        listener_id: listened.listener_id,
        scope: "workspace",
        about: "Zero-config local session",
      },
    ]);

    // 3. cito_send transmits a >500 codepoint message (2,500 chars) end-to-end without rejection
    const longMessage = "Context payload segment — ".repeat(100).trim();
    expect(longMessage.length).toBeGreaterThan(2000);
    const sent = await call(service, "cito_send", {
      to: listened.listener_id,
      message: longMessage,
    });
    expect(sent).toMatchObject({
      to: listened.listener_id,
      status: "wake_accepted",
      seq: 1,
    });

    const canonId = toCanonicalListenerId(listened.listener_id);
    await service.receivers.get(`local:${canonId}`)!.dispatch();
    await waitFor(() => pi.messages.length === 1);
    expect(pi.messages[0]!.content).toContain(longMessage);

    // Reset acknowledged_at_ms to verify cito_inbox also returns the full >500 char message intact
    service.state.run("UPDATE local_signals SET acknowledged_at_ms = NULL");
    const inbox = await call(service, "cito_inbox", { ack_all: true }, pi.meta);
    expect(inbox.signals).toHaveLength(1);
    expect(inbox.signals[0].message).toBe(longMessage);

    // 4. cito_unlisten revokes the local listener
    const unlistened = await call(service, "cito_unlisten", {}, pi.meta);
    expect(unlistened).toMatchObject({ status: "unlistened", listener_id: listened.listener_id });

    // 5. Verify cito-ui hides scope:<id> pill on local (auth_mode: "none") and accepts auth_mode: "token"
    if (process.platform === "darwin") {
      const uiBin = await ensureUiCompiled(root);
      const diagnostics: string[] = [];
      const bridge = new UiBridge(root, {
        binary: uiBin,
        env: { CITO_UI_HEADLESS: "1" },
        diagnostic: line => diagnostics.push(line),
      });
      cleanups.push(() => bridge.close());
      await bridge.start();
      await waitFor(() => diagnostics.includes("cito-ui: state hubs=0 listeners=0"));

      const localListener: UiListenerSummary = {
        hub: "local",
        listener_id: "c_abcdefghijkl",
        public_id: "local-dev-abcdefghijkl",
        scoped_id: "local-dev-abcdefghijkl",
        harness: "codex",
        session_id: "thread-1",
        title: "Local Codex Session",
        cwd: "/tmp",
        repo: null,
        scope: "workspace",
      };
      bridge.state(
        [{ name: "local", status: "connected", user: "alice", auth_mode: "none", error: null }],
        [localListener],
      );
      await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1"));

      const child = Reflect.get(bridge, "child") as ChildProcessWithoutNullStreams;
      const before = diagnostics.filter(l => l.startsWith("cito-ui: menu ")).length;
      child.stdin.write(JSON.stringify({ op: "test_inspect" }) + "\n");
      await waitFor(() => diagnostics.filter(l => l.startsWith("cito-ui: menu ")).length > before);
      const snap = JSON.parse(diagnostics.filter(l => l.startsWith("cito-ui: menu ")).at(-1)!.slice("cito-ui: menu ".length));
      const flatten = (nodes: { id: string; x: number; width: number; children: any[] }[]): { id: string; x: number; width: number }[] =>
        nodes.flatMap(n => [n, ...flatten(n.children ?? [])]);
      const allNodes = flatten(snap.items);

      // scope:local-dev-abcdefghijkl and open: are omitted on local (auth_mode: "none"), while stop: is at x=328
      expect(allNodes.find(n => n.id === "scope:" + localListener.scoped_id)).toBeUndefined();
      expect(allNodes.find(n => n.id === "open:" + localListener.scoped_id)).toBeUndefined();
      expect(allNodes.find(n => n.id === "stop:" + localListener.scoped_id)?.x).toBe(328);

      // Token-authenticated hub state frame is accepted and rendered by cito-ui
      bridge.state(
        [{ name: "tailnet", status: "connected", user: "alice", auth_mode: "token", error: null }],
        [{ ...localListener, hub: "tailnet", scoped_id: "tailnet:local-dev-abcdefghijkl" }],
      );
      await waitFor(() => diagnostics.filter(l => l === "cito-ui: state hubs=1 listeners=1").length === 2);
    }
  }, 20_000);

  it("installs executable ~/.cito/bin shims, manages LaunchAgent plist (--login-item / --no-login-item), and builds a < 5 MB Cito.app bundle", async () => {
    const { root, env } = await makeTempHome();
    const bin = resolve("bin/cito-mcp");

    // 1. ensureCliShims writes executable 0755 shims into ~/.cito/bin/{cito,cito-mcp,cito-claude-hook}
    const shims = await ensureCliShims({ env });
    expect(shims.paths).toHaveLength(3);
    for (const shimPath of shims.paths) {
      expect((await stat(shimPath)).mode & 0o777).toBe(0o755);
    }

    const hookShimRes = spawnSync(join(shims.binDir, "cito-claude-hook"), [], {
      env: { ...process.env, ...env },
      input: JSON.stringify({ hook_event_name: "Stop", cwd: root }),
      encoding: "utf8",
      timeout: 5000,
    });
    expect(hookShimRes.status).toBe(0);
    expect(hookShimRes.stdout).toBe("{}\n");

    const citoVerRes = spawnSync(join(shims.binDir, "cito"), ["--version"], {
      env: { ...process.env, ...env },
      encoding: "utf8",
      timeout: 5000,
    });
    expect(citoVerRes.status).toBe(0);
    expect(citoVerRes.stdout).toBe("cito v0.15.0\n");

    // 2. LaunchAgent management via API and CLI (--login-item / --no-login-item)
    expect((await inspectLaunchAtLogin({ env })).enabled).toBe(false);
    const enabledLogin = await setLaunchAtLogin(true, { env });
    expect(enabledLogin.enabled).toBe(true);
    expect(enabledLogin.plistPath).toBe(join(root, "Library/LaunchAgents/com.tefx.cito.daemon.plist"));
    const plistContent = await readFile(enabledLogin.plistPath, "utf8");
    expect(plistContent).toContain("com.tefx.cito.daemon");
    expect(plistContent).toContain("<key>RunAtLoad</key>");
    expect((await inspectLaunchAtLogin({ env })).enabled).toBe(true);

    const disableCli = spawnSync(
      process.execPath,
      [bin, "setup", "--no-login-item", "--json"],
      { env: { ...process.env, ...env }, encoding: "utf8", timeout: 5000 },
    );
    expect(disableCli.status).toBe(0);
    expect((await inspectLaunchAtLogin({ env })).enabled).toBe(false);

    // 3. buildMacAppBundle (--build-app) creates < 5 MB Cito.app with Info.plist, MacOS/Cito, and Helpers
    const appDir = join(root, "Apps");
    const buildCli = spawnSync(
      process.execPath,
      [bin, "setup", "--build-app", appDir, "--json"],
      { env: { ...process.env, ...env }, encoding: "utf8", timeout: 30_000 },
    );
    expect(buildCli.status).toBe(0);
    const built = JSON.parse(buildCli.stdout) as { appPath: string; bytes: number };
    expect(built.appPath).toBe(join(appDir, "Cito.app"));
    expect(built.bytes).toBeGreaterThan(100_000);
    expect(built.bytes).toBeLessThan(5 * 1024 * 1024);

    const infoPlist = await readFile(join(built.appPath, "Contents/Info.plist"), "utf8");
    expect(infoPlist).toContain("one.tefx.cito");
    expect(infoPlist).toContain("<key>LSUIElement</key>\n  <true/>");
    expect((await stat(join(built.appPath, "Contents/MacOS/Cito"))).mode & 0o777).toBe(0o755);
    expect((await stat(join(built.appPath, "Contents/Helpers/cito-ui"))).mode & 0o777).toBe(0o755);
    if (process.platform === "darwin" && process.arch === "arm64") {
      expect((await stat(join(built.appPath, "Contents/Helpers/cito-native"))).mode & 0o777).toBe(0o755);
    }

    const directBuilt = await buildMacAppBundle({ env });
    expect(directBuilt.appPath).toBe(join(root, ".cito/Applications/Cito.app"));
    expect(directBuilt.bytes).toBeLessThan(5 * 1024 * 1024);
  }, 30_000);
});
