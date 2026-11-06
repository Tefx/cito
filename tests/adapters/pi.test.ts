import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, utimes, writeFile } from "node:fs/promises";
import { bridgeSocketPath, identifyBridge, resolveCaller, SocketBridgeAdapter, type DeliverContext } from "../../src/adapters/index.js";
import { loadConfig } from "../../src/shared/index.js";
import type { ListenerRow } from "../../src/mcp/state.js";
import { isolatedState, unixServer } from "./support.js";

const exec = promisify(execFile);
describe("socket receiver resolution and drivers", () => {
  let state: Awaited<ReturnType<typeof isolatedState>>;
  const servers: Awaited<ReturnType<typeof unixServer>>[] = [];
  beforeEach(async () => { state = await isolatedState(); });
  afterEach(async () => { await Promise.all(servers.splice(0).map(s => s.stop())); await state.cleanup(); });
  const identity = (pid: number, cwd: string) => ({ ok: true, pi_pid: pid, session_id: "session", session_name: "Title", cwd });
  const path = (pid: number) => bridgeSocketPath(state.stateDir, "pi", pid);
  async function bridge(pid: number, response: object, requests: Record<string, unknown>[] = []) {
    servers.push(await unixServer(path(pid), socket => {
      let buffer = "";
      socket.on("data", chunk => {
        buffer += chunk.toString(); if (!buffer.includes("\n")) return;
        requests.push(JSON.parse(buffer.split("\n")[0]!)); socket.end(JSON.stringify(response) + "\n");
      });
    }));
  }
  const context = (signal = new AbortController().signal): DeliverContext => ({
    batchId: "batch", card: "card", config: loadConfig(state.env), signal, queueWritten: false,
    markQueueWritten() {}, runEffect: async submit => { await submit(); return true; },
  });
  it("uses explicit pi_pid, caller_pid, or nested gateway parent_pid, and returns normalized receiver metadata", async () => {
    const requests: Record<string, unknown>[] = []; await bridge(43210, identity(43210, state.root), requests);
    expect(await resolveCaller({ pi_pid: 43210 }, loadConfig(state.env))).toMatchObject({ harness: "pi", endpoint_pid: 43210, title: "Title", repo: null, branch: null });
    await resolveCaller({ caller_pid: 43210 }, loadConfig(state.env));
    expect(await resolveCaller({ gateway: { parent_pid: 43210 } }, loadConfig(state.env))).toMatchObject({ harness: "pi", endpoint_pid: 43210 });
    expect(requests).toEqual([{ op: "identify" }, { op: "identify" }, { op: "identify" }]);
    await expect(resolveCaller({ pi_pid: 43211, caller_pid: 43210 }, loadConfig(state.env))).rejects.toMatchObject({ code: "INVALID_INPUT" });
  });
  it("walks the real ps parent chain beyond the immediate parent", async () => {
    const { stdout } = await exec("/bin/ps", ["-o", "ppid=", "-p", String(process.ppid)]), ancestor = Number(stdout.trim());
    expect(ancestor).toBeGreaterThan(0); await bridge(ancestor, identity(ancestor, state.root));
    expect((await resolveCaller({}, loadConfig(state.env))).endpoint_pid).toBe(ancestor);
  });
  it("returns BINDING_REQUIRED for absent or unopen implicit ancestors and propagates explicit PID failure", async () => {
    const config = loadConfig(state.env);
    await expect(resolveCaller({}, config, { requireOpen: false })).rejects.toMatchObject({ code: "BINDING_REQUIRED" });
    await expect(resolveCaller({ pi_pid: "123" }, config)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    await mkdir(state.stateDir + "/sockets"); await writeFile(path(process.ppid), "stale");
    await expect(resolveCaller({}, config, { requireOpen: false })).rejects.toMatchObject({ code: "BINDING_REQUIRED" });
    await expect(resolveCaller({ pi_pid: process.ppid }, config)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("distinguishes no active session at the bridge while explicit caller claims fail visibly", async () => {
    await bridge(43210, { ok: false, error: "SESSION_NOT_OPEN" });
    await expect(identifyBridge("pi", 43210, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
    await expect(resolveCaller({ pi_pid: 43210 }, loadConfig(state.env))).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    await bridge(43211, { ...identity(43211, state.root), session_id: "" });
    await expect(identifyBridge("pi", 43211, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    await bridge(43212, { ...identity(43212, state.root), pi_pid: 999 });
    await expect(identifyBridge("pi", 43212, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("uses a bounded identify deadline and rejects sockets closing without newline", async () => {
    servers.push(await unixServer(path(43210), () => {}));
    await expect(identifyBridge("pi", 43210, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE", message: "Socket bridge timed out" });
    servers.push(await unixServer(path(43211), socket => socket.end('{"ok":true}')));
    await expect(identifyBridge("pi", 43211, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("sends status and deliver with exact frozen batch fields and classifies session mismatch", async () => {
    const requests: Record<string, unknown>[] = []; await bridge(43210, { ok: true, delivered: true, deduplicated: true }, requests);
    const row = { endpoint_pid: 43210, session_id: "session" } as ListenerRow, adapter = new SocketBridgeAdapter("pi");
    await adapter.updateBadge(row, "badge", loadConfig(state.env)); expect(await adapter.deliver(row, context())).toBe("delivered");
    expect(requests).toEqual([{ op: "status", session_id: "session", badge: "badge" }, { op: "deliver", session_id: "session", batch_id: "batch", content: "card", wake: true }]);
    await bridge(43211, { ok: false, error: "SESSION_MISMATCH" });
    expect(await adapter.deliver({ ...row, endpoint_pid: 43211 }, context())).toBe("session_switched");
  });
  it("honors pre-aborted and mid-flight AbortSignals without leaving connections", async () => {
    let accepted!: () => void; const connected = new Promise<void>(resolve => { accepted = resolve; });
    servers.push(await unixServer(path(43210), socket => { socket.once("data", () => accepted()); }));
    const row = { endpoint_pid: 43210, session_id: "session" } as ListenerRow, adapter = new SocketBridgeAdapter("pi"), controller = new AbortController();
    const pending = adapter.deliver(row, context(controller.signal)), rejection = expect(pending).rejects.toMatchObject({ name: "AbortError" });
    await connected; controller.abort(); await rejection;
    await expect(adapter.deliver(row, context(controller.signal))).rejects.toMatchObject({ name: "AbortError" });
  });
  it("discovers live socket sessions ordered by mtimeMs while isolating dead PIDs, non-sockets, and slow bridges, and fences changed sessions", async () => {
    const config = loadConfig(state.env), adapter = new SocketBridgeAdapter("pi");
    expect(await adapter.discoverAvailableSessions(config)).toEqual([]);
    await bridge(process.pid, { ok: true, endpoint_pid: process.pid, session_id: "live-current", session_name: "Current Session", cwd: state.root });
    await bridge(process.ppid, { ok: true, endpoint_pid: process.ppid, session_id: "live-parent", session_name: "Parent Session", cwd: state.root });
    // Dead PID socket and non-socket file are ignored without failing discovery
    await bridge(2147483647, { ok: true, endpoint_pid: 2147483647, session_id: "dead-pid", session_name: "Dead", cwd: state.root });
    await writeFile(path(99999), "not-a-socket");
    const nowSec = Math.floor(Date.now() / 1000);
    await utimes(path(process.pid), nowSec - 10, nowSec - 10);
    await utimes(path(process.ppid), nowSec, nowSec);
    const discovered = await adapter.discoverAvailableSessions(config);
    expect(discovered.map(s => ({ session_id: s.session_id, title: s.title, endpoint_pid: s.endpoint_pid }))).toEqual([
      { session_id: "live-parent", title: "Parent Session", endpoint_pid: process.ppid },
      { session_id: "live-current", title: "Current Session", endpoint_pid: process.pid },
    ]);
    // Identity fencing on resolveCaller verifies expected_harness and expected_session_id
    expect(await resolveCaller({ endpoint_pid: process.pid, expected_harness: "pi", expected_session_id: "live-current" }, config))
      .toMatchObject({ harness: "pi", session_id: "live-current", endpoint_pid: process.pid });
    await expect(resolveCaller({ endpoint_pid: process.pid, expected_harness: "pi", expected_session_id: "stale-session" }, config))
      .rejects.toMatchObject({ code: "SESSION_NOT_OPEN", message: "Session changed since discovery" });
    await expect(resolveCaller({ endpoint_pid: process.pid, expected_harness: "custom", expected_session_id: "live-current" }, config))
      .rejects.toMatchObject({ code: "SESSION_NOT_OPEN", message: "Session changed since discovery" });
  });
});
