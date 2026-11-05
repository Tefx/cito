import { afterEach, expect, test } from "vitest";
import { stat } from "node:fs/promises";
import { basename } from "node:path";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import { startStdioChannelBridge } from "../../src/adapters/claude-channel.js";
import { bridgeSocketPath, requestBridge } from "../../src/adapters/pi.js";
import { loadConfig } from "../../src/shared/index.js";
import { isolatedState } from "./support.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const close of cleanups.splice(0).reverse()) await close(); });

test("closing a channel releases its endpoint even while notification transport drain is blocked", async () => {
  const state = await isolatedState(); cleanups.push(() => state.cleanup());
  let entered!: () => void, release!: () => void;
  const started = new Promise<void>(resolve => { entered = resolve; });
  const blocked = new Promise<void>(resolve => { release = resolve; });
  const bridge = await startStdioChannelBridge(loadConfig(state.env), { send: async () => { entered(); await blocked; } });
  cleanups.push(() => bridge.close());
  const path = bridgeSocketPath(state.stateDir, "claude", process.ppid);
  const identity = await requestBridge(path, { op: "identify" }, 1500);
  const pending = requestBridge(path, { op: "deliver", session_id: identity.session_id, batch_id: "blocked", content: "message" }, 3000);
  const failed = expect(pending).rejects.toThrow();
  try {
    await started;
    await bridge.close();
    await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
    await failed;
  } finally { release(); }
});
test("channel socket accepts serialized deliveries above one MiB within the four MiB bridge boundary", async () => {
  const state = await isolatedState(); cleanups.push(() => state.cleanup());
  const notifications: JSONRPCMessage[] = [];
  const bridge = await startStdioChannelBridge(loadConfig(state.env), { send: async message => { notifications.push(message); } });
  cleanups.push(() => bridge.close());
  const path = bridgeSocketPath(state.stateDir, "claude", process.ppid);
  const identity = await requestBridge(path, { op: "identify" }, 1500), content = "🙂".repeat(300000);
  expect(Buffer.byteLength(content)).toBeGreaterThan(1_048_576);
  expect(await requestBridge(path, { op: "deliver", session_id: identity.session_id, batch_id: "large", content }, 3000))
    .toMatchObject({ delivered: true, deduplicated: false });
  expect(notifications).toEqual([{ jsonrpc: "2.0", method: "notifications/claude/channel",
    params: { content, meta: { origin: "cito" } } }]);
});
test("channel socket owns its endpoint, checks session identity, serializes dedup and bounds its ring at 200", async () => {
  const state = await isolatedState(); cleanups.push(() => state.cleanup());
  const config = loadConfig(state.env), notifications: JSONRPCMessage[] = [];
  let fail = true;
  const transport = { async send(message: JSONRPCMessage) { if (fail) throw new Error("transport unavailable"); notifications.push(message); } };
  const bridge = await startStdioChannelBridge(config, transport); cleanups.push(() => bridge.close());
  const path = bridgeSocketPath(config.stateDir, "claude", process.ppid);
  expect((await stat(path)).mode & 0o777).toBe(0o600);
  const identity = await requestBridge(path, { op: "identify" }, 1500);
  expect(identity).toEqual({ ok: true, endpoint_pid: process.ppid, session_id: process.env.CLAUDE_SESSION_ID || `claude-${process.ppid}`,
    session_name: basename(process.cwd()), cwd: process.cwd() });
  await expect(startStdioChannelBridge(config, transport)).rejects.toMatchObject({ code: "EADDRINUSE" });
  const deliver = (batch: string, session: unknown = identity.session_id) => requestBridge(path,
    { op: "deliver", session_id: session, batch_id: batch, content: "channel message" }, 3000);
  await expect(deliver("one", "another-session")).rejects.toThrow("SESSION_MISMATCH");
  await expect(deliver("one")).rejects.toThrow("DELIVERY_FAILED");
  fail = false;
  const pair = await Promise.all([deliver("one"), deliver("one")]);
  expect(pair.map(reply => reply.deduplicated)).toEqual([false, true]);
  expect(notifications).toEqual([{ jsonrpc: "2.0", method: "notifications/claude/channel",
    params: { content: "channel message", meta: { origin: "cito" } } }]);
  for (let i = 0; i < 200; i++) await deliver(`batch-${i}`);
  expect((await deliver("batch-0")).deduplicated).toBe(true);
  expect((await deliver("one")).deduplicated).toBe(false);
  expect(notifications).toHaveLength(202);
  await bridge.close(); await bridge.close();
  await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
});
