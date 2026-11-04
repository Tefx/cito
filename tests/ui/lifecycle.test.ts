import { afterAll, beforeAll, expect, test } from "vitest";
import { mkdtemp, rm, writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { ensureUiCompiled, UiBridge, type BannerPayload, type UiToDaemonAction, type UiListenerSummary } from "../../src/ui/index.js";
import { KeyedMutex } from "../../src/shared/index.js";
import { waitFor } from "../mcp/support.js";

let root: string, binary: string;
beforeAll(async () => { root = await mkdtemp("/tmp/cito-ui-test-"); binary = await ensureUiCompiled(root); }, 120000);
afterAll(async () => { await rm(root, { recursive: true, force: true }); });
const listener: UiListenerSummary = { hub: "personal", listener_id: "c_abcdefghijkl", public_id: "repo-abcdefghijkl", scoped_id: "personal:repo-abcdefghijkl",
  harness: "codex", session_id: "thread-1", title: "中文 conversation", cwd: "/tmp", repo: null, scope: "self", can_jump: true };
const hubs = [{ name: "personal", status: "connected" as const, user: "alice", auth_mode: "oidc" as const, error: null }];
const payload: BannerPayload = { target_harness: "codex", target_session_id: "thread-1", target_title: "中文 conversation", from_user: "alice", from_host: "host", from_title: "sender", message: "hello 中文", effective_wake: true };
function childOf(bridge: UiBridge) { return Reflect.get(bridge, "child") as ChildProcessWithoutNullStreams | undefined; }

test("KeyedMutex serializes same-key async/sync work, releases failures and permits unrelated keys", async () => {
  const mutex = new KeyedMutex(), order: string[] = [];
  let release!: () => void;
  const first = mutex.run("a", async () => { order.push("a1"); await new Promise<void>(r => { release = r; }); order.push("a2"); throw new Error("expected"); });
  const failure = expect(first).rejects.toThrow("expected");
  const second = mutex.run("a", () => { order.push("a3"); return 3; });
  await mutex.run("b", () => order.push("b"));
  expect(order).toEqual(["a1", "b"]); release(); await failure; expect(await second).toBe(3);
  expect(order).toEqual(["a1", "b", "a2", "a3"]);
  expect(Reflect.get(mutex, "tails").size).toBe(0);
});

test("compiled cito-ui decodes UTF-8 fragmented frames, drives actual menu actions, hides banners without exiting, and exits on EOF", async () => {
  const log = join(root, "opened"), opener = join(root, "open.cjs");
  await writeFile(opener, `#!${process.execPath}\nrequire('node:fs').appendFileSync(${JSON.stringify(log)}, process.argv[2]+'\\n');`, { mode: 0o700 });
  const child = spawn(binary, [], { stdio: "pipe", env: { ...process.env, CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "120", CITO_BANNER_URL_OPENER: opener } });
  let stderr = "", stdout = ""; child.stderr.on("data", c => { stderr += c; }); child.stdout.on("data", c => { stdout += c; });
  const exited = new Promise<number | null>(resolve => child.once("exit", resolve));
  const send = (data: unknown) => child.stdin.write(JSON.stringify(data) + "\n");
  try {
    const state = Buffer.from(JSON.stringify({ op: "state", hubs, listeners: [listener] }) + "\n");
    const split = state.indexOf(Buffer.from("中文")) + 1;
    child.stdin.write(state.subarray(0, split)); child.stdin.write(state.subarray(split));
    await waitFor(() => stderr.includes("state hubs=1 listeners=1"));
    child.stdin.write("bad json\n" + "x".repeat(1048577) + "\n");
    send({ op: "banner", payload }); await waitFor(() => stderr.includes("display "));
    await waitFor(() => stderr.includes("banner hidden")); expect(child.exitCode).toBeNull();
    send({ op: "banner", payload }); await waitFor(() => stderr.split("display ").length === 3);
    child.kill("SIGUSR1");
    await waitFor(async () => { try { return (await readFile(log, "utf8")).includes("codex://threads/thread-1"); } catch { return false; } });
    expect(child.exitCode).toBeNull();
    for (const index of [2, 3, 4, 5, 6, 7, 8, 9]) send({ op: "test_action", index });
    await waitFor(() => stdout.trim().split("\n").length === 8);
    expect(stdout.trim().split("\n").map(line => JSON.parse(line))).toEqual([
      { op: "set_scope", hub: "personal", listener_id: listener.listener_id, scope: "workspace" },
      { op: "unlisten", hub: "personal", listener_id: listener.listener_id },
      { op: "unlisten", hub: "personal", listener_id: listener.listener_id }, { op: "unlisten_all" },
      { op: "hub_action", hub: "personal", action: "login" }, { op: "hub_action", hub: "personal", action: "reconnect" },
      { op: "hub_action", hub: "personal", action: "disconnect" }, { op: "quit" },
    ]);
    expect(stderr).toContain("invalid frame"); expect(stderr).toContain("oversized frame");
    child.stdin.end(); expect(await exited).toBe(0);
  } finally { if (child.exitCode === null) child.kill("SIGKILL"); await exited; }
});

test("the default HUD timer hides after six seconds while the companion remains alive", async () => {
  const diagnostics: string[] = [];
  const bridge = new UiBridge(root, { binary, env: { CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: undefined }, diagnostic: line => diagnostics.push(line) });
  try {
    await bridge.start(); bridge.banner(payload);
    await waitFor(() => diagnostics.some(line => line.startsWith("cito-ui: display")));
    const began = performance.now(), child = childOf(bridge)!;
    await waitFor(() => diagnostics.includes("cito-ui: banner hidden"), 7500);
    expect(performance.now() - began).toBeGreaterThan(5500); expect(child.exitCode).toBeNull();
  } finally { await bridge.close(); }
}, 10000);

test("UiBridge coalesces snapshots, delivers banners and validated actions, respawns after crash with snapshot replay, and closes stdin", async () => {
  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, { binary, env: { CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "100" }, diagnostic: line => diagnostics.push(line), onAction: action => { actions.push(action); } });
  try {
    await bridge.start(); await waitFor(() => diagnostics.includes("cito-ui: state hubs=0 listeners=0"));
    diagnostics.length = 0;
    bridge.state(hubs, []); bridge.state(hubs, [listener]); bridge.state(hubs, []); bridge.state(hubs, [listener]);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1"));
    expect(diagnostics.filter(line => line.startsWith("cito-ui: state"))).toEqual(["cito-ui: state hubs=1 listeners=1"]);
    bridge.banner(payload); await waitFor(() => diagnostics.some(line => line.startsWith("cito-ui: display")));
    const old = childOf(bridge)!;
    old.stdin.write(JSON.stringify({ op: "test_action", index: 3 }) + "\n");
    await waitFor(() => actions.length === 1); expect(actions[0]).toEqual({ op: "unlisten", hub: "personal", listener_id: listener.listener_id });
    old.kill("SIGKILL");
    await waitFor(() => childOf(bridge)?.pid !== undefined && childOf(bridge)?.pid !== old.pid);
    await waitFor(() => diagnostics.filter(line => line === "cito-ui: state hubs=1 listeners=1").length === 2);
    const current = childOf(bridge)!; await bridge.close(); expect(current.exitCode).toBe(0);
  } finally { await bridge.close(); }
});

test("UiBridge discards malformed/oversized action lines and preserves multibyte frames across stdout chunks", async () => {
  const script = join(root, "frames.cjs");
  await writeFile(script, `#!${process.execPath}\nconst b=Buffer.from(JSON.stringify({op:'hub_action',hub:'中文',action:'disconnect'})+'\\n'); process.stdout.write('bad\\n'+JSON.stringify({op:'set_scope',hub:'a',listener_id:'invalid',scope:'self'})+'\\n'+'x'.repeat(1048577)+'\\n'); process.stdout.write(b.subarray(0,b.indexOf(Buffer.from('中文'))+1)); setTimeout(()=>process.stdout.write(b.subarray(b.indexOf(Buffer.from('中文'))+1)),20); process.stdin.resume(); process.stdin.on('end',()=>process.exit(0));`, { mode: 0o700 });
  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, { binary: script, diagnostic: line => diagnostics.push(line), onAction: action => { actions.push(action); } });
  try {
    await bridge.start(); await waitFor(() => actions.length === 1);
    expect(actions).toEqual([{ op: "hub_action", hub: "中文", action: "disconnect" }]);
    expect(diagnostics.filter(line => line.includes("invalid frame")).length).toBe(2);
  } finally { await bridge.close(); }
});
