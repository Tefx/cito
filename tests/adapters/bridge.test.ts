import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createConnection } from "node:net";
import { stat, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import citoBridge, { type BridgeAPI, type BridgeCommandOptions, type BridgeContext } from "../../plugins/pi/cito-bridge.js";
import { bridgeSocketPath, identifyBridge, requestBridge, SocketBridgeAdapter, type DeliverContext } from "../../src/adapters/index.js";
import { startHttpServer } from "../../src/mcp/server.js";
import { loadConfig } from "../../src/shared/index.js";
import type { ListenerRow } from "../../src/mcp/state.js";
import { fixture } from "../mcp/support.js";
import { isolatedState } from "./support.js";

describe("self-contained Pi bridge over real Unix sockets", () => {
  let state: Awaited<ReturnType<typeof isolatedState>>;
  let ctx: BridgeContext, currentId: string | undefined, name: string | undefined, api: BridgeAPI;
  let handlers: Map<string, (event: unknown, ctx: BridgeContext) => void | Promise<void>>;
  let command: BridgeCommandOptions | undefined;
  const send = vi.fn<BridgeAPI["sendMessage"]>(), status = vi.fn<BridgeContext["ui"]["setStatus"]>();
  const notify = vi.fn<NonNullable<BridgeContext["ui"]["notify"]>>();
  const register = vi.fn<NonNullable<BridgeAPI["registerMessageRenderer"]>>();
  const registerCommand = vi.fn<NonNullable<BridgeAPI["registerCommand"]>>((_name, opts) => { command = opts; });
  const emit = async (event: string) => { await handlers.get(event)!({}, ctx); };
  const path = () => bridgeSocketPath(state.stateDir, "pi", process.pid);
  const request = (value: object) => requestBridge(path(), value, 1500);
  const identify = () => identifyBridge("pi", process.pid, state);
  beforeEach(async () => {
    state = await isolatedState(); vi.stubEnv("CITO_STATE_DIR", state.stateDir); vi.stubEnv("CITO_CODEX_HOME", state.codexHome);
    currentId = "session-a"; name = "Human title"; command = undefined;
    ctx = { cwd: state.root, sessionManager: { getSessionId: () => currentId, getSessionName: () => name }, ui: { setStatus: status, notify } };
    handlers = new Map(); send.mockReset(); status.mockReset(); notify.mockReset(); register.mockReset(); registerCommand.mockReset();
    api = { on: (event, handler) => handlers.set(event, handler), sendMessage: send, registerMessageRenderer: register, registerCommand };
    citoBridge(api);
  });
  afterEach(async () => { await emit("session_shutdown"); vi.unstubAllEnvs(); await state.cleanup(); });
  it("defers socket creation until session_start; identifies and closes with private permissions", async () => {
    await expect(stat(path())).rejects.toMatchObject({ code: "ENOENT" }); await emit("session_start");
    expect((await stat(path())).mode & 0o777).toBe(0o600);
    expect((await stat(state.stateDir + "/sockets")).mode & 0o777).toBe(0o700);
    const id1 = await identify();
    expect(id1).toEqual({ ok: true, endpoint_pid: process.pid, pi_pid: process.pid, session_id: "session-a", session_name: "Human title", cwd: state.root, updated_at_ms: expect.any(Number) });
    // Read-only identify and status probes must not advance updated_at_ms when sessionId is unchanged
    await new Promise(r => setTimeout(r, 15));
    const id2 = await identify();
    expect(id2.updated_at_ms).toBe(id1.updated_at_ms);
    await request({ op: "status", session_id: "session-a", badge: "🔔 test" });
    await new Promise(r => setTimeout(r, 15));
    expect((await identify()).updated_at_ms).toBe(id1.updated_at_ms);
    // session_info_changed advances updated_at_ms
    name = "Renamed"; await emit("session_info_changed");
    const id3 = await identify();
    expect(id3.session_name).toBe("Renamed");
    expect(Number(id3.updated_at_ms)).toBeGreaterThan(Number(id1.updated_at_ms));
    currentId = undefined;
    await expect(identify()).rejects.toMatchObject({ bridgeError: "SESSION_NOT_OPEN" });
    await emit("session_shutdown"); await expect(stat(path())).rejects.toMatchObject({ code: "ENOENT" });
  });
  it("preserves per-session batch dedup through A to B to A", async () => {
    await emit("session_start");
    const delivery = { op: "deliver", session_id: "session-a", batch_id: "same-batch", wake: true, content: "A card" };
    expect(await request(delivery)).toMatchObject({ deduplicated: false });
    currentId = "session-b"; await emit("session_tree");
    expect(await request({ ...delivery, session_id: "session-b", content: "B card" })).toMatchObject({ deduplicated: false });
    currentId = "session-a"; await emit("session_tree");
    expect(await request(delivery)).toMatchObject({ deduplicated: true });
    expect(send.mock.calls.map(call => call[0].content)).toEqual(["A card", "B card"]);
  });
  it("checks session identity before either status update or duplicate delivery", async () => {
    await emit("session_start");
    const row = { endpoint_pid: process.pid, session_id: "session-a" } as ListenerRow, config = loadConfig(state.env);
    const adapter = new SocketBridgeAdapter("pi");
    await adapter.updateBadge(row, "🔔 listener", config); expect(status).toHaveBeenLastCalledWith("cito", "🔔 listener");
    await adapter.updateBadge(row, undefined, config); expect(status).toHaveBeenLastCalledWith("cito", undefined);
    const context: DeliverContext = { batchId: "batch", card: "hello", config, signal: new AbortController().signal, queueWritten: false,
      markQueueWritten() {}, runEffect: async submit => { await submit(); return true; } };
    expect(await adapter.deliver(row, context)).toBe("delivered");
    await expect(adapter.updateBadge({ ...row, session_id: "old" }, "no", config)).rejects.toMatchObject({ bridgeError: "SESSION_MISMATCH" });
    expect(await adapter.deliver({ ...row, session_id: "old" }, context)).toBe("session_switched");
    expect(send).toHaveBeenCalledTimes(1);
    expect(status.mock.calls).toEqual([["cito", undefined], ["cito", "🔔 listener"], ["cito", undefined]]);
  });
  it("deduplicates concurrent retries and evicts only the oldest of 200 IDs", async () => {
    await emit("session_start");
    const deliver = (batch_id: string) => request({ op: "deliver", session_id: "session-a", batch_id, wake: true, content: "card" });
    const replies = await Promise.all([deliver("batch-0"), deliver("batch-0")]);
    expect(replies.map(r => r.deduplicated).sort()).toEqual([false, true]);
    expect(send).toHaveBeenCalledWith({ customType: "cito-notification", content: "card", display: true }, { triggerTurn: true, deliverAs: "followUp" });
    for (let i = 1; i < 200; i++) await deliver("batch-" + i);
    expect((await deliver("batch-0")).deduplicated).toBe(true); await deliver("batch-200");
    expect((await deliver("batch-1")).deduplicated).toBe(true); expect((await deliver("batch-0")).deduplicated).toBe(false);
    expect(send).toHaveBeenCalledTimes(202);
  });
  it("does not record failed sendMessage calls; retries the exact batch", async () => {
    await emit("session_start");
    const delivery = { op: "deliver", session_id: "session-a", batch_id: "retry", wake: true, content: "card" };
    send.mockImplementationOnce(() => { throw new Error("Pi unavailable"); });
    await expect(request(delivery)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    expect(await request(delivery)).toMatchObject({ deduplicated: false }); expect(await request(delivery)).toMatchObject({ deduplicated: true });
    expect(send).toHaveBeenCalledTimes(2);
  });
  it("honors the bridge wake field, echoes IDs, and handles fragmented newline framing once", async () => {
    await emit("session_start");
    const reply = await new Promise<string>((resolve, reject) => {
      const socket = createConnection(path()); socket.on("error", reject); let output = "";
      socket.on("data", chunk => { output += chunk.toString(); }); socket.on("end", () => resolve(output));
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
    await emit("session_start"); await request({ op: "status", session_id: "session-a", badge: "🔔 old" });
    currentId = "session-b"; await emit("session_tree"); expect(status).toHaveBeenLastCalledWith("cito", undefined);
    const calls = status.mock.calls.length; await emit("session_info_changed"); expect(status).toHaveBeenCalledTimes(calls);
    await request({ op: "status", session_id: "session-b", badge: "🔔 new" });
    currentId = "session-a"; await identify(); expect(status).toHaveBeenLastCalledWith("cito", undefined);
  });
  it("renders notifications with themed bars and preserves content from text blocks", () => {
    expect(register).toHaveBeenCalledWith("cito-notification", expect.any(Function));
    const renderer = register.mock.calls[0]![1];
    const theme = { fg: vi.fn((_color: string, text: string) => text), bg: vi.fn((_color: string, text: string) => text), bold: vi.fn((text: string) => text) };
    const content = '🔔 Cito · alice@mac [↩ repo-pbtjxcxkgw3y]\n\nHello';
    expect(renderer({ content }, { expanded: false, outputPad: 1 }, theme).render(80).map(line => line.trimEnd())).toEqual([
      ' 🔔 Cito · alice@mac [↩ repo-pbtjxcxkgw3y]', ' │', ' │ Hello',
    ]);
    expect(theme.bg).toHaveBeenCalledWith("customMessageBg", expect.any(String));
    expect(theme.fg).toHaveBeenCalledWith("accent", expect.stringContaining("🔔 Cito")); expect(theme.fg).toHaveBeenCalledWith("borderAccent", "│ ");
    expect(theme.bold).toHaveBeenCalled();
    expect(renderer({ content: [{ type: "text", text: content }] }, { expanded: true }, theme).render(80).join("\n")).toContain("Hello");
  });
  it("wraps CJK, emoji and combining characters within narrow terminal widths and reapplies themes", () => {
    const renderer = register.mock.calls[0]![1]; let style = "";
    const theme = { fg: (_color: string, text: string) => text, bg: (_color: string, text: string) => style + text, bold: (text: string) => text };
    const component = renderer({ content: "🔔 Cito\n你好🙂é" }, { expanded: false }, theme);
    expect(component.render(7)).toEqual(["🔔 Cito", "│ 你好 ", "│ 🙂é  "]); expect(component.render(1)).toEqual([" ", "C", "i", "t", "o", "é"]);
    style = "new-theme:"; component.invalidate(); expect(component.render(7).every(line => line.startsWith("new-theme:"))).toBe(true); expect(component.render(0)).toEqual([]);
  });
  it("registers /cito command, toggles via activeBadge, resets on session switch, and notifies on errors", async () => {
    expect(registerCommand).toHaveBeenCalledWith("cito", expect.objectContaining({ description: "Toggle or configure Cito session listening" }));
    expect(command!.getArgumentCompletions!("")).toEqual([{ value: "on", label: "on" }, { value: "team", label: "team" }, { value: "off", label: "off" }]);
    expect(command!.getArgumentCompletions!(" o")).toEqual([{ value: "on", label: "on" }, { value: "off", label: "off" }]);
    expect(command!.getArgumentCompletions!("te")).toEqual([{ value: "team", label: "team" }]);
    expect(command!.getArgumentCompletions!("none")).toEqual([]);
    expect(command!.getArgumentCompletions!("on -")).toEqual([{ value: "on --hub ", label: "--hub <hub>" }]);
    expect(command!.getArgumentCompletions!("team --h")).toEqual([{ value: "team --hub ", label: "--hub <hub>" }]);
    expect(command!.getArgumentCompletions!("off --hub")).toEqual([{ value: "off --hub ", label: "--hub <hub>" }]);
    expect(command!.getArgumentCompletions!("on --other")).toEqual([]);
    const f = await fixture({ pi: false, env: { CITO_STATE_DIR: state.stateDir, CITO_CODEX_HOME: state.codexHome } });
    const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
    vi.stubEnv("CITO_DAEMON_PORT", String(http.port));
    try {
      await emit("session_start");
      await command!.handler("", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringMatching(/^Cito listening as .+ \(self\)$/), "info");
      await command!.handler("", ctx);
      expect(notify).toHaveBeenLastCalledWith("Cito stopped listening", "info");
      name = "   ";
      await command!.handler("on", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringMatching(/^Cito listening as .+ \(self\)$/), "info");
      expect((await f.service.call("cito_find", {})).content[0]!.text).toContain(basename(state.root));
      // Switching sessions clears activeBadge so bare /cito listens for the new session instead of unlistens.
      currentId = "session-b";
      await emit("session_tree");
      await command!.handler("", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringMatching(/^Cito listening as .+ \(self\)$/), "info");
      await command!.handler("team Shared work", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringMatching(/^Cito listening as .+ \(workspace\)$/), "info");
      await command!.handler("off trailing", ctx);
      expect(notify).toHaveBeenLastCalledWith("Usage: /cito [on [about...] | team [about...] | off]", "warning");
      await command!.handler("on --hub", ctx);
      expect(notify).toHaveBeenLastCalledWith("Usage: /cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]", "warning");
      await command!.handler("on --hub=BAD!", ctx);
      expect(notify).toHaveBeenLastCalledWith("Usage: /cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]", "warning");
      await command!.handler("off --hub default extra", ctx);
      expect(notify).toHaveBeenLastCalledWith("Usage: /cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]", "warning");
      await command!.handler("unknown", ctx);
      expect(notify).toHaveBeenLastCalledWith("Usage: /cito [on [about...] | team [about...] | off]", "warning");
      await command!.handler("off", ctx);
      expect(notify).toHaveBeenLastCalledWith("Cito stopped listening", "info");
      // Tool error (SESSION_NOT_OPEN) and network error both notify with "error".
      currentId = undefined;
      await command!.handler("on", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringContaining("No caller socket bridge found"), "error");
    } finally {
      await http.close();
      await f.close();
    }
  });
  it("supports /cito on|team|off with --hub <hub> and --hub=<hub>, preserving activeBadge when another hub remains active", async () => {
    const base = await fixture({ pi: false, env: { CITO_STATE_DIR: state.stateDir, CITO_CODEX_HOME: state.codexHome } });
    const configPath = join(state.stateDir, "multi-hub.json");
    await writeFile(configPath, JSON.stringify({
      default_hub: "default",
      hubs: {
        default: { natsUrl: base.broker.url, authMode: "none", user: "alice" },
        work: { natsUrl: base.broker.url, authMode: "none", user: "bob" },
      },
    }));
    const multiConfig = loadConfig({ ...base.env, CITO_CONFIG_PATH: configPath });
    const f = await fixture({
      broker: base.broker,
      pi: false,
      env: { CITO_STATE_DIR: state.stateDir, CITO_CODEX_HOME: state.codexHome, CITO_CONFIG_PATH: configPath },
    });
    const http = await startHttpServer({ ...multiConfig, daemonPort: 0 }, f.service);
    vi.stubEnv("CITO_DAEMON_PORT", String(http.port));
    try {
      await emit("session_start");
      await command!.handler("on --hub default Default task", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringMatching(/^Cito listening as .+ \(self\)$/), "info");
      await command!.handler("team --hub=work Work task", ctx);
      expect(notify).toHaveBeenLastCalledWith(expect.stringMatching(/^Cito listening as work:.+ \(workspace\)$/), "info");
      expect(status).toHaveBeenLastCalledWith("cito", expect.stringMatching(/^🔔 /));

      // Unlistening only --hub work keeps default active and preserves activeBadge so bare /cito unlistens default!
      await command!.handler("off --hub work", ctx);
      expect(notify).toHaveBeenLastCalledWith("Cito stopped listening on work", "info");
      expect(status.mock.calls.at(-1)?.[1]).toEqual(expect.stringMatching(/^🔔 /));

      // Bare /cito now sees activeBadge is still set and unlistens the remaining default hub
      await command!.handler("", ctx);
      expect(notify).toHaveBeenLastCalledWith("Cito stopped listening", "info");
      expect(status).toHaveBeenLastCalledWith("cito", undefined);

      // Session-switch fence: if sessionId switches while /cito on is awaiting callMcpTool, ignore late completion
      const origCall = f.service.call.bind(f.service);
      vi.spyOn(f.service, "call").mockImplementationOnce(async (...args) => {
        currentId = "switched-mid-rpc";
        await emit("session_tree");
        return origCall(...args);
      });
      notify.mockClear();
      await command!.handler("on Late listen", ctx);
      expect(notify).not.toHaveBeenCalled();
    } finally {
      await http.close();
      await f.close();
      await base.close();
    }
  });
});
