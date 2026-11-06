import { afterEach, expect, test } from "vitest";
import { appendFile, mkdir, readFile, rename, stat, utimes, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { isolatedState, unixServer } from "./support.js";
import { loadConfig, type CitoConfig } from "../../src/shared/index.js";
import {
  ADAPTER_REGISTRY, ClaudeCodeAdapter, appendClaudeWake, claudeBadgePath, claudeProjectDir, claudeSessionClaim,
  claudeWakeQueuePath, drainClaudeWakes, getAdapter, peekClaudeWakes, resolveCaller, resolveClaudeSessionId,
  resolveClaudeSessionTitle, validateClaudeSessionId, writeClaudeBadge,
} from "../../src/adapters/index.js";
import type { DeliverContext } from "../../src/adapters/types.js";
import type { ListenerRow } from "../../src/mcp/state.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
const SESSION = "11111111-2222-3333-4444-555555555555";
const OLDER = "99999999-8888-7777-6666-555555555555";

async function transcript(claudeHome: string, cwd: string, id: string, mtimeMs: number): Promise<void> {
  const dir = claudeProjectDir(cwd, { claudeHome });
  await mkdir(dir, { recursive: true });
  const path = join(dir, id + ".jsonl");
  await writeFile(path, "{}\n");
  await utimes(path, mtimeMs / 1000, mtimeMs / 1000);
}
function deliverContext(config: CitoConfig, card = "🔔 Cito · one"): DeliverContext {
  return { batchId: "batch-1", card, queueWritten: false, markQueueWritten: () => {},
    runEffect: async submit => { try { await submit(); return true; } catch { return false; } },
    config, signal: new AbortController().signal };
}

test("the newest transcript names the running session and unrelated files never do", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  expect(await resolveClaudeSessionId(f.root, config)).toBeUndefined();
  await transcript(f.claudeHome, f.root, OLDER, 1_700_000_000_000);
  await transcript(f.claudeHome, f.root, SESSION, 1_800_000_000_000);
  await writeFile(join(claudeProjectDir(f.root, config), "summary.jsonl"), "{}\n");
  await writeFile(join(claudeProjectDir(f.root, config), SESSION + ".txt"), "{}\n");
  expect(await resolveClaudeSessionId(f.root, config)).toBe(SESSION);
  await transcript(f.claudeHome, f.root, OLDER, 1_900_000_000_000);
  expect(await resolveClaudeSessionId(f.root, config)).toBe(OLDER);
});
test("the newest custom-title names the session, and an untitled or unreadable transcript has none", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  const dir = claudeProjectDir(f.root, config);
  await mkdir(dir, { recursive: true });
  const write = async (id: string, body: string) => { await writeFile(join(dir, id + ".jsonl"), body); };
  const record = (title: string) => JSON.stringify({ type: "custom-title", customTitle: title });
  // The last record wins, unrelated and malformed lines are skipped, and control characters are flattened.
  await write(SESSION, [
    record("First name"), '{"type":"user"}', "{not json",
    JSON.stringify({ type: "custom-title" }), JSON.stringify({ type: "custom-title", customTitle: "   " }),
    record("Review\u0007 the\tadapter"), '{"type":"assistant"}', "",
  ].join("\n"));
  expect(await resolveClaudeSessionTitle(f.root, SESSION, config)).toBe("Review  the adapter");
  await write(OLDER, '{"type":"user"}\n');
  expect(await resolveClaudeSessionTitle(f.root, OLDER, config)).toBeUndefined();
  expect(await resolveClaudeSessionTitle(f.root, "66666666-6666-6666-6666-666666666666", config)).toBeUndefined();
  // A session id that is not a transcript name must never be turned into a path.
  expect(await resolveClaudeSessionTitle(f.root, "claude-123", config)).toBeUndefined();
  await write(SESSION, record("x".repeat(500)));
  expect((await resolveClaudeSessionTitle(f.root, SESSION, config))!).toHaveLength(200);
});
test("only the transcript tail is read, so a title older than the window yields the workspace fallback", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  const dir = claudeProjectDir(f.root, config);
  await mkdir(dir, { recursive: true });
  const path = join(dir, SESSION + ".jsonl");
  const filler = JSON.stringify({ type: "assistant", text: "y".repeat(4000) }) + "\n";
  await writeFile(path, JSON.stringify({ type: "custom-title", customTitle: "Ancient" }) + "\n" + filler.repeat(50));
  expect((await stat(path)).size).toBeGreaterThan(128 * 1024);
  expect(await resolveClaudeSessionTitle(f.root, SESSION, config)).toBeUndefined();
  // Appending a fresh title both lands inside the window and invalidates the cached miss.
  await appendFile(path, JSON.stringify({ type: "custom-title", customTitle: "Current" }) + "\n");
  expect(await resolveClaudeSessionTitle(f.root, SESSION, config)).toBe("Current");
});
test("explicit session claims accept one identifier and reject conflicts or unsafe names", () => {
  expect(claudeSessionClaim({})).toBeUndefined();
  expect(claudeSessionClaim({ claudeSessionId: " " + SESSION + " " })).toBe(SESSION);
  expect(claudeSessionClaim({ claude_session_id: SESSION })).toBe(SESSION);
  expect(claudeSessionClaim({ expected_harness: "claude", expected_session_id: SESSION })).toBe(SESSION);
  expect(claudeSessionClaim({ expected_harness: "codex", expected_session_id: SESSION })).toBeUndefined();
  expect(claudeSessionClaim({ claudeSessionId: SESSION }, { "x-claude-session-id": SESSION })).toBe(SESSION);
  expect(() => claudeSessionClaim({ claudeSessionId: SESSION }, { "x-claude-session-id": OLDER }))
    .toThrow(expect.objectContaining({ code: "INVALID_INPUT" }));
  for (const bad of ["../escape", "a/b", "", ".hidden", "x".repeat(129)]) {
    expect(() => validateClaudeSessionId(bad)).toThrow(expect.objectContaining({ code: "INVALID_INPUT" }));
  }
});
test("a claimed session binds to its workspace and cannot be mixed with a PID claim", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  expect(await resolveCaller({ meta: { claudeSessionId: SESSION, cwd: f.root }, httpTransport: true }, config))
    .toMatchObject({ harness: "claude", session_id: SESSION, cwd: f.root });
  await expect(resolveCaller({ meta: { claudeSessionId: SESSION, cwd: f.root, endpoint_pid: process.pid }, httpTransport: true }, config))
    .rejects.toMatchObject({ code: "INVALID_INPUT" });
  // A claim for a session that moved on is rejected before any effect runs.
  await expect(resolveCaller({ meta: { claudeSessionId: SESSION, cwd: f.root, expected_harness: "claude", expected_session_id: OLDER }, httpTransport: true }, config))
    .rejects.toMatchObject({ code: "INVALID_INPUT" });
});
test("the wake queue drains once, recovers a staging file and survives an append that races a drain", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  expect(await drainClaudeWakes(config, SESSION)).toEqual([]);
  await appendClaudeWake(config, SESSION, { batch_id: "a", content: "first", wake: true, created_at_ms: 1 });
  await appendClaudeWake(config, SESSION, { batch_id: "b", content: "second", wake: true, created_at_ms: 2 });
  expect((await peekClaudeWakes(config, SESSION)).map(w => w.batch_id)).toEqual(["a", "b"]);
  // A drain that died after staging must not swallow the cards it had taken.
  const path = claudeWakeQueuePath(config, SESSION);
  await rename(path, path + ".draining-abandoned");
  await appendClaudeWake(config, SESSION, { batch_id: "c", content: "third", wake: false, created_at_ms: 3 });
  const drained = await drainClaudeWakes(config, SESSION);
  expect(drained.map(w => w.batch_id)).toEqual(["a", "b", "c"]);
  expect(drained.map(w => w.wake)).toEqual([true, true, false]);
  expect(await drainClaudeWakes(config, SESSION)).toEqual([]);
  expect(await peekClaudeWakes(config, SESSION)).toEqual([]);
  // A torn line never blocks the cards around it.
  await appendClaudeWake(config, SESSION, { batch_id: "d", content: "fourth", wake: true, created_at_ms: 4 });
  await writeFile(path, await readFile(path, "utf8") + '{"batch_id":"e","con', { flag: "w" });
  expect((await drainClaudeWakes(config, SESSION)).map(w => w.batch_id)).toEqual(["d"]);
});
test("badges are written beside the queue and removed with the session", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  await writeClaudeBadge(config, SESSION, "🔔 abcdefghijkl (1)");
  expect(await readFile(claudeBadgePath(config, SESSION), "utf8")).toBe("🔔 abcdefghijkl (1)\n");
  await writeClaudeBadge(config, SESSION, undefined);
  await expect(readFile(claudeBadgePath(config, SESSION), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
});
test("claude is registered by default and queues a card without claiming it was read", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  const adapter = ADAPTER_REGISTRY.get("claude");
  expect(adapter).toBeInstanceOf(ClaudeCodeAdapter);
  expect(getAdapter("claude")).toBe(adapter);
  const row = { harness: "claude", session_id: SESSION, endpoint_pid: process.pid } as ListenerRow;
  let reply: object = { ok: true, delivered: true };
  const server = await unixServer(join(f.stateDir, `sockets/claude-pid-${process.pid}.sock`),
    socket => socket.once("data", () => socket.end(JSON.stringify(reply) + "\n")));
  cleanups.push(() => server.stop());
  expect(await adapter!.deliver(row, deliverContext(config))).toBe("delivered_unconfirmed");
  reply = { ok: false, error: "SESSION_MISMATCH" };
  expect(await adapter!.deliver(row, deliverContext(config))).toBe("session_switched");
  reply = { ok: true, delivered: true };
  expect(await adapter!.deliver({ ...row, endpoint_pid: 2147483647 } as ListenerRow, deliverContext(config))).toBe("driver_unavailable");
});
