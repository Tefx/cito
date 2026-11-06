import { afterEach, expect, test } from "vitest";
import { mkdir, readFile, stat, utimes, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import { isolatedState, unixServer } from "./support.js";
import { loadConfig } from "../../src/shared/index.js";
import {
  bridgeSocketPath, claudeAliasPath, claudeBadgePath, claudeProjectDir, drainClaudeWakes, identifyBridge,
  peekClaudeWakes, requestBridge, startClaudeChannelBridge, startOptionalStdioChannel,
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
test("identify reports the title the user gave the session, not the workspace folder", async () => {
  const { f, config } = await bridge({ transcript: SESSION });
  const path = join(claudeProjectDir(f.root, config), SESSION + ".jsonl");
  await writeFile(path, JSON.stringify({ type: "custom-title", customTitle: "Claude adapter review" }) + "\n");
  expect(await identifyBridge("claude", process.pid, config)).toMatchObject({
    session_id: SESSION, session_name: "Claude adapter review", cwd: f.root,
  });
  // A transcript that carries no title leaves discovery with the workspace name it had before.
  await writeFile(path, '{"type":"user"}\n');
  expect((await identifyBridge("claude", process.pid, config)).session_name).toBe(basename(f.root));
});
test("an explicit session id wins over the transcript scan, first-turn transcript creation preserves claude-<pid> via alias, and /clear switches to the second transcript", async () => {
  const explicit = await bridge({ transcript: SESSION, env: { CLAUDE_SESSION_ID: "env-session" } });
  expect(await explicit.channel.sessionId()).toBe("env-session");
  const pid = process.pid + 1;
  const fallback = `claude-${pid}`;
  const bare = await bridge({ pid });
  expect(await bare.channel.sessionId()).toBe(fallback);
  const dir = claudeProjectDir(bare.f.root, bare.config);
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, SESSION + ".jsonl"), "{}\n");
  // First transcript creation during turn 1 keeps the fallback session_id stable and writes an alias.
  expect(await bare.channel.sessionId()).toBe(fallback);
  expect((await readFile(claudeAliasPath(bare.config, SESSION), "utf8")).trim()).toBe(fallback);
  expect((await stat(claudeAliasPath(bare.config, SESSION))).mode & 0o777).toBe(0o600);
  await expect(requestBridge(bare.channel.socketPath, {
    op: "deliver", session_id: fallback, batch_id: "first-turn-batch", wake: true, content: "🔔 Cito · first turn",
  }, 2000)).resolves.toMatchObject({ delivered: true });
  expect((await drainClaudeWakes(bare.config, SESSION)).map(w => w.batch_id)).toEqual(["first-turn-batch"]);
  await writeFile(join(dir, SESSION + ".jsonl"), JSON.stringify({ type: "custom-title", customTitle: "First Turn Title" }) + "\n");
  expect((await identifyBridge("claude", pid, bare.config)).session_name).toBe("First Turn Title");
  // A second distinct transcript (/clear) switches the session_id, resets title to the new transcript, and rejects the old fallback binding.
  const second = "22222222-3333-4444-5555-666666666666";
  await new Promise(resolve => setTimeout(resolve, 15));
  const futureMtimeMs = Date.now() + 120_000;
  await writeFile(join(dir, second + ".jsonl"), JSON.stringify({ type: "custom-title", customTitle: "Second Turn Title" }) + "\n");
  await utimes(join(dir, second + ".jsonl"), futureMtimeMs / 1000, futureMtimeMs / 1000);
  expect(await bare.channel.sessionId()).toBe(second);
  const afterClearIdentity = await identifyBridge("claude", pid, bare.config);
  expect(afterClearIdentity).toMatchObject({
    session_id: second,
    session_name: "Second Turn Title",
  });
  expect(Number(afterClearIdentity.updated_at_ms)).toBeGreaterThanOrEqual(futureMtimeMs - 1000);
  await expect(requestBridge(bare.channel.socketPath, {
    op: "deliver", session_id: fallback, batch_id: "after-clear", wake: true, content: "stale",
  }, 2000)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
});
test("status writes badges across session switches, non-blocking notifications never stall delivery, and closing clears all badged sessions", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig({ ...f.env, CITO_CLAUDE_CHANNEL: "1" });
  const dir = claudeProjectDir(f.root, config);
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, SESSION + ".jsonl"), "{}\n");
  // Transport send that never resolves must not block socket bridge delivery.
  const channel = (await startOptionalStdioChannel(config, { send: () => new Promise<void>(() => {}) },
    { endpointPid: process.pid, cwd: f.root, env: {} }))!;
  cleanups.push(() => channel.close());
  expect(await startOptionalStdioChannel({ ...config, enableClaudeChannel: false }, { send: async () => {} })).toBeUndefined();
  const path = bridgeSocketPath(config.stateDir, "claude", process.pid);
  await expect(requestBridge(path, { op: "deliver", session_id: SESSION, batch_id: "nonblocking", wake: true, content: "card" }, 500))
    .resolves.toMatchObject({ delivered: true });
  await requestBridge(path, { op: "status", session_id: SESSION, badge: "🔔 abcdefghijkl (2)" }, 2000);
  expect(await readFile(claudeBadgePath(config, SESSION), "utf8")).toBe("🔔 abcdefghijkl (2)\n");
  const second = "33333333-4444-5555-6666-777777777777";
  await new Promise(resolve => setTimeout(resolve, 15));
  await writeFile(join(dir, second + ".jsonl"), "{}\n");
  await requestBridge(path, { op: "status", session_id: second, badge: "🔔 abcdefghijkl (1)" }, 2000);
  expect(await readFile(claudeBadgePath(config, second), "utf8")).toBe("🔔 abcdefghijkl (1)\n");
  await expect(requestBridge(path, { op: "status", session_id: second, badge: 7 }, 2000)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
  await channel.close();
  await expect(readFile(claudeBadgePath(config, SESSION), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  await expect(readFile(claudeBadgePath(config, second), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
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
