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
  const register = vi.fn<NonNullable<BridgeAPI["registerMessageRenderer"]>>();
  const emit = async (event: string, value = {}) => { await handlers.get(event)!(value, ctx); };
  beforeEach(async () => {
    state = await isolatedState();
    vi.stubEnv("CITO_STATE_DIR", state.stateDir);
    vi.stubEnv("CITO_CODEX_HOME", state.codexHome);
    currentId = "session-a"; name = "Human title"; idle = true;
    ctx = { cwd: state.root, isIdle: () => idle,
      sessionManager: { getSessionId: () => currentId, getSessionName: () => name }, ui: { setStatus: status } };
    handlers = new Map(); send.mockReset(); status.mockReset(); register.mockReset();
    api = { on: (event, handler) => handlers.set(event, handler), sendMessage: send, registerMessageRenderer: register };
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
  it("preserves per-session human counts and batch dedup through A to B to A", async () => {
    await emit("session_start");
    const a = { pi_pid: process.pid, session_id: "session-a" };
    await emit("input", { source: "interactive" }); await emit("input", { source: "interactive" });
    const original = await identifyPi(process.pid, state);
    expect(await deliverPiWake(a, "same-batch", "A card", state)).toMatchObject({ deduplicated: false });
    currentId = "session-b"; await emit("session_tree");
    await emit("input", { source: "interactive" });
    expect(await deliverPiWake({ ...a, session_id: "session-b" }, "same-batch", "B card", state)).toMatchObject({ deduplicated: false });
    currentId = "session-a"; await emit("session_tree");
    expect(await identifyPi(process.pid, state)).toMatchObject({ bridge_epoch: original.bridge_epoch, human_event_count: 2 });
    expect(await deliverPiWake(a, "same-batch", "A card", state)).toMatchObject({ deduplicated: true });
    await emit("input", { source: "interactive" });
    expect((await identifyPi(process.pid, state)).human_event_count).toBe(3);
    currentId = "session-b"; await emit("session_tree");
    expect((await identifyPi(process.pid, state)).human_event_count).toBe(1);
    expect(send.mock.calls.map(call => call[0].content)).toEqual(["A card", "B card"]);
  });
  it("checks session identity before either status update or duplicate delivery", async () => {
    await emit("session_start");
    const endpoint = { pi_pid: process.pid, session_id: "session-a" };
    await setPiStatus(endpoint, "🔔 listener", state);
    expect(status).toHaveBeenLastCalledWith("cito", "🔔 listener");
    await setPiStatus(endpoint, null, state);
    expect(status).toHaveBeenLastCalledWith("cito", undefined);
    await deliverPiWake(endpoint, "batch", "hello", state);
    const wrong = { ...endpoint, session_id: "old" };
    await expect(setPiStatus(wrong, "no", state)).rejects.toMatchObject({ bridgeError: "SESSION_MISMATCH" });
    await expect(deliverPiWake(wrong, "batch", "no", state)).rejects.toMatchObject({ bridgeError: "SESSION_MISMATCH" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(status.mock.calls).toEqual([["cito", undefined], ["cito", "🔔 listener"], ["cito", undefined]]);
  });
  it("deduplicates concurrent retries and evicts only the oldest of 200 IDs", async () => {
    await emit("session_start");
    const endpoint = { pi_pid: process.pid, session_id: "session-a" };
    const deliver = (id: string) => deliverPiWake(endpoint, id, "card", state);
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
    await expect(deliverPiWake(endpoint, "retry", "card", state)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    expect(await deliverPiWake(endpoint, "retry", "card", state)).toMatchObject({ deduplicated: false });
    expect(await deliverPiWake(endpoint, "retry", "card", state)).toMatchObject({ deduplicated: true });
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
  it("clears stale badges immediately on session changes, including switches discovered by identify", async () => {
    await emit("session_start");
    await setPiStatus({ pi_pid: process.pid, session_id: "session-a" }, "🔔 old", state);
    currentId = "session-b"; await emit("session_tree");
    expect(status).toHaveBeenLastCalledWith("cito", undefined);
    const calls = status.mock.calls.length;
    await emit("session_info_changed");
    expect(status).toHaveBeenCalledTimes(calls);
    await setPiStatus({ pi_pid: process.pid, session_id: "session-b" }, "🔔 new", state);
    currentId = "session-a";
    await identifyPi(process.pid, state);
    expect(status).toHaveBeenLastCalledWith("cito", undefined);
  });
  it("renders new and legacy notifications with themed bars and expandable acknowledgement details", () => {
    expect(register).toHaveBeenCalledWith("cito-notification", expect.any(Function));
    const renderer = register.mock.calls[0]![1];
    const theme = { fg: vi.fn((_color: string, text: string) => text), bg: vi.fn((_color: string, text: string) => text),
      bold: vi.fn((text: string) => text) };
    const content = '🔔 Cito · alice@mac [↩ repo-pbtjxcxkgw3y]\n\nHello\n\n↳ Ack: cito_inbox(ack_ids: ["sig_1"])';
    const collapsed = renderer({ content }, { expanded: false, outputPad: 1 }, theme);
    expect(collapsed.render(80).map(line => line.trimEnd())).toEqual([
      ' 🔔 Cito · alice@mac [↩ repo-pbtjxcxkgw3y]', ' │', ' │ Hello',
    ]);
    expect(theme.bg).toHaveBeenCalledWith("customMessageBg", expect.any(String));
    expect(theme.fg).toHaveBeenCalledWith("accent", expect.stringContaining("🔔 Cito"));
    expect(theme.fg).toHaveBeenCalledWith("borderAccent", "│ ");
    expect(theme.bold).toHaveBeenCalled();
    const expanded = renderer({ content: [{ type: "text", text: content }] }, { expanded: true }, theme);
    expect(expanded.render(80).join("\n")).toContain('↳ Ack: cito_inbox(ack_ids: ["sig_1"])');
    expect(theme.fg).toHaveBeenCalledWith("dim", expect.stringContaining("↳ Ack:"));
    const legacy = '<cito_signal count="1"><signal id="sig_1" from="repo-pbtjxcxkgw3y" from_user="alice" from_host="mac.local" from_title="Build">\nLegacy message\n</signal>\nPlease call cito_inbox(ack_ids: ["sig_1"]) to acknowledge.</cito_signal>';
    const old = renderer({ content: legacy }, { expanded: false }, theme).render(80).join("\n");
    expect(old).toContain("alice@mac · Build [↩ repo-pbtjxcxkgw3y]");
    expect(old).toContain("Legacy message");
    expect(old).not.toContain("<cito_signal");
    expect(old).not.toContain("cito_inbox");
    expect(renderer({ content: legacy }, { expanded: true }, theme).render(80).join("\n")).toContain("↳ Ack:");
  });
  it("wraps CJK, emoji and combining characters within narrow terminal widths and reapplies themes", () => {
    const renderer = register.mock.calls[0]![1];
    let style = "";
    const theme = { fg: (_color: string, text: string) => text, bg: (_color: string, text: string) => style + text,
      bold: (text: string) => text };
    const component = renderer({ content: "🔔 Cito\n你好🙂é" }, { expanded: false }, theme);
    expect(component.render(7)).toEqual(["🔔 Cito", "│ 你好 ", "│ 🙂é  "]);
    expect(component.render(1)).toEqual([" ", "C", "i", "t", "o", "é"]);
    style = "new-theme:"; component.invalidate();
    expect(component.render(7).every(line => line.startsWith("new-theme:"))).toBe(true);
    expect(component.render(0)).toEqual([]);
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
