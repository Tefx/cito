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
  harness: "codex", session_id: "thread-1", title: "café conversation", cwd: "/tmp", repo: null, scope: "self", can_jump: true };
const hubs = [{ name: "personal", status: "connected" as const, user: "alice", auth_mode: "oidc" as const, error: null }];
const payload: BannerPayload = { target_harness: "codex", target_session_id: "thread-1", target_title: "café conversation", from_user: "alice", from_host: "host", from_title: "sender", message: "hello café", effective_wake: true };
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
    const split = state.indexOf(Buffer.from("café")) + 1;
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

interface MenuNode { id: string; text?: string; tooltip?: string; symbol?: string; green?: boolean; width: number; height: number; x: number; y: number; radius: number; hidden?: boolean; children: MenuNode[] }
interface MenuSnapshot { icon_alpha: Record<string, number>; button_title: string; button_image_position: number; button_font_size: number; button_font_weight: number; button_font_features: { type: number; selector: number }[]; button_digit_widths: number[]; items: MenuNode[]; actions: number; count: number; icon_template: boolean; icon_width: number; icon_height: number; clipboard: string }
function flatten(nodes: MenuNode[]): MenuNode[] { return nodes.flatMap(node => [node, ...flatten(node.children)]); }

test("real AppKit card views expose template icon, 380pt control-center geometry, state-driven hub drawers, and SF Symbol controls", async () => {
  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, { binary, env: { CITO_UI_HEADLESS: "1" }, diagnostic: line => diagnostics.push(line), onAction: action => { actions.push(action); } });
  const send = (value: object) => childOf(bridge)!.stdin.write(JSON.stringify(value) + "\n");
  const inspect = async (): Promise<MenuSnapshot> => {
    const before = diagnostics.filter(line => line.startsWith("cito-ui: menu ")).length;
    send({ op: "test_inspect" });
    await waitFor(() => diagnostics.filter(line => line.startsWith("cito-ui: menu ")).length > before);
    return JSON.parse(diagnostics.filter(line => line.startsWith("cito-ui: menu ")).at(-1)!.slice("cito-ui: menu ".length));
  };
  try {
    await bridge.start();
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=0 listeners=0"));
    const idle = await inspect();
    expect(idle).toMatchObject({ count: 0, button_title: "", button_image_position: 1 });
    expect(idle.icon_alpha.arc).toBeGreaterThan(0.9);
    expect(idle.icon_alpha.core).toBeCloseTo(0.4, 1);
    expect(idle.icon_alpha.wave).toBeCloseTo(0.4, 1);
    expect(idle.icon_alpha.arc_gap).toBeLessThan(0.05);
    expect(idle.icon_alpha.warning_dot).toBeLessThan(0.05);
    expect(idle.icon_alpha.warning_ring).toBeGreaterThan(0.9);
    bridge.state(hubs, [listener]);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1"));
    const initial = await inspect(), nodes = flatten(initial.items);
    expect(initial).toMatchObject({ actions: 10, count: 1, icon_template: true, icon_width: 18, icon_height: 18 });
    expect(initial).toMatchObject({ button_title: " 1", button_image_position: 7, button_font_size: 12 });
    expect(initial.button_font_weight).toBeCloseTo(0.3, 2);
    expect(initial.button_font_features).toContainEqual({ type: 6, selector: 0 });
    expect(initial.button_digit_widths[0]).toBeCloseTo(initial.button_digit_widths[1]!, 4);
    expect(initial.icon_alpha.arc).toBeGreaterThan(0.9);
    expect(initial.icon_alpha.core).toBeGreaterThan(0.9);
    expect(initial.icon_alpha.wave).toBeGreaterThan(0.9);
    expect(initial.icon_alpha.arc_gap).toBeLessThan(0.05);
    expect(initial.icon_alpha.warning_dot).toBeLessThan(0.05);
    expect(initial.items.every(node => node.width === 380)).toBe(true);
    expect(nodes).toContainEqual(expect.objectContaining({ width: 360, height: 58, x: 10, radius: 10 }));
    expect(initial.items.find(node => node.id === "header")).toMatchObject({ width: 380, height: 40 });
    expect(initial.items.find(node => node.id === "footer")).toMatchObject({ width: 380, height: 36 });
    expect(nodes.find(node => node.id === "stop_all")).toMatchObject({ text: "Stop All", symbol: "stop.fill", x: 12, y: 6, width: 82, height: 24 });
    expect(nodes.find(node => node.id === "quit")).toMatchObject({ text: "Quit Cito ⌘Q", x: 262, y: 6, width: 106, height: 24 });
    expect(initial.items.find(node => node.id === "drawer:personal")).toMatchObject({ width: 380, height: 32, hidden: true });
    expect(nodes.map(node => node.text)).not.toContain("Codex");
    const copyNode = nodes.find(node => node.id === "copy:" + listener.scoped_id)!;
    expect(copyNode).toMatchObject({ text: "repo#abcd", symbol: "doc.on.doc", tooltip: listener.scoped_id, x: 54, y: 7, height: 19 });
    expect(copyNode.children.some(child => child.hidden === false && child.width === 12)).toBe(true);
    send({ op: "test_control", id: "copy:" + listener.scoped_id });
    const copied = await inspect();
    expect(copied.clipboard).toBe(listener.scoped_id);
    const copiedNode = flatten(copied.items).find(node => node.id === "copy:" + listener.scoped_id)!;
    expect(copiedNode).toMatchObject({ text: "✓ Copied", green: true });
    expect(copiedNode.children.some(child => child.hidden === true && child.width === 12)).toBe(true);
    await waitFor(async () => {
      const restored = flatten((await inspect()).items).find(node => node.id === "copy:" + listener.scoped_id);
      return restored?.text === "repo#abcd" && restored.children.some(child => child.hidden === false && child.width === 12);
    }, 2500);
    send({ op: "test_control", id: "hub:personal" });
    expect((await inspect()).items.find(node => node.id === "drawer:personal")?.hidden).toBe(false);
    send({ op: "test_control", id: "hub:personal" });
    expect((await inspect()).items.find(node => node.id === "drawer:personal")?.hidden).toBe(true);
    expect(nodes.find(node => node.id === "scope:" + listener.scoped_id)).toMatchObject({ text: "Self", symbol: "lock.fill", x: 236, y: 30, width: 62, height: 22 });
    send({ op: "test_control", id: "scope:" + listener.scoped_id });
    expect(flatten((await inspect()).items).find(node => node.id === "scope:" + listener.scoped_id)).toMatchObject({ text: "Team", symbol: "person.2.fill" });
    send({ op: "test_control", id: "scope:" + listener.scoped_id });
    await waitFor(() => actions.length === 2);
    expect(actions.map(action => action.op === "set_scope" && action.scope)).toEqual(["workspace", "self"]);
    bridge.state([{ ...hubs[0]!, status: "auth_required", error: "Sign in required" }], []);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=0"));
    const empty = await inspect(), emptyNodes = flatten(empty.items);
    expect(empty).toMatchObject({ count: 0, icon_template: true, button_title: "", button_image_position: 1 });
    expect(empty.icon_alpha.arc).toBeGreaterThan(0.9);
    expect(empty.icon_alpha.core).toBeCloseTo(0.4, 1);
    expect(empty.icon_alpha.wave).toBeCloseTo(0.4, 1);
    expect(empty.icon_alpha.warning_dot).toBeGreaterThan(0.9);
    expect(empty.icon_alpha.inside_dot).toBeGreaterThan(0.9);
    expect(empty.icon_alpha.warning_ring).toBeLessThan(0.05);
    expect(empty.icon_alpha.outside_cutout).toBeGreaterThan(0.9);
    expect(empty.items.find(node => node.id === "drawer:personal")?.hidden).toBe(false);
    expect(emptyNodes).toContainEqual(expect.objectContaining({ width: 360, height: 64 }));
    expect(emptyNodes.map(node => node.text)).toContain("No active listeners");
    expect(emptyNodes.find(node => node.id === "login:personal")).toMatchObject({ text: "Sign In…" });
    expect(emptyNodes.find(node => node.id === "disconnect:personal")).toMatchObject({ text: "Disconnect" });
    expect(emptyNodes.find(node => node.id === "reconnect:personal")).toBeUndefined();
    send({ op: "test_control", id: "hub:personal" });
    expect((await inspect()).items.find(node => node.id === "drawer:personal")?.hidden).toBe(true);
    // Single-default-hub mode eliminates literal "default" and renders compact 44x22pt icon pill when connected (none)
    const richListener: UiListenerSummary = {
      ...listener, hub: "default", public_id: "superlongrepositoryslugname-f8k2abcdefgh",
      scoped_id: "superlongrepositoryslugname-f8k2abcdefgh", harness: "pi", can_jump: false,
      about: "Primary workspace orchestrator session", branch: "main",
    };
    bridge.state([{ name: "default", status: "connected", user: "alice", auth_mode: "none", error: null }],
      [richListener, { ...listener, hub: "work", about: "CAFÉ CONVERSATION", branch: "HEAD" }]);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=2"));
    const duplicate = await inspect(), dupNodes = flatten(duplicate.items);
    expect(duplicate.count).toBe(2);
    const defaultPill = dupNodes.find(node => node.id === "hub:default")!;
    expect(defaultPill).toMatchObject({ text: "", tooltip: "Connected · alice", width: 44, height: 22 });
    expect(dupNodes.some(node => (node.text ?? "").includes("default"))).toBe(false);
    expect(dupNodes.find(node => node.id === "session:" + richListener.scoped_id)).toMatchObject({ width: 380, height: 78 });
    expect(dupNodes).toContainEqual(expect.objectContaining({ width: 360, height: 72, x: 10, radius: 10 }));
    expect(dupNodes.map(node => node.text)).toContain("Primary workspace orchestrator session");
    expect(dupNodes.find(node => node.id === "copy:" + richListener.scoped_id)?.text).toBe("superlon…lugname#f8k2");
    expect(dupNodes.map(node => node.text)).toContain("/tmp · main");
    // Second listener has case-insensitively identical about == title and branch == "HEAD" -> 2-row 64pt row / 58pt card, no branch suffix
    expect(dupNodes.find(node => node.id === "session:" + listener.scoped_id)).toMatchObject({ width: 380, height: 64 });
    expect(dupNodes.map(node => node.text)).toContain("/tmp");
    expect(dupNodes.map(node => node.text)).not.toContain("/tmp · HEAD");
    // State-driven hub drawer matrix across statuses and auth modes
    for (const [hubState, pillText, summaryText, buttonTexts] of [
      [{ name: "default", status: "connected" as const, user: "alice", auth_mode: "oidc" as const, error: null }, "alice", "Connected as alice", ["Switch Account", "Disconnect"]],
      [{ name: "default", status: "connected" as const, user: "alice", auth_mode: "none" as const, error: "Restore failed" }, "alice", "Restore failed", ["Reconnect", "Disconnect"]],
      [{ name: "default", status: "disconnected" as const, user: null, auth_mode: "none" as const, error: null }, "Offline", "Disconnected", ["Connect"]],
      [{ name: "default", status: "disconnected" as const, user: null, auth_mode: "oidc" as const, error: null }, "Offline", "Disconnected", ["Connect", "Sign In…"]],
      [{ name: "default", status: "connecting" as const, user: null, auth_mode: "none" as const, error: null }, "Connecting…", "Connecting to hub…", ["Cancel"]],
    ] as const) {
      diagnostics.length = 0;
      bridge.state([hubState], []);
      await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=0"));
      const snap = await inspect(), snapNodes = flatten(snap.items);
      expect(snapNodes.find(node => node.id === "hub:default")?.text).toBe(pillText);
      const drawer = snap.items.find(node => node.id === "drawer:default")!;
      expect(flatten([drawer]).map(node => node.text)).toContain(summaryText);
      const renderedButtons = drawer.children.filter(child => child.id.endsWith(":default")).map(child => child.text);
      expect([...renderedButtons].sort()).toEqual([...buttonTexts].sort());
    }
  } finally { await bridge.close(); }
}, 15000);

