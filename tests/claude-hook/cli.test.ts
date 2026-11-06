import { afterEach, expect, test } from "vitest";
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { isolatedState } from "../adapters/support.js";
import { loadConfig, type CitoConfig } from "../../src/shared/index.js";
import {
  ADAPTER_REGISTRY, appendClaudeWake, claudeBadgePath, claudeProjectDir, peekClaudeWakes,
  startClaudeChannelBridge, writeClaudeBadge, writeClaudeSessionAlias,
} from "../../src/adapters/index.js";
import type { DeliverContext } from "../../src/adapters/types.js";
import type { ListenerRow } from "../../src/mcp/state.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
const SESSION = "11111111-2222-3333-4444-555555555555";

function runHook(env: NodeJS.ProcessEnv, payload: object | string, args: string[] = []) {
  return new Promise<{ json: Record<string, unknown>; stdout: string; stderr: string; code: number | null }>((done, fail) => {
    const child = spawn(process.execPath, [resolve("bin/cito-claude-hook"), ...args], { env, stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    child.stdout.on("data", chunk => { stdout += chunk; });
    child.stderr.on("data", chunk => { stderr += chunk; });
    child.once("error", fail);
    child.once("close", code => {
      let json: Record<string, unknown> = {};
      try { json = JSON.parse(stdout.trim()) as Record<string, unknown>; } catch { /* Asserted through stdout. */ }
      done({ json, stdout, stderr, code });
    });
    child.stdin.end(typeof payload === "string" ? payload : JSON.stringify(payload));
  });
}
async function queued(cards: string[]) {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config = loadConfig(f.env);
  const dir = claudeProjectDir(f.root, config);
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, SESSION + ".jsonl"), "{}\n");
  for (const [index, content] of cards.entries()) {
    await appendClaudeWake(config, SESSION, { batch_id: `batch-${index}`, content, wake: true, created_at_ms: index });
  }
  return { f, config };
}

test("a Stop hook blocks the finished turn with every queued card and empties the queue", async () => {
  const { f, config } = await queued(["🔔 Cito · bob@laptop\n\nreview the PR", "🔔 Cito · eve@ci\n\nbuild is green"]);
  const first = await runHook(f.env, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root, stop_hook_active: false });
  expect(first.code).toBe(0);
  expect(first.json.decision).toBe("block");
  expect(first.json.reason).toBe("🔔 Cito · bob@laptop\n\nreview the PR\n\n🔔 Cito · eve@ci\n\nbuild is green");
  expect(await peekClaudeWakes(config, SESSION)).toEqual([]);
  // A second boundary with nothing pending must let the turn end.
  const second = await runHook(f.env, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root });
  expect(second.json).toEqual({});
}, 20_000);
test("turn-start events inject the cards as context, honor CLAUDE_SESSION_ID, strip badge unread count, and resolve the session from the workspace", async () => {
  const { f, config } = await queued(["🔔 Cito · bob@laptop\n\nping"]);
  await writeClaudeBadge(config, SESSION, "🔔 cito (2)");
  const start = await runHook(f.env, { hook_event_name: "SessionStart", cwd: f.root });
  expect(start.json).toEqual({ hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: "🔔 Cito · bob@laptop\n\nping" } });
  expect(await peekClaudeWakes(config, SESSION)).toEqual([]);
  expect(await readFile(claudeBadgePath(config, SESSION), "utf8")).toBe("🔔 cito\n");
  await appendClaudeWake(config, "env-override", { batch_id: "env-batch", content: "🔔 Cito · env\n\nfrom env", wake: true, created_at_ms: 5 });
  const fromEnv = await runHook({ ...f.env, CLAUDE_SESSION_ID: "env-override" }, { hook_event_name: "SessionStart", cwd: f.root });
  expect(fromEnv.json).toEqual({ hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: "🔔 Cito · env\n\nfrom env" } });
  // Aliased fallback queue and badge are also drained and stripped when the hook runs with the transcript UUID.
  await writeClaudeSessionAlias(config, SESSION, "claude-7777");
  await writeClaudeBadge(config, "claude-7777", "🔔 cito (1)");
  await appendClaudeWake(config, "claude-7777", { batch_id: "later", content: "🔔 Cito · eve@ci\n\nmerged", wake: true, created_at_ms: 9 });
  const prompt = await runHook(f.env, { hook_event_name: "UserPromptSubmit", session_id: SESSION, cwd: f.root });
  expect(prompt.json).toEqual({ hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: "🔔 Cito · eve@ci\n\nmerged" } });
  expect(await readFile(claudeBadgePath(config, "claude-7777"), "utf8")).toBe("🔔 cito\n");
}, 20_000);
test("an event that cannot carry text, and malformed input, leave the queue untouched", async () => {
  const { f, config } = await queued(["🔔 Cito · bob@laptop\n\nhold"]);
  for (const payload of [{ hook_event_name: "PreToolUse", session_id: SESSION, cwd: f.root },
    { hook_event_name: "SubagentStop", session_id: SESSION, cwd: f.root }, { session_id: SESSION, cwd: f.root }]) {
    expect((await runHook(f.env, payload)).json).toEqual({});
  }
  const broken = await runHook(f.env, "not json at all");
  expect(broken.stdout.trim()).toBe("{}");
  expect(broken.stderr).toContain("cito-claude-hook:");
  const unsafe = await runHook(f.env, { hook_event_name: "Stop", session_id: "../escape", cwd: f.root });
  expect(unsafe.stdout.trim()).toBe("{}");
  expect(unsafe.stderr).toContain("Invalid Claude session identifier");
  expect((await peekClaudeWakes(config, SESSION)).map(w => w.batch_id)).toEqual(["batch-0"]);
}, 30_000);
test("a card delivered by the daemon reaches the model through the bridge and the Stop hook", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const config: CitoConfig = loadConfig(f.env);
  const dir = claudeProjectDir(f.root, config);
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, SESSION + ".jsonl"), "{}\n");
  const channel = await startClaudeChannelBridge(config, { send: async () => {} },
    { endpointPid: process.pid, cwd: f.root, env: {} });
  cleanups.push(() => channel.close());
  const ctx: DeliverContext = { batchId: "batch-e2e", card: "🔔 Cito · bob@laptop\n\nship it", queueWritten: false,
    markQueueWritten: () => {}, runEffect: async submit => { try { await submit(); return true; } catch { return false; } },
    config, signal: new AbortController().signal };
  const row = { harness: "claude", session_id: SESSION, endpoint_pid: process.pid } as ListenerRow;
  // The queue write is durable, but only the hook proves the model ever saw the card.
  expect(await ADAPTER_REGISTRY.get("claude")!.deliver(row, ctx)).toBe("delivered_unconfirmed");
  const hook = await runHook(f.env, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root });
  expect(hook.json).toEqual({ decision: "block", reason: "🔔 Cito · bob@laptop\n\nship it" });
}, 20_000);
