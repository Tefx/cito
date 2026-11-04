import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, writeFile } from "node:fs/promises";
import { deliverPiWake, identifyPi, piSocketPath, resolvePiCaller, setPiStatus } from "../../src/adapters/index.js";
import { isolatedState, unixServer } from "./support.js";

const exec = promisify(execFile);
describe("Pi receiver resolution and drivers", () => {
  let state: Awaited<ReturnType<typeof isolatedState>>;
  const servers: Awaited<ReturnType<typeof unixServer>>[] = [];
  beforeEach(async () => {
    state = await isolatedState();
    vi.stubEnv("CITO_STATE_DIR", state.stateDir); vi.stubEnv("CITO_CODEX_HOME", state.codexHome);
  });
  afterEach(async () => {
    await Promise.all(servers.splice(0).map(s => s.stop())); vi.unstubAllEnvs(); await state.cleanup();
  });
  const identity = (pid: number, cwd: string) => ({ ok: true, pi_pid: pid, session_id: "session", session_name: "Title",
    cwd, is_streaming: false, bridge_epoch: "epoch", human_event_count: 7 });
  async function bridge(pid: number, response: object, requests: Record<string, unknown>[] = []) {
    servers.push(await unixServer(piSocketPath(state.stateDir, pid), socket => {
      let buffer = "";
      socket.on("data", chunk => {
        buffer += chunk.toString();
        if (!buffer.includes("\n")) return;
        requests.push(JSON.parse(buffer.split("\n")[0]!));
        socket.end(JSON.stringify(response) + "\n");
      });
    }));
  }
  it("uses explicit pi_pid, caller_pid, or nested gateway parent_pid, and returns normalized receiver metadata", async () => {
    const requests: Record<string, unknown>[] = [];
    await bridge(43210, identity(43210, state.root), requests);
    expect(await resolvePiCaller({ pi_pid: 43210 }, state)).toMatchObject({ harness: "pi", pi_pid: 43210,
      title: "Title", repo: null, branch: null, human_event_count: 7 });
    await resolvePiCaller({ caller_pid: 43210 }, state);
    expect(await resolvePiCaller({ "x-gateway-caller": { parent_pid: 43210 } }, state)).toMatchObject({ harness: "pi", pi_pid: 43210 });
    expect(requests).toEqual([{ op: "identify" }, { op: "identify" }, { op: "identify" }]);
    await expect(resolvePiCaller({ pi_pid: 43211, caller_pid: 43210 }, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("walks the real ps parent chain beyond the immediate parent", async () => {
    const { stdout } = await exec("/bin/ps", ["-o", "ppid=", "-p", String(process.ppid)]);
    const ancestor = Number(stdout.trim());
    expect(ancestor).toBeGreaterThan(0);
    await bridge(ancestor, identity(ancestor, state.root));
    expect((await resolvePiCaller({}, state)).pi_pid).toBe(ancestor);
  });
  it("returns BINDING_REQUIRED only when no ancestor socket or hint exists", async () => {
    await expect(resolvePiCaller({}, state)).rejects.toMatchObject({ code: "BINDING_REQUIRED" });
    await expect(resolvePiCaller({ pi_pid: "123" }, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    await mkdir(state.stateDir + "/sockets");
    await writeFile(piSocketPath(state.stateDir, process.ppid), "stale");
    await expect(resolvePiCaller({ pi_pid: process.ppid }, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("distinguishes no active session from unavailable/malformed bridge replies", async () => {
    await bridge(43210, { ok: false, error: "SESSION_NOT_OPEN" });
    await expect(resolvePiCaller({ pi_pid: 43210 }, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
    await bridge(43211, { ...identity(43211, state.root), human_event_count: -1 });
    await expect(identifyPi(43211, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    await bridge(43212, { ...identity(43212, state.root), pi_pid: 999 });
    await expect(identifyPi(43212, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("uses a bounded identify deadline and rejects sockets closing without newline", async () => {
    servers.push(await unixServer(piSocketPath(state.stateDir, 43210), () => {}));
    await expect(identifyPi(43210, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE", message: "Pi bridge timed out" });
    servers.push(await unixServer(piSocketPath(state.stateDir, 43211), socket => socket.end('{"ok":true}')));
    await expect(identifyPi(43211, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  });
  it("sends status and deliver with exact frozen batch fields; preserves mismatch reason", async () => {
    const requests: Record<string, unknown>[] = [];
    await bridge(43210, { ok: true, delivered: true, deduplicated: true }, requests);
    const endpoint = { pi_pid: 43210, session_id: "session" };
    await setPiStatus(endpoint, "badge", state);
    expect(await deliverPiWake(endpoint, "batch", "card", state)).toEqual({ ok: true, delivered: true, deduplicated: true });
    expect(requests).toEqual([
      { op: "status", session_id: "session", badge: "badge" },
      { op: "deliver", session_id: "session", batch_id: "batch", content: "card", wake: true },
    ]);
    await bridge(43211, { ok: false, error: "SESSION_MISMATCH" });
    await expect(deliverPiWake({ ...endpoint, pi_pid: 43211 }, "batch", "card", state))
      .rejects.toMatchObject({ code: "SESSION_NOT_OPEN", bridgeError: "SESSION_MISMATCH" });
  });
  it("honors pre-aborted and mid-flight AbortSignals without leaving connections", async () => {
    let accepted!: () => void;
    const connected = new Promise<void>(resolve => { accepted = resolve; });
    servers.push(await unixServer(piSocketPath(state.stateDir, 43210), socket => {
      socket.once("data", () => accepted());
    }));
    const endpoint = { pi_pid: 43210, session_id: "session" };
    const controller = new AbortController();
    const pending = deliverPiWake(endpoint, "batch", "card", state, controller.signal);
    const rejection = expect(pending).rejects.toMatchObject({ name: "AbortError" });
    await connected; controller.abort(); await rejection;
    await expect(deliverPiWake(endpoint, "batch", "card", state, controller.signal)).rejects.toMatchObject({ name: "AbortError" });
  });
});
