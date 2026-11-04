import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createConnection } from "node:net";
import { stat } from "node:fs/promises";
import citoBridge, { type BridgeAPI, type BridgeContext } from "../../src/pi-extension/cito-bridge.js";
import { deliverPiWake, identifyPi, piSocketPath, setPiStatus } from "../../src/adapters/index.js";
import { isolatedState } from "./support.js";

describe("self-contained Pi bridge over real Unix sockets", () => {
  let state: Awaited<ReturnType<typeof isolatedState>>;
  let ctx: BridgeContext;
  let currentId: string | undefined;
  let name: string | undefined;
  let idle: boolean;
  let api: BridgeAPI;
  let handlers: Map<string, (event: { source?: string }, ctx: BridgeContext) => void | Promise<void>>;
  const send = vi.fn<BridgeAPI["sendMessage"]>();
  const status = vi.fn<BridgeContext["ui"]["setStatus"]>();
  const emit = async (event: string, value = {}) => { await handlers.get(event)!(value, ctx); };
  beforeEach(async () => {
    state = await isolatedState();
    vi.stubEnv("CITO_STATE_DIR", state.stateDir);
    vi.stubEnv("CITO_CODEX_HOME", state.codexHome);
    currentId = "session-a"; name = "Human title"; idle = true;
    ctx = { cwd: state.root, isIdle: () => idle,
      sessionManager: { getSessionId: () => currentId, getSessionName: () => name }, ui: { setStatus: status } };
    handlers = new Map(); send.mockReset(); status.mockReset();
    api = { on: (event, handler) => handlers.set(event, handler), sendMessage: send };
    citoBridge(api);
  });
  afterEach(async () => {
    await emit("session_shutdown");
    vi.unstubAllEnvs();
    await state.cleanup();
  });
  it("defers socket creation until session_start; identifies and closes with private permissions", async () => {
    const path = piSocketPath(state.stateDir, process.pid);
    await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
    await emit("session_start");
    expect((await stat(path)).mode & 0o777).toBe(0o600);
    expect((await stat(state.stateDir + "/sockets")).mode & 0o777).toBe(0o700);
    const first = await identifyPi(process.pid, state);
    expect(first).toMatchObject({ pi_pid: process.pid, session_id: "session-a", session_name: "Human title",
      cwd: state.root, is_streaming: false, human_event_count: 0 });
    expect(first.bridge_epoch).toMatch(/^[0-9a-f-]{36}$/);
    idle = false; name = "Renamed";
    await emit("session_info_changed");
    expect(await identifyPi(process.pid, state)).toMatchObject({ session_name: "Renamed", is_streaming: true,
      bridge_epoch: first.bridge_epoch });
    await emit("session_shutdown");
    await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
  });
  it("counts interactive input only and resets on session identity change", async () => {
    await emit("session_start");
    await emit("input", { source: "extension" });
    await emit("input", { source: "rpc" });
    await emit("input", { source: "interactive" });
    await emit("input", { source: "interactive" });
    const first = await identifyPi(process.pid, state);
    expect(first.human_event_count).toBe(2);
    currentId = "session-b";
    await emit("session_start");
    expect(await identifyPi(process.pid, state)).toMatchObject({ session_id: "session-b", human_event_count: 0,
      bridge_epoch: first.bridge_epoch });
    currentId = undefined;
    await expect(identifyPi(process.pid, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN", bridgeError: "SESSION_NOT_OPEN" });
  });
  it("checks session identity before either status update or duplicate delivery", async () => {
    await emit("session_start");
    const endpoint = { pi_pid: process.pid, session_id: "session-a" };
    await setPiStatus(endpoint, "📡 cito: listener", state);
    expect(status).toHaveBeenLastCalledWith("cito", "📡 cito: listener");
    await setPiStatus(endpoint, null, state);
    expect(status).toHaveBeenLastCalledWith("cito", undefined);
    await deliverPiWake(endpoint, "batch", "hello", 1, state);
    const wrong = { ...endpoint, session_id: "old" };
    await expect(setPiStatus(wrong, "no", state)).rejects.toMatchObject({ bridgeError: "SESSION_MISMATCH" });
    await expect(deliverPiWake(wrong, "batch", "no", 1, state)).rejects.toMatchObject({ bridgeError: "SESSION_MISMATCH" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(status).toHaveBeenCalledTimes(2);
  });
  it("deduplicates concurrent retries and evicts only the oldest of 200 IDs", async () => {
    await emit("session_start");
    const endpoint = { pi_pid: process.pid, session_id: "session-a" };
    const deliver = (id: string) => deliverPiWake(endpoint, id, "card", 2, state);
    const replies = await Promise.all([deliver("batch-0"), deliver("batch-0")]);
    expect(replies.map(r => r.deduplicated).sort()).toEqual([false, true]);
    expect(send).toHaveBeenCalledWith({ customType: "cito-notification", content: "card", display: true },
      { triggerTurn: true, deliverAs: "followUp" });
    for (let i = 1; i < 200; i++) await deliver("batch-" + i);
    expect((await deliver("batch-0")).deduplicated).toBe(true);
    await deliver("batch-200");
    expect((await deliver("batch-1")).deduplicated).toBe(true);
    expect((await deliver("batch-0")).deduplicated).toBe(false);
    expect(send).toHaveBeenCalledTimes(202);
  });
  it("does not record failed sendMessage calls; retries the exact batch", async () => {
    await emit("session_start");
    const endpoint = { pi_pid: process.pid, session_id: "session-a" };
    send.mockImplementationOnce(() => { throw new Error("Pi unavailable"); });
    await expect(deliverPiWake(endpoint, "retry", "card", 1, state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    expect(await deliverPiWake(endpoint, "retry", "card", 1, state)).toMatchObject({ deduplicated: false });
    expect(await deliverPiWake(endpoint, "retry", "card", 1, state)).toMatchObject({ deduplicated: true });
    expect(send).toHaveBeenCalledTimes(2);
  });
  it("honors wake:false, echoes request IDs, and handles fragmented newline framing once", async () => {
    await emit("session_start");
    const reply = await new Promise<string>((resolve, reject) => {
      const socket = createConnection(piSocketPath(state.stateDir, process.pid));
      socket.on("error", reject); let output = "";
      socket.on("data", chunk => { output += chunk.toString(); });
      socket.on("end", () => resolve(output));
      socket.on("connect", () => {
        socket.write('{"id":"x","op":"deliver","session_id":"session-a",');
        socket.write('"batch_id":"passive","wake":false,"content":"card"}\n{"op":"identify"}\n');
      });
    });
    expect(reply.split("\n").filter(Boolean)).toHaveLength(1);
    expect(JSON.parse(reply)).toEqual({ id: "x", ok: true, delivered: true, deduplicated: false });
    expect(send).toHaveBeenCalledWith(expect.anything(), { triggerTurn: false, deliverAs: "followUp" });
  });
  it("generates a different epoch for a new extension instance", async () => {
    await emit("session_start");
    const old = await identifyPi(process.pid, state);
    await emit("session_shutdown");
    citoBridge(api);
    await emit("session_start");
    expect((await identifyPi(process.pid, state)).bridge_epoch).not.toBe(old.bridge_epoch);
  });
});
