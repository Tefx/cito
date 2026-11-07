import { afterEach, expect, test } from "vitest";
import { spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { isolatedState, unixServer } from "./support.js";
import { fixture, call, payload } from "../mcp/support.js";
import { CitoError, loadConfig, toCanonicalListenerId } from "../../src/shared/index.js";
import {
  ADAPTER_REGISTRY,
  BASE_PID_KEYS,
  BUILTIN_PLUGIN_IDS,
  RESERVED_CLI_FLAGS,
  SocketBridgeAdapter,
  discoverAllAvailableSessions,
  extractCallerPidRoots,
  getAdapter,
  getAllPidKeys,
  getUiHarnessMetadata,
  loadPlugins,
  registerAdapter,
  rejectedPluginIds,
  resolveCaller,
  startOptionalStdioChannel,
} from "../../src/adapters/index.js";
import type { ListenerRow } from "../../src/mcp/state.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
test("PID extraction validates explicit claims and uses nested roots without appending the parent", () => {
  expect(extractCallerPidRoots({ endpoint_pid: 11, caller_pid: 11, pi_pid: 11 })).toEqual([11]);
  expect(extractCallerPidRoots({ gateway: { parent_pid: 22, child: { endpoint_pid: 33 }, ignored: 0 }, other: { pi_pid: 44, caller_pid: 33 } })).toEqual([22, 33, 44]);
  expect(extractCallerPidRoots({ gateway: { parent_pid: -1, caller_pid: 0 } })).toEqual([process.ppid]);
  for (const pid of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, "11"]) expect(() => extractCallerPidRoots({ endpoint_pid: pid })).toThrow(expect.objectContaining({ code: "PI_BRIDGE_UNAVAILABLE" }));
  expect(() => extractCallerPidRoots({ endpoint_pid: 11, pi_pid: 12 })).toThrow(expect.objectContaining({ code: "INVALID_INPUT" }));
});
test("a nested explicit root cannot accidentally bind to a bridge on process.ppid", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const server = await unixServer(join(f.stateDir, `sockets/aider-pid-${process.ppid}.sock`), socket => socket.once("data", () => socket.end(JSON.stringify({
    ok: true, endpoint_pid: process.ppid, session_id: "parent", session_name: "Parent", cwd: f.root,
  }) + "\n"))); cleanups.push(() => server.stop());
  const config = loadConfig(f.env);
  expect((await resolveCaller({}, config)).harness).toBe("aider");
  await expect(resolveCaller({ gateway: { parent_pid: 2147483647 } }, config)).rejects.toMatchObject({ code: "BINDING_REQUIRED" });
});
test("registered explicit identity hooks receive generic context and conflicting driver/PID claims fail", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env), caller = { harness: "editor", session_id: "s", cwd: f.root, title: "Editor", repo: null, branch: null };
  let received: unknown;
  registerAdapter({ ...new SocketBridgeAdapter("editor"), id: "editor",
    probe: async () => ({ status: "alive" }), deliver: async () => "delivered_unconfirmed",
    detectExplicitCaller: async context => { received = context; return context.meta.editor_session ? caller : null; } });
  try {
    const input = { meta: { editor_session: "s" }, headers: { "trace-id": "one" }, httpTransport: true, httpClientPid: process.pid };
    expect(await resolveCaller(input, config)).toEqual(caller);
    expect(received).toMatchObject(input);
    await expect(resolveCaller({ ...input, meta: { ...input.meta, endpoint_pid: process.pid } }, config)).rejects.toMatchObject({ code: "INVALID_INPUT" });
    await expect(resolveCaller({ ...input, meta: { ...input.meta, caller_pid: process.pid } }, config)).rejects.toMatchObject({ code: "INVALID_INPUT" });
    await expect(resolveCaller({ ...input, meta: { ...input.meta, codexThreadId: "other" } }, config)).rejects.toMatchObject({ code: "INVALID_INPUT" });
  } finally { ADAPTER_REGISTRY.delete("editor"); }
});
test("socket probes distinguish matching, switched, definitive closure, dead process and transient failure", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup()); const config = loadConfig(f.env);
  let response: object = { ok: true, endpoint_pid: process.pid, session_id: "s", session_name: "S", cwd: f.root };
  const server = await unixServer(join(f.stateDir, `sockets/aider-pid-${process.pid}.sock`), socket => socket.once("data", () => socket.end(JSON.stringify(response) + "\n")));
  cleanups.push(() => server.stop());
  const row = { endpoint_pid: process.pid, session_id: "s" } as ListenerRow, adapter = new SocketBridgeAdapter("aider");
  expect(await adapter.probe(row, config, new AbortController().signal)).toEqual({ status: "alive" });
  response = { ok: true, endpoint_pid: process.pid, session_id: "other", session_name: "S", cwd: f.root };
  expect(await adapter.probe(row, config, new AbortController().signal)).toEqual({ status: "switched" });
  response = { ok: false, error: "SESSION_NOT_OPEN" };
  expect(await adapter.probe(row, config, new AbortController().signal)).toEqual({ status: "closed_definitive" });
  response = { ok: false, error: "TEMPORARY" };
  expect((await adapter.probe(row, config, new AbortController().signal)).status).toBe("transient_error");
  expect((await adapter.probe({ ...row, endpoint_pid: 2147483647 }, config, new AbortController().signal)).status).toBe("closed_definitive");
  expect((await new SocketBridgeAdapter("unreachable").probe(row, config, new AbortController().signal)).status).toBe("transient_error");
  expect(getAdapter("invalid:id")).toBeUndefined();
});
test("offline PID fallback accepts one retained pair across hubs/statuses and rejects zero or multiple pairs", async () => {
  const f = await fixture(); cleanups.push(() => f.close());
  const enrolled = await call(f.service, "cito_listen", { about: "cached" }, f.pi.meta), id = toCanonicalListenerId(enrolled.listener_id);
  f.service.state.run("UPDATE local_listeners SET endpoint_pid = 2147483647, status = 'UNLISTENED'");
  f.service.state.run("INSERT INTO local_listeners SELECT 'other-hub', listener_id, public_id, tag, principal_id, owner_user, host_id, host_name, harness, session_id, endpoint_pid, cwd, repo, branch, title, about, scope, 'FENCED_INACTIVE', 9, owner_token, persisted_seq, consecutive_probe_failures, replay_gap_detected, replay_gap_range, dispatcher_token, lease_until_ms FROM local_listeners WHERE hub = 'default'");
  const offlineMeta = { endpoint_pid: 2147483647 };
  expect(await call(f.service, "cito_inbox", {}, offlineMeta)).toMatchObject({ listener_id: enrolled.listener_id, signals: [] });
  expect(payload(await f.service.call("cito_inbox", {}, { endpoint_pid: 2147483646 }))).toMatchObject({ error: { code: "PI_BRIDGE_UNAVAILABLE" } });
  f.service.state.run("UPDATE local_listeners SET session_id = 'another' WHERE hub = 'other-hub'");
  expect(payload(await f.service.call("cito_inbox", {}, offlineMeta))).toMatchObject({ error: { code: "PI_BRIDGE_UNAVAILABLE" } });
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
});

