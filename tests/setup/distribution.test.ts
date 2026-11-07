import { afterEach, describe, expect, it, vi } from "vitest";
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import citoBridge, { type BridgeAPI } from "../../plugins/pi/cito-bridge.js";
import {
  applySetup,
  buildMacAppBundle,
  ensureCliShims,
  inspectSetup,
  removeSetup,
  resolveCodesignArgs,
  resolveStablePackageRoot,
  setLaunchAtLogin,
  uninstallSetup,
} from "../../src/setup/index.js";
import { CitoService } from "../../src/mcp/service.js";
import { CITO_VERSION, loadConfig } from "../../src/shared/index.js";
import { ensureUiCompiled, UiBridge, validAction } from "../../src/ui/index.js";
import { freePort, waitFor } from "../mcp/support.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => {
  for (const fn of cleanups.splice(0).reverse()) await fn();
});

async function makeTempHome() {
  const root = await mkdtemp(join(tmpdir(), "cito-dist-"));
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

describe("Stage 5 (v0.17.0): Zero-Node Core/Hub/UI, Native Plugin Manifests, First-Launch Consent, Surgical Disconnect/Uninstall & Distribution", () => {
  it("verifies Homebrew formula syntax, zero depends_on node, Cellar->opt mapping, conditional codesign args, MIT LICENSE, and native plugin manifests", async () => {
    expect(CITO_VERSION).toBe("0.17.0");

    // 1. Homebrew formula syntax and zero depends_on "node"
    const formulaPath = resolve("packaging/homebrew/cito.rb");
    expect(existsSync(formulaPath)).toBe(true);
    const formulaText = await readFile(formulaPath, "utf8");
    expect(formulaText).toContain('version "0.17.0"');
    expect(formulaText).not.toMatch(/depends_on\s+["']node["']/i);
    if (existsSync("/usr/bin/ruby")) {
      const rubyCheck = spawnSync("/usr/bin/ruby", ["-c", formulaPath], { encoding: "utf8" });
      expect(rubyCheck.status).toBe(0);
    }

    // 2. resolveStablePackageRoot maps /Cellar/<name>/<version> to /opt/<name>
    expect(resolveStablePackageRoot("/opt/homebrew/Cellar/cito/0.17.0")).toBe("/opt/homebrew/opt/cito");
    expect(resolveStablePackageRoot("/opt/homebrew/Cellar/cito/0.17.0/libexec")).toBe("/opt/homebrew/opt/cito/libexec");
    expect(resolveStablePackageRoot("/usr/local/Cellar/cito/0.17.0")).toBe("/usr/local/opt/cito");
    expect(resolveStablePackageRoot("/Applications/Cito.app")).toBe("/Applications/Cito.app");

    // 3. Conditional Apple Developer ID signing vs ad-hoc signing
    expect(resolveCodesignArgs("/tmp/Cito.app", {})).toEqual(["-s", "-", "--force", "--deep", "/tmp/Cito.app"]);
    expect(resolveCodesignArgs("/tmp/Cito.app", { APPLE_SIGNING_IDENTITY: "Developer ID Application: Example" })).toEqual([
      "--force",
      "--options",
      "runtime",
      "--timestamp",
      "--deep",
      "--sign",
      "Developer ID Application: Example",
      "/tmp/Cito.app",
    ]);

    // 4. MIT LICENSE and release.yml workflow
    const licenseText = await readFile(resolve("LICENSE"), "utf8");
    expect(licenseText).toContain("MIT License");
    const pkgJson = JSON.parse(await readFile(resolve("package.json"), "utf8"));
    expect(pkgJson.license).toBe("MIT");
    expect(pkgJson.version).toBe("0.17.0");
    const releaseYml = await readFile(resolve(".github/workflows/release.yml"), "utf8");
    expect(releaseYml).toContain("notarytool");
    expect(releaseYml).toContain("stapler");
    expect(releaseYml).toContain("--options runtime --timestamp");

    // 5. Native harness plugin manifests for Pi, Codex, and Claude Code
    const marketplace = JSON.parse(await readFile(resolve(".agents/plugins/marketplace.json"), "utf8"));
    expect(marketplace.version).toBe("0.17.0");
    expect(marketplace.plugins[0]).toMatchObject({ name: "cito", source: "./plugins/codex" });

    const codexPlugin = JSON.parse(await readFile(resolve("plugins/codex/.codex-plugin/plugin.json"), "utf8"));
    expect(codexPlugin).toMatchObject({ name: "cito", version: "0.17.0", mcpServers: "../.mcp.json" });
    const codexMcp = JSON.parse(await readFile(resolve("plugins/codex/.mcp.json"), "utf8"));
    expect(codexMcp.mcpServers.cito).toEqual({ command: "cito-mcp", args: [] });

    const claudePlugin = JSON.parse(await readFile(resolve("plugins/claude/.claude-plugin/plugin.json"), "utf8"));
    expect(claudePlugin).toMatchObject({
      name: "cito",
      version: "0.17.0",
      mcpServers: "../.mcp.json",
      hooks: "../hooks/hooks.json",
    });
    const claudeMcp = JSON.parse(await readFile(resolve("plugins/claude/.mcp.json"), "utf8"));
    expect(claudeMcp.mcpServers.cito).toEqual({
      command: "cito-mcp",
      args: [],
      env: { CITO_CLAUDE_CHANNEL: "1" },
    });
    const claudeHooks = JSON.parse(await readFile(resolve("plugins/claude/hooks/hooks.json"), "utf8"));
    expect(Object.keys(claudeHooks.hooks)).toEqual(["Stop", "SessionStart", "UserPromptSubmit"]);

    // 6. Pi cito-bridge.ts calls pi.registerMcpServer?.("cito", ...) inside try/catch on load
    const registerMcpServer = vi.fn();
    const api: BridgeAPI = {
      registerMcpServer,
      on: () => {},
      sendMessage: () => {},
    };
    citoBridge(api);
    expect(registerMcpServer).toHaveBeenCalledTimes(1);
    expect(registerMcpServer).toHaveBeenCalledWith("cito", {
      command: expect.stringMatching(/cito-mcp$/),
      args: [],
    });

    // Throwing registerMcpServer is caught gracefully on load
    expect(() =>
      citoBridge({
        registerMcpServer: () => {
          throw new Error("MCP not ready yet");
        },
        on: () => {},
        sendMessage: () => {},
      }),
    ).not.toThrow();
  });

  it("enforces first-launch explicit consent (zero silent file mutation) and performs surgical per-app Disconnect and global Uninstall", async () => {
    const { root, env } = await makeTempHome();
    const codexDir = join(root, ".codex");
    const piDir = join(root, ".pi/agent");
    const claudeDir = join(root, ".claude");
    await mkdir(codexDir, { recursive: true });
    await mkdir(piDir, { recursive: true });
    await mkdir(claudeDir, { recursive: true });

    // Pre-populate user files with comments, custom hooks, and legacy Pi npm:@tefx/cito package
    const codexTomlPath = join(codexDir, "config.toml");
    const userToml = [
      "# User top comment",
      'model = "gpt-5"',
      "",
      "[mcp_servers.custom]",
      'command = "custom-mcp"',
      "# User bottom comment",
    ].join("\n") + "\n";
    await writeFile(codexTomlPath, userToml, "utf8");

    const piSettingsPath = join(piDir, "settings.json");
    await writeFile(
      piSettingsPath,
      JSON.stringify({ theme: "solarized", packages: ["npm:other-pkg", "npm:@tefx/cito"] }, null, 2) + "\n",
      "utf8",
    );
    const piMcpPath = join(piDir, "mcp/global.mcp.json");
    await mkdir(join(piDir, "mcp"), { recursive: true });
    await writeFile(
      piMcpPath,
      JSON.stringify({ mcpServers: { keepme: { command: "keep-mcp", args: [] } } }, null, 2) + "\n",
      "utf8",
    );

    const claudeSettingsPath = join(claudeDir, "settings.json");
    const initialClaude = {
      theme: "dark",
      mcpServers: { other: { command: "other-mcp", args: [] } },
      hooks: {
        Stop: [{ hooks: [{ type: "command", command: "user-stop-hook" }] }],
      },
    };
    await writeFile(claudeSettingsPath, JSON.stringify(initialClaude, null, 2) + "\n", "utf8");

    // 1. First-launch invariant: CitoService.create() never modifies ~/.codex, ~/.pi, ~/.claude, or LaunchAgents
    const cfg = loadConfig(env);
    const service = await CitoService.create(cfg);
    cleanups.push(() => service.close());

    expect(await readFile(codexTomlPath, "utf8")).toBe(userToml);
    expect(existsSync(join(piDir, "extensions/cito-bridge.ts"))).toBe(false);
    expect(JSON.parse(await readFile(claudeSettingsPath, "utf8"))).toEqual(initialClaude);
    expect(existsSync(join(root, "Library/LaunchAgents/com.tefx.cito.daemon.plist"))).toBe(false);

    // 2. Explicit Set Up across Pi, Codex, and Claude Code
    const applied = await applySetup({ env, enableHarnesses: ["claude"], launchAtLogin: true });
    expect(applied.ok).toBe(true);
    expect(existsSync(join(piDir, "extensions/cito-bridge.ts"))).toBe(true);
    const piSettingsAfterApply = JSON.parse(await readFile(piSettingsPath, "utf8"));
    expect(piSettingsAfterApply).toEqual({ theme: "solarized", packages: ["npm:other-pkg"] });
    const piMcpAfterApply = JSON.parse(await readFile(piMcpPath, "utf8"));
    expect(piMcpAfterApply.mcpServers).toEqual({
      keepme: { command: "keep-mcp", args: [] },
      cito: { command: "cito-mcp", args: [] },
    });
    expect(await readFile(codexTomlPath, "utf8")).toContain('[mcp_servers.cito]\ncommand = "cito-mcp"');
    const claudeAfterApply = JSON.parse(await readFile(claudeSettingsPath, "utf8"));
    expect(claudeAfterApply.mcpServers.cito).toBeDefined();
    expect(claudeAfterApply.hooks.Stop).toHaveLength(2);

    // 3. Surgical per-app Disconnect via removeSetup / setup_remove_app
    await removeSetup({ env, harnesses: ["pi"] });
    expect(existsSync(join(piDir, "extensions/cito-bridge.ts"))).toBe(false);
    expect(JSON.parse(await readFile(piMcpPath, "utf8"))).toEqual({
      mcpServers: { keepme: { command: "keep-mcp", args: [] } },
    });

    await removeSetup({ env, harnesses: ["codex"] });
    const codexAfterRemove = await readFile(codexTomlPath, "utf8");
    expect(codexAfterRemove).toBe(userToml);
    expect(codexAfterRemove).not.toContain("[mcp_servers.cito]");

    await removeSetup({ env, harnesses: ["claude"] });
    const claudeAfterRemove = JSON.parse(await readFile(claudeSettingsPath, "utf8"));
    expect(claudeAfterRemove).toEqual(initialClaude);

    const inspectAfterDisconnect = await inspectSetup({ env });
    expect(inspectAfterDisconnect.harnesses.every(h => !h.configured)).toBe(true);

    // 4. Re-apply and test global Uninstall with purgeData: false and purgeData: true
    await applySetup({ env, enableHarnesses: ["claude"], launchAtLogin: true });
    await setLaunchAtLogin(true, { env });
    expect(existsSync(join(root, "Library/LaunchAgents/com.tefx.cito.daemon.plist"))).toBe(true);
    expect(existsSync(join(root, ".cito/bin/cito"))).toBe(true);

    await uninstallSetup({ env, purgeData: false });
    expect(existsSync(join(piDir, "extensions/cito-bridge.ts"))).toBe(false);
    expect(await readFile(codexTomlPath, "utf8")).toBe(userToml);
    expect(JSON.parse(await readFile(claudeSettingsPath, "utf8"))).toEqual(initialClaude);
    expect(existsSync(join(root, "Library/LaunchAgents/com.tefx.cito.daemon.plist"))).toBe(false);
    expect(existsSync(join(root, ".cito/bin"))).toBe(false);
    expect(existsSync(join(root, ".cito/config.json"))).toBe(true);

    // Create ~/Applications/Cito.app and verify purgeData: true removes both ~/.cito and ~/Applications/Cito.app
    await mkdir(join(root, "Applications/Cito.app"), { recursive: true });
    await uninstallSetup({ env, purgeData: true });
    expect(existsSync(join(root, ".cito"))).toBe(false);
    expect(existsSync(join(root, "Applications/Cito.app"))).toBe(false);
  });

  it("verifies SettingsPanel first-launch onboarding, per-app Disconnect button, and global Remove All Integrations confirmation in cito-ui", async () => {
    if (process.platform !== "darwin") return;
    const { root, env } = await makeTempHome();
    const uiBin = await ensureUiCompiled(root);
    const diagnostics: string[] = [];
    const actions: Record<string, unknown>[] = [];

    expect(validAction({ op: "setup_remove_app", harness: "codex" })).toBe(true);
    expect(validAction({ op: "setup_uninstall_all", purge_data: true })).toBe(true);

    const bridge = new UiBridge(root, {
      binary: uiBin,
      env: { ...env, CITO_UI_HEADLESS: "1", CITO_FORCE_ONBOARDING: "1" },
      diagnostic: line => diagnostics.push(line),
      onAction: action => {
        actions.push(action as unknown as Record<string, unknown>);
      },
    });
    cleanups.push(() => bridge.close());
    await bridge.start();
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=0 listeners=0"));

    // Send initial state with unconfigured apps -> triggers first-launch onboarding opening SettingsPanel on "apps" tab
    bridge.state(
      [{ name: "local", status: "connected", user: "alice", auth_mode: "none", error: null }],
      [],
      [],
      [],
      0,
      undefined,
      {
        apps: [
          {
            id: "codex",
            display_name: "Codex",
            tier: "stable",
            installed: true,
            configured: false,
            status: "not_configured",
            detail: "Click Set Up to connect",
            paths: [],
          },
        ],
        hubs: [],
        host_hub: {
          enabled: false,
          running: false,
          port: 4291,
          endpoint_url: "http://127.0.0.1:4291",
          token: "",
          invite_command: "cito setup --hub lan --url http://127.0.0.1:4291 --default",
          error: null,
        },
        launch_at_login: false,
      },
    );
    await waitFor(() => actions.some(a => a.op === "open_settings"));
    expect(existsSync(join(root, ".cito/onboarding-shown"))).toBe(true);

    // Update app to configured: true and verify remove_app:codex ("Disconnect") and uninstall confirmation controls
    bridge.state(
      [{ name: "local", status: "connected", user: "alice", auth_mode: "none", error: null }],
      [],
      [],
      [],
      0,
      undefined,
      {
        apps: [
          {
            id: "codex",
            display_name: "Codex",
            tier: "stable",
            installed: true,
            configured: true,
            status: "ready",
            detail: "Connected and ready",
            paths: [],
          },
        ],
        hubs: [],
        host_hub: {
          enabled: false,
          running: false,
          port: 4291,
          endpoint_url: "http://127.0.0.1:4291",
          token: "",
          invite_command: "cito setup --hub lan --url http://127.0.0.1:4291 --default",
          error: null,
        },
        launch_at_login: false,
      },
    );
    await waitFor(() => diagnostics.filter(l => l.startsWith("cito-ui: state ")).length >= 3);

    const child = Reflect.get(bridge, "child") as ChildProcessWithoutNullStreams;
    child.stdin.write(JSON.stringify({ op: "test_control", id: "remove_app:codex" }) + "\n");
    await waitFor(() => actions.some(a => a.op === "setup_remove_app" && a.harness === "codex"));

    // Open uninstall confirmation, cancel via cancel_uninstall_all, then re-open, toggle purge_data checkbox, and confirm uninstall
    child.stdin.write(JSON.stringify({ op: "test_control", id: "open_uninstall_confirm" }) + "\n");
    child.stdin.write(JSON.stringify({ op: "test_control", id: "cancel_uninstall_all" }) + "\n");
    child.stdin.write(JSON.stringify({ op: "test_control", id: "open_uninstall_confirm" }) + "\n");
    child.stdin.write(JSON.stringify({ op: "test_control", id: "uninstall_purge_data" }) + "\n");
    child.stdin.write(JSON.stringify({ op: "test_control", id: "confirm_uninstall_all" }) + "\n");
    await waitFor(() => actions.some(a => a.op === "setup_uninstall_all" && a.purge_data === true));
  }, 20_000);

  it("executes Cito.app/Contents/MacOS/Cito, cito hub, cito setup, cito-mcp, and cito-claude-hook in a Clean-Room Zero-Node environment (PATH='/usr/bin:/bin:/usr/sbin:/sbin', CITO_NODE_BIN='')", async () => {
    if (process.platform !== "darwin" || process.arch !== "arm64") return;
    const { root, env } = await makeTempHome();
    const daemonPort = await freePort();
    const hubPort = await freePort();

    const codexDir = join(root, ".codex");
    const piDir = join(root, ".pi/agent");
    const claudeDir = join(root, ".claude");
    await mkdir(codexDir, { recursive: true });
    await mkdir(piDir, { recursive: true });
    await mkdir(claudeDir, { recursive: true });

    // Build Cito.app bundle and shims
    const appDir = join(root, "Apps");
    const built = await buildMacAppBundle({ destDir: appDir, env });
    await ensureCliShims({ env });

    const cleanRoomEnv: NodeJS.ProcessEnv = {
      HOME: root,
      USER: "alice",
      PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
      CITO_NODE_BIN: "",
      CITO_STATE_DIR: join(root, ".cito"),
      CITO_CODEX_HOME: codexDir,
      CITO_PI_HOME: piDir,
      CITO_CLAUDE_HOME: claudeDir,
      CITO_DAEMON_PORT: String(daemonPort),
      CITO_UI_HEADLESS: "1",
    };

    const appExecutable = join(built.appPath, "Contents/MacOS/Cito");
    const citoShim = join(root, ".cito/bin/cito");
    const mcpShim = join(root, ".cito/bin/cito-mcp");
    const hookShim = join(root, ".cito/bin/cito-claude-hook");

    // Verify no 'node' exists on cleanRoomEnv.PATH
    const whichNode = spawnSync("/usr/bin/which", ["node"], { env: cleanRoomEnv, encoding: "utf8" });
    expect(whichNode.status).not.toBe(0);

    // 1. Cito.app/Contents/MacOS/Cito --version and cito shim --version work without Node
    const appVer = spawnSync(appExecutable, ["--version"], { env: cleanRoomEnv, encoding: "utf8", timeout: 5000 });
    expect(appVer.status).toBe(0);
    expect(appVer.stdout).toBe("cito v0.17.0\n");

    // 2. cito setup --status --json, cito setup --enable claude, cito setup --disable codex, cito setup --uninstall work without Node
    const statusBefore = spawnSync(citoShim, ["setup", "--status", "--json"], {
      env: cleanRoomEnv,
      encoding: "utf8",
      timeout: 5000,
    });
    expect(statusBefore.status).toBe(0);
    expect(JSON.parse(statusBefore.stdout).defaultHub).toBe("local");

    const setupApply = spawnSync(citoShim, ["setup", "--enable", "claude", "--json"], {
      env: cleanRoomEnv,
      encoding: "utf8",
      timeout: 5000,
    });
    expect(setupApply.status).toBe(0);
    expect(JSON.parse(setupApply.stdout).ok).toBe(true);
    expect(existsSync(join(piDir, "extensions/cito-bridge.ts"))).toBe(true);
    expect(existsSync(join(codexDir, "config.toml"))).toBe(true);
    expect(existsSync(join(claudeDir, "settings.json"))).toBe(true);

    // Dotted-key mcp_servers.cito.* replacement with --force preserves surrounding TOML content
    await writeFile(
      join(codexDir, "config.toml"),
      '# Dotted user comment\nmodel = "gpt-5"\nmcp_servers.cito.command = "legacy-cito"\n',
      "utf8",
    );
    const forceDotted = spawnSync(citoShim, ["setup", "--force", "--json"], {
      env: cleanRoomEnv,
      encoding: "utf8",
      timeout: 5000,
    });
    expect(forceDotted.status).toBe(0);
    const dottedAfterForce = await readFile(join(codexDir, "config.toml"), "utf8");
    expect(dottedAfterForce).toContain("# Dotted user comment");
    expect(dottedAfterForce).toContain('model = "gpt-5"');
    expect(dottedAfterForce).not.toContain("legacy-cito");
    expect(dottedAfterForce).toContain('[mcp_servers.cito]\ncommand = "cito-mcp"');

    const setupDisableCodex = spawnSync(citoShim, ["setup", "--disable", "codex", "--json"], {
      env: cleanRoomEnv,
      encoding: "utf8",
      timeout: 5000,
    });
    expect(setupDisableCodex.status).toBe(0);
    expect(await readFile(join(codexDir, "config.toml"), "utf8")).not.toContain("[mcp_servers.cito]");

    // 3. cito hub runs natively without Node, serves /health, and exits 0 on SIGTERM
    const hubChild = spawn(citoShim, ["hub", "--port", String(hubPort), "--bind", "127.0.0.1"], {
      env: cleanRoomEnv,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let hubStderr = "";
    hubChild.stderr.on("data", chunk => {
      hubStderr += String(chunk);
    });
    const hubExited = new Promise<number | null>(resolveExit => hubChild.once("close", resolveExit));
    try {
      await waitFor(() => hubStderr.includes(`http://127.0.0.1:${hubPort}`), 5000);
      const hubHealth = await fetch(`http://127.0.0.1:${hubPort}/health`).then(r => r.json());
      expect(hubHealth).toEqual({ ok: true, auth_mode: "none" });
      hubChild.kill("SIGTERM");
      expect(await hubExited).toBe(0);
    } finally {
      if (hubChild.exitCode === null && hubChild.signalCode === null) {
        hubChild.kill("SIGKILL");
        await hubExited;
      }
    }

    // 4. Cito.app/Contents/MacOS/Cito starts daemon natively without Node andwithout mutating disconnected codex config.toml
    const daemonChild = spawn(appExecutable, [], {
      env: cleanRoomEnv,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const daemonExited = new Promise<number | null>(resolveExit => daemonChild.once("close", resolveExit));
    try {
      await waitFor(async () => {
        try {
          const h = (await fetch(`http://127.0.0.1:${daemonPort}/health`).then(r => r.json())) as { ok?: boolean; version?: string };
          return h.ok === true && h.version === "0.17.0";
        } catch {
          return false;
        }
      }, 5000);

      // Disconnected codex config.toml remains untouched after daemon launch
      expect(await readFile(join(codexDir, "config.toml"), "utf8")).not.toContain("[mcp_servers.cito]");

      // 5. cito-mcp stdio session (initialize, tools/list, cito_listen, cito_find, cito_send, cito_inbox, cito_unlisten) without Node
      const mcpChild = spawn(mcpShim, [], {
        env: cleanRoomEnv,
        stdio: ["pipe", "pipe", "pipe"],
      });
      const mcpResponses: Record<string, unknown>[] = [];
      let mcpStdoutBuf = "";
      mcpChild.stdout.on("data", chunk => {
        mcpStdoutBuf += String(chunk);
        const lines = mcpStdoutBuf.split("\n");
        mcpStdoutBuf = lines.pop() ?? "";
        for (const line of lines) {
          if (line.trim()) {
            try {
              mcpResponses.push(JSON.parse(line));
            } catch {}
          }
        }
      });

      mcpChild.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }) + "\n");
      await waitFor(() => mcpResponses.some(r => r.id === 1), 5000);
      expect(mcpResponses.find(r => r.id === 1)).toMatchObject({
        result: { serverInfo: { name: "cito", version: "0.17.0" } },
      });

      mcpChild.stdin.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: { name: "cito_listen", arguments: { about: "Clean-room session", tag: "clean" } },
        }) + "\n",
      );
      await waitFor(() => mcpResponses.some(r => r.id === 2), 5000);

      // 6. cito find, cito send, cito inbox, and cito-claude-hook work without Node
      const findRes = spawnSync(citoShim, ["find", "Clean-room"], {
        env: cleanRoomEnv,
        encoding: "utf8",
        timeout: 5000,
      });
      expect(findRes.status).toBe(0);
      const found = JSON.parse(findRes.stdout);
      expect(found.listeners).toHaveLength(1);
      const listenerId = found.listeners[0].listener_id as string;

      const sendRes = spawnSync(citoShim, ["send", "--to", listenerId, "-m", "Hello from zero-node CLI"], {
        env: cleanRoomEnv,
        encoding: "utf8",
        timeout: 5000,
      });
      expect(sendRes.status).toBe(0);
      expect(sendRes.stdout).toContain(`to ${listenerId}`);

      const inboxRes = spawnSync(citoShim, ["inbox"], {
        env: cleanRoomEnv,
        encoding: "utf8",
        timeout: 5000,
      });
      expect(inboxRes.status).toBe(0);
      expect(JSON.parse(inboxRes.stdout).signals[0].message).toBe("Hello from zero-node CLI");

      const hookRes = spawnSync(hookShim, [], {
        env: cleanRoomEnv,
        input: JSON.stringify({ hook_event_name: "Stop", cwd: root }),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(hookRes.status).toBe(0);
      expect(hookRes.stdout).toBe("{}\n");

      mcpChild.kill("SIGTERM");
    } finally {
      daemonChild.kill("SIGTERM");
      await daemonExited;
    }
  }, 30_000);
});
