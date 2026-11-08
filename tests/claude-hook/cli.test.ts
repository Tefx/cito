import { afterEach, expect, test } from "vitest";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { NativeHarness, nativeFixture, object, type JsonObject } from "../support/native-harness.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
const SESSION = "11111111-2222-3333-4444-555555555555";

async function runHook(f: NativeHarness, payload: object | string, env: NodeJS.ProcessEnv = {}) {
  const result = await f.command([], { executable: "plugins/claude/bin/cito-claude-hook", env,
    input: typeof payload === "string" ? payload : JSON.stringify(payload) });
  let json: JsonObject = {};
  try { json = object(JSON.parse(result.stdout.trim())); } catch { /* Malformed output is asserted below. */ }
  return { ...result, json };
}
function queuePath(f: NativeHarness, session = SESSION) { return join(f.stateDir, "claude", "wakes", session + ".jsonl"); }
function badgePath(f: NativeHarness, session = SESSION) { return join(f.stateDir, "claude", "badges", session); }
async function queuedRows(f: NativeHarness, session = SESSION): Promise<JsonObject[]> {
  let text: string;
  try { text = await readFile(queuePath(f, session), "utf8"); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
  return text.trim().split("\n").filter(Boolean).map(line => object(JSON.parse(line)));
}
async function append(f: NativeHarness, content: string, batch: string, session = SESSION, wake = true) {
  const result = await f.plugin("claude", { op: "wake", session_id: session, cwd: f.root,
    batch_id: batch, wake, text: content, endpoint_pid: process.pid, signals: [] });
  expect(result.ok).toBe(true);
  expect(result.outcome).toBe("delivered_unconfirmed");
}
async function queued(cards: string[]) {
  const f = await nativeFixture(); cleanups.push(() => f.close());
  const dir = join(f.env.CITO_CLAUDE_HOME!, "projects", f.root.replace(/[^a-zA-Z0-9]/g, "-"));
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, SESSION + ".jsonl"), "{}\n");
  for (const [index, content] of cards.entries()) await append(f, content, `batch-${index}`);
  return f;
}

test("a native Stop hook blocks the finished turn with every queued card and empties the queue", async () => {
  const f = await queued(["🔔 Cito · bob@laptop\n\nreview the PR", "🔔 Cito · eve@ci\n\nbuild is green"]);
  const first = await runHook(f, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root, stop_hook_active: false });
  expect(first.code).toBe(0);
  expect(first.json.decision).toBe("block");
  expect(first.json.reason).toBe("🔔 Cito · bob@laptop\n\nreview the PR\n\n🔔 Cito · eve@ci\n\nbuild is green");
  expect(await queuedRows(f)).toEqual([]);
  const second = await runHook(f, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root });
  expect(second.json).toEqual({});
}, 20_000);

test("native turn-start events inject context, honor CLAUDE_SESSION_ID, strip badge unread count, and resolve the workspace session", async () => {
  const f = await queued(["🔔 Cito · bob@laptop\n\nping"]);
  expect((await f.plugin("claude", { op: "update_badge", session_id: SESSION, badge: "🔔 cito (2)" })).ok).toBe(true);
  const start = await runHook(f, { hook_event_name: "SessionStart", cwd: f.root });
  expect(start.json).toEqual({ hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: "🔔 Cito · bob@laptop\n\nping" } });
  expect(await queuedRows(f)).toEqual([]);
  expect(await readFile(badgePath(f), "utf8")).toBe("🔔 cito\n");
  await append(f, "🔔 Cito · env\n\nfrom env", "env-batch", "env-override");
  const fromEnv = await runHook(f, { hook_event_name: "SessionStart", cwd: f.root }, { CLAUDE_SESSION_ID: "env-override" });
  expect(fromEnv.json).toEqual({ hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: "🔔 Cito · env\n\nfrom env" } });
  const alias = join(f.stateDir, "claude", "aliases", SESSION);
  await mkdir(dirname(alias), { recursive: true }); await writeFile(alias, "claude-7777\n");
  expect((await f.plugin("claude", { op: "update_badge", session_id: "claude-7777", badge: "🔔 cito (1)" })).ok).toBe(true);
  await append(f, "🔔 Cito · eve@ci\n\nmerged", "later", "claude-7777");
  const prompt = await runHook(f, { hook_event_name: "UserPromptSubmit", session_id: SESSION, cwd: f.root });
  expect(prompt.json).toEqual({ hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: "🔔 Cito · eve@ci\n\nmerged" } });
  expect(await readFile(badgePath(f, "claude-7777"), "utf8")).toBe("🔔 cito\n");
}, 20_000);

test("native events that cannot carry text and malformed input leave the queue untouched", async () => {
  const f = await queued(["🔔 Cito · bob@laptop\n\nhold"]);
  for (const payload of [{ hook_event_name: "PreToolUse", session_id: SESSION, cwd: f.root },
    { hook_event_name: "SubagentStop", session_id: SESSION, cwd: f.root }, { session_id: SESSION, cwd: f.root }]) {
    expect((await runHook(f, payload)).json).toEqual({});
  }
  const broken = await runHook(f, "not json at all");
  expect(broken.stdout.trim()).toBe("{}");
  expect(broken.stderr).toContain("cito-claude-hook:");
  const unsafe = await runHook(f, { hook_event_name: "Stop", session_id: "../escape", cwd: f.root });
  expect(unsafe.stdout.trim()).toBe("{}");
  expect(unsafe.stderr).toContain("Invalid Claude session identifier");
  expect((await queuedRows(f)).map(w => w.batch_id)).toEqual(["batch-0"]);
}, 30_000);

test("a card delivered by the native plugin reaches the model through the native Stop hook", async () => {
  const f = await queued([]);
  await append(f, "🔔 Cito · bob@laptop\n\nship it", "batch-e2e");
  const hook = await runHook(f, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root });
  expect(hook.json).toEqual({ decision: "block", reason: "🔔 Cito · bob@laptop\n\nship it" });
}, 20_000);

test("F44: passive-only native Stop context never blocks the turn", async () => {
  const f = await queued([]);
  await append(f, "Passive context", "passive", SESSION, false);
  const hook = await runHook(f, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root });
  expect(hook.code).toBe(0);
  expect(hook.json).toEqual({ hookSpecificOutput: { hookEventName: "Stop", additionalContext: "Passive context" } });
  expect(await queuedRows(f)).toEqual([]);
});

test("F62: a torn queue line cannot corrupt the next native append", async () => {
  const f = await queued([]);
  await mkdir(dirname(queuePath(f)), { recursive: true });
  await writeFile(queuePath(f), '{"batch_id":"torn"');
  await append(f, "Complete card", "complete");
  const hook = await runHook(f, { hook_event_name: "Stop", session_id: SESSION, cwd: f.root });
  expect(hook.json).toEqual({ decision: "block", reason: "Complete card" });
});