test("HUD presents English wake/passive counts and harness-specific click labels", async () => {
  const diagnostics: string[] = [];
  const bridge = new UiBridge(root, { binary, env: { CITO_UI_HEADLESS: "1" }, diagnostic: line => diagnostics.push(line) });
  try {
    await bridge.start();
    bridge.banner({ ...payload, count: 2 });
    await waitFor(() => diagnostics.some(line => line.includes('"semantic_label":"Notification · ×2"')));
    const wake = JSON.parse(diagnostics.find(line => line.startsWith("cito-ui: display "))!.slice("cito-ui: display ".length));
    expect(wake.action_label).toBe("Open in Codex ↗");
    bridge.banner({ ...payload, target_harness: "claude", effective_wake: false });
    await waitFor(() => diagnostics.some(line => line.includes('"semantic_label":"Passive"')));
    const passive = JSON.parse(diagnostics.filter(line => line.startsWith("cito-ui: display ")).at(-1)!.slice("cito-ui: display ".length));
    expect(passive).toMatchObject({ action_label: "claude session · Click to dismiss", subtitle: "[Passive] alice@host · sender", display_subtitle: "[Passive] alice@host · sender" });
  } finally { await bridge.close(); }
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
  await writeFile(script, `#!${process.execPath}\nconst b=Buffer.from(JSON.stringify({op:'hub_action',hub:'café',action:'disconnect'})+'\\n'); process.stdout.write('bad\\n'+JSON.stringify({op:'set_scope',hub:'a',listener_id:'invalid',scope:'self'})+'\\n'+'x'.repeat(1048577)+'\\n'); process.stdout.write(b.subarray(0,b.indexOf(Buffer.from('café'))+1)); setTimeout(()=>process.stdout.write(b.subarray(b.indexOf(Buffer.from('café'))+1)),20); process.stdin.resume(); process.stdin.on('end',()=>process.exit(0));`, { mode: 0o700 });
  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, { binary: script, diagnostic: line => diagnostics.push(line), onAction: action => { actions.push(action); } });
  try {
    await bridge.start(); await waitFor(() => actions.length === 1);
    expect(actions).toEqual([{ op: "hub_action", hub: "café", action: "disconnect" }]);
    expect(diagnostics.filter(line => line.includes("invalid frame")).length).toBe(2);
  } finally { await bridge.close(); }
});
