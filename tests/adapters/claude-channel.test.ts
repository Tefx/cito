import { afterEach, expect, test } from "vitest";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import { isolatedState, unixServer } from "./support.js";
import { loadConfig } from "../../src/shared/index.js";
import {
  bridgeSocketPath, claudeBadgePath, claudeProjectDir, identifyBridge, peekClaudeWakes, requestBridge,
  startClaudeChannelBridge,
} from "../../src/adapters/index.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
const SESSION = "11111111-2222-3333-4444-555555555555";

async function bridge(options: { env?: NodeJS.ProcessEnv; transcript?: string; pid?: number } = {}) {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  if (options.transcript) {
    const dir = claudeProjectDir(f.root, config);
    await mkdir(dir, { recursive: true });
    await writeFile(join(dir, options.transcript + ".jsonl"), "{}\n");
  }
  const sent: JSONRPCMessage[] = [];
  const channel = await startClaudeChannelBridge(config, { send: async message => { sent.push(message); } },
    { endpointPid: options.pid ?? process.pid, cwd: f.root, env: options.env ?? {} });
  cleanups.push(() => channel.close());
  return { f, config, sent, channel };
}

test("the bridge identifies its hosting session, queues one card per batch and refuses a stale session", async () => {
  const { f, config, sent, channel } = await bridge({ transcript: SESSION });
  const path = bridgeSocketPath(config.stateDir, "claude", process.pid);
  expect(channel.socketPath).toBe(path);
  expect((await stat(path)).mode & 0o777).toBe(0o600);
  const identity = await identifyBridge("claude", process.pid, config);
  expect(identity).toMatchObject({ endpoint_pid: process.pid, session_id: SESSION, session_name: basename(f.root), cwd: f.root });
  const deliver = (batch: string, session = SESSION) =>
    requestBridge(path, { op: "deliver", session_id: session, batch_id: batch, wake: true, content: "🔔 Cito · one" }, 2000);
  expect(await deliver("batch-1")).toMatchObject({ delivered: true, deduplicated: false });
  expect(await deliver("batch-1")).toMatchObject({ delivered: true, deduplicated: true });
  expect(await deliver("batch-2")).toMatchObject({ delivered: true, deduplicated: false });
  expect((await peekClaudeWakes(config, SESSION)).map(w => w.batch_id)).toEqual(["batch-1", "batch-2"]);
  expect(sent.map(m => (m as { method: string }).method)).toEqual(["notifications/claude/channel", "notifications/claude/channel"]);
  // A card addressed to a session that has already been replaced must not reach the live one.
  await expect(deliver("batch-3", "22222222-2222-3333-4444-555555555555")).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
  await expect(requestBridge(path, { op: "sabotage", session_id: SESSION }, 2000)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  expect(await peekClaudeWakes(config, SESSION)).toHaveLength(2);
});
test("an explicit session id wins over the transcript scan and a workspace with no transcript still binds", async () => {
  const explicit = await bridge({ transcript: SESSION, env: { CLAUDE_SESSION_ID: "env-session" } });
  expect(await explicit.channel.sessionId()).toBe("env-session");
  const bare = await bridge({ pid: process.pid + 1 });
  expect(await bare.channel.sessionId()).toBe(`claude-${process.pid + 1}`);
});
test("status writes a badge the session can render and closing the bridge clears it", async () => {
  const { config, channel } = await bridge({ transcript: SESSION });
  const path = bridgeSocketPath(config.stateDir, "claude", process.pid);
  await requestBridge(path, { op: "status", session_id: SESSION, badge: "🔔 abcdefghijkl (2)" }, 2000);
  expect(await readFile(claudeBadgePath(config, SESSION), "utf8")).toBe("🔔 abcdefghijkl (2)\n");
  await expect(requestBridge(path, { op: "status", session_id: SESSION, badge: 7 }, 2000)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  await channel.close();
  await expect(readFile(claudeBadgePath(config, SESSION), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  // A closed bridge keeps its endpoint unbound instead of answering for a session it no longer hosts.
  await expect(requestBridge(path, { op: "identify" }, 500)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
});
test("only the initialize result gains the channel capability", async () => {
  const { channel } = await bridge({ transcript: SESSION });
  const initialize = { jsonrpc: "2.0", id: 1, result: { protocolVersion: "2026-07-28", capabilities: { tools: {} } } } as JSONRPCMessage;
  expect(channel.forward(initialize)).toMatchObject({ result: { capabilities: { tools: {}, experimental: { "claude/channel": {} } } } });
  const toolResult = { jsonrpc: "2.0", id: 2, result: { content: [] } } as JSONRPCMessage;
  expect(channel.forward(toolResult)).toBe(toolResult);
});
test("a second bridge refuses to steal a live endpoint but reclaims an abandoned socket", async () => {
  const { f, config } = await bridge({ transcript: SESSION });
  await expect(startClaudeChannelBridge(config, { send: async () => {} },
    { endpointPid: process.pid, cwd: f.root, env: {} })).rejects.toMatchObject({ code: "EADDRINUSE" });
  const stale = bridgeSocketPath(config.stateDir, "claude", process.pid + 1);
  const abandoned = await unixServer(stale, socket => socket.destroy());
  await abandoned.stop();
  const reclaimed = await startClaudeChannelBridge(config, { send: async () => {} },
    { endpointPid: process.pid + 1, cwd: f.root, env: {} });
  cleanups.push(() => reclaimed.close());
  expect((await identifyBridge("claude", process.pid + 1, config)).cwd).toBe(f.root);
});