test("external plugins load from stateDir/plugins, enforce reserved IDs, directory-name match, forced custom tier, and rejectedPluginIds socket blocking", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const pluginsRoot = join(f.stateDir, "plugins");
  await mkdir(pluginsRoot, { recursive: true });

  expect([...BUILTIN_PLUGIN_IDS]).toEqual(["pi", "codex", "claude"]);
  expect(BASE_PID_KEYS).toEqual(["endpoint_pid", "caller_pid"]);
  expect(getAllPidKeys()).toContain("pi_pid");
  expect(() => registerAdapter(new SocketBridgeAdapter("pi"))).toThrow(
    expect.objectContaining({ code: "INVALID_INPUT" }),
  );

  // 1. Valid manifest-only plugin (forced to tier: "custom" even if manifest declares "stable")
  await mkdir(join(pluginsRoot, "cursor"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "cursor/plugin.json"),
    JSON.stringify({
      id: "cursor",
      displayName: "Cursor",
      tier: "stable",
      sfSymbol: "cursorarrow.rays",
      tint: "purple",
      cliFlag: true,
    }),
  );

  // 2. Valid index.ts plugin importing @tefx/cito/plugin-sdk
  await mkdir(join(pluginsRoot, "windsurf"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "windsurf/plugin.json"),
    JSON.stringify({ id: "windsurf", displayName: "Windsurf", sfSymbol: "wind", tint: "green" }),
  );
  await writeFile(
    join(pluginsRoot, "windsurf/index.ts"),
    `import { SocketBridgeAdapter, type PluginManifest } from "@tefx/cito/plugin-sdk";
export default (manifest: PluginManifest) => new SocketBridgeAdapter(manifest.id, { manifest, pidKeys: ["windsurf_pid"] });\n`,
  );

  // 3. Reserved built-in override attempt ("pi") -> rejected with warning, built-in preserved
  await mkdir(join(pluginsRoot, "pi"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "pi/plugin.json"),
    JSON.stringify({ id: "pi", displayName: "Fake Pi" }),
  );

  // 4. Directory-name mismatch ("wrongdir" with id "mismatch") -> rejected and blocked from socket fallback
  await mkdir(join(pluginsRoot, "wrongdir"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "wrongdir/plugin.json"),
    JSON.stringify({ id: "mismatch", displayName: "Mismatch" }),
  );

  // 5. Invalid manifest schema ("badschema") -> rejected and blocked from socket fallback
  await mkdir(join(pluginsRoot, "badschema"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "badschema/plugin.json"),
    JSON.stringify({ id: "badschema", displayName: "", invalidField: true }),
  );

  // 6. Throwing entry script ("brokenentry") -> rejected and blocked from socket fallback
  await mkdir(join(pluginsRoot, "brokenentry"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "brokenentry/plugin.json"),
    JSON.stringify({ id: "brokenentry", displayName: "Broken" }),
  );
  await writeFile(
    join(pluginsRoot, "brokenentry/index.ts"),
    "throw new Error('Plugin init boom');\n",
  );

  // 7. Reserved CLI flag ("setup") with cliFlag: true -> cliFlag stripped with warning
  await mkdir(join(pluginsRoot, "setup"), { recursive: true });
  await writeFile(
    join(pluginsRoot, "setup/plugin.json"),
    JSON.stringify({ id: "setup", displayName: "Setup Harness", cliFlag: true }),
  );

  const warnings: string[] = [];
  const config = loadConfig(f.env);
  await loadPlugins(config, { diagnostic: msg => warnings.push(msg) });

  expect(warnings.some(w => w.includes("reserved built-in plugin 'pi'"))).toBe(true);
  expect(warnings.some(w => w.includes("does not match directory name 'wrongdir'"))).toBe(true);
  expect(warnings.some(w => w.includes("badschema"))).toBe(true);
  expect(warnings.some(w => w.includes("Plugin init boom"))).toBe(true);
  expect(warnings.some(w => w.includes("reserved CLI flag '--setup'"))).toBe(true);

  // Built-in pi remains intact
  expect(ADAPTER_REGISTRY.get("pi")?.manifest?.displayName).toBe("Pi");
  expect(ADAPTER_REGISTRY.get("pi")?.manifest?.tier).toBe("stable");

  // External plugins loaded and forced to tier: "custom"
  const cursor = ADAPTER_REGISTRY.get("cursor");
  expect(cursor).toBeInstanceOf(SocketBridgeAdapter);
  expect(cursor?.manifest).toMatchObject({ id: "cursor", displayName: "Cursor", tier: "custom", tint: "purple", cliFlag: true });

  const windsurf = ADAPTER_REGISTRY.get("windsurf");
  expect(windsurf?.manifest).toMatchObject({ id: "windsurf", displayName: "Windsurf", tier: "custom", tint: "green" });
  expect(getAllPidKeys()).toContain("windsurf_pid");

  expect(ADAPTER_REGISTRY.get("setup")?.manifest?.cliFlag).toBe(false);
  expect([...RESERVED_CLI_FLAGS]).toEqual(["hub", "team", "scope", "tag", "harness", "session", "help", "version", "setup"]);

  // Rejected plugins cannot fall through to auto-instantiated socket bridges
  for (const rejected of ["wrongdir", "mismatch", "badschema", "brokenentry"]) {
    expect(rejectedPluginIds.has(rejected)).toBe(true);
    expect(getAdapter(rejected)).toBeUndefined();
  }
  const brokenSock = await unixServer(join(f.stateDir, `sockets/brokenentry-pid-${process.pid}.sock`), socket => {
    socket.once("data", () => socket.end(JSON.stringify({
      ok: true, endpoint_pid: process.pid, session_id: "broken-s", session_name: "Broken", cwd: f.root,
    }) + "\n"));
  });
  cleanups.push(() => brokenSock.stop());
  const discovered = await discoverAllAvailableSessions(config, []);
  expect(discovered.some(s => s.harness === "brokenentry")).toBe(false);

  const uiHarnesses = getUiHarnessMetadata();
  expect(uiHarnesses.map(h => [h.id, h.tier, h.order])).toEqual([
    ["pi", "stable", 0],
    ["codex", "stable", 1],
    ["claude", "experimental", 2],
    ["cursor", "custom", 3],
    ["setup", "custom", 3],
    ["windsurf", "custom", 3],
  ]);
});

test("per-plugin 1500ms deadlines isolate hanging discovery and caller detection, and startOptionalStdioChannel enforces single active channel", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);

  // 1. Hanging discoverAvailableSessions times out in ~1500ms without starving fast plugins, and >16 total candidates across 3+ harnesses are retained (up to 8 per harness)
  registerAdapter({
    id: "hanging",
    discoverAvailableSessions: () => new Promise(() => {}),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  registerAdapter({
    id: "alpha",
    discoverAvailableSessions: async () => Array.from({ length: 10 }, (_, i) => ({
      harness: "alpha",
      session_id: `alpha-${i}`,
      title: `Alpha ${i}`,
      cwd: f.root,
    })),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  registerAdapter({
    id: "beta",
    discoverAvailableSessions: async () => Array.from({ length: 10 }, (_, i) => ({
      harness: "beta",
      session_id: `beta-${i}`,
      title: `Beta ${i}`,
      cwd: f.root,
    })),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  registerAdapter({
    id: "gamma",
    discoverAvailableSessions: async () => Array.from({ length: 10 }, (_, i) => ({
      harness: "gamma",
      session_id: `gamma-${i}`,
      title: `Gamma ${i}`,
      cwd: f.root,
    })),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  cleanups.push(async () => {
    for (const id of ["hanging", "alpha", "beta", "gamma", "hangcaller", "chan1", "chan2"]) {
      ADAPTER_REGISTRY.delete(id);
    }
  });

  const startMs = performance.now();
  const candidates = await discoverAllAvailableSessions(config, []);
  const elapsedMs = performance.now() - startMs;
  expect(elapsedMs).toBeLessThan(2500);
  expect(candidates.filter(c => c.harness === "alpha")).toHaveLength(8);
  expect(candidates.filter(c => c.harness === "beta")).toHaveLength(8);
  expect(candidates.filter(c => c.harness === "gamma")).toHaveLength(8);
  expect(candidates.length).toBeGreaterThanOrEqual(24);

  // 2. Hanging detectExplicitCaller fails caller binding after 1500ms rather than silently falling through to implicit PID socket
  const sock = await unixServer(join(f.stateDir, `sockets/pi-pid-${process.ppid}.sock`), socket => {
    socket.once("data", () => socket.end(JSON.stringify({
      ok: true, endpoint_pid: process.ppid, session_id: "ppid-session", session_name: "PPID", cwd: f.root,
    }) + "\n"));
  });
  cleanups.push(() => sock.stop());
  registerAdapter({
    id: "hangcaller",
    detectExplicitCaller: () => new Promise(() => {}),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  await expect(resolveCaller({}, config)).rejects.toMatchObject({
    code: "PI_BRIDGE_UNAVAILABLE",
    message: expect.stringContaining("Plugin 'hangcaller' caller detection timed out"),
  });
  ADAPTER_REGISTRY.delete("hangcaller");

  // 3. Single-stdio-channel guard closes all channels if >1 plugin activates a channel in the same process
  let closed1 = false, closed2 = false;
  registerAdapter({
    id: "chan1",
    startStdioChannel: async () => ({
      socketPath: "/tmp/chan1.sock",
      sessionId: async () => "s1",
      forward: msg => msg,
      close: async () => { closed1 = true; },
    }),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  registerAdapter({
    id: "chan2",
    startStdioChannel: async () => ({
      socketPath: "/tmp/chan2.sock",
      sessionId: async () => "s2",
      forward: msg => msg,
      close: async () => { closed2 = true; },
    }),
    probe: async () => ({ status: "alive" }),
    deliver: async () => "delivered",
  });
  expect(await startOptionalStdioChannel(config, { send: async () => {} })).toBeUndefined();
  expect(closed1).toBe(true);
  expect(closed2).toBe(true);
}, 15_000);

test("CLI --harness and --session validate orphan --session, unknown/rejected harnesses, and conflicting flags", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const runCli = (args: string[]) => spawnSync(
    process.execPath,
    [resolve("bin/cito-mcp"), ...args],
    { cwd: f.root, env: f.env, encoding: "utf8", timeout: 5000 },
  );

  const orphanSession = runCli(["listen", "--session", "s1"]);
  expect(orphanSession.status).toBe(1);
  expect(orphanSession.stderr).toContain("--session requires --harness");

  const unknownHarness = runCli(["listen", "--harness", "nonexistent"]);
  expect(unknownHarness.status).toBe(1);
  expect(unknownHarness.stderr).toContain("Unknown harness: nonexistent");

  const conflictHarness = runCli(["listen", "--codex", "--harness", "pi"]);
  expect(conflictHarness.status).toBe(1);
  expect(conflictHarness.stderr).toContain("Conflicting harness flags");

  const conflictSession = runCli(["listen", "--claude=session-a", "--session", "session-b"]);
  expect(conflictSession.status).toBe(1);
  expect(conflictSession.stderr).toContain("Conflicting session flags");
});
