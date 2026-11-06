import { afterAll, beforeAll, expect, test } from "vitest";
import { mkdtemp, rm, writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { ensureUiCompiled, UiBridge, validAction, type BannerPayload, type UiToDaemonAction, type UiListenerSummary, type UiRemotePeerSummary } from "../../src/ui/index.js";
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

interface MenuNode { id: string; text?: string; tooltip?: string; symbol?: string; green?: boolean; hovered?: boolean; reuse_identifier?: string; background?: number[]; border?: number[]; border_width?: number; foreground?: number[]; tint?: number[]; width: number; height: number; x: number; y: number; radius: number; hidden?: boolean; children: MenuNode[] }
interface MenuSnapshot { icon_alpha: Record<string, number>; button_title: string; button_image_position: number; button_font_size: number; button_font_weight: number; button_font_features: { type: number; selector: number }[]; button_digit_widths: number[]; items: MenuNode[]; actions: number; count: number; available_count?: number; rebuilds?: number; directory_open?: boolean; directory_online_count?: number; directory_matched_count?: number; directory_query?: string; directory_scope?: string; directory_harness?: string; directory_selected_id?: string; directory_items?: MenuNode[]; icon_template: boolean; icon_width: number; icon_height: number; clipboard: string }
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
    send({ op: "test_hover", id: "hub:personal", hovered: true });
    expect(flatten((await inspect()).items).find(node => node.id === "hub:personal")?.hovered).toBe(true);
    send({ op: "test_hover", id: "quit", hovered: true });
    const movedHover = flatten((await inspect()).items);
    expect(movedHover.find(node => node.id === "hub:personal")?.hovered).toBe(false);
    expect(movedHover.find(node => node.id === "quit")?.hovered).toBe(true);
    send({ op: "test_menu_close" });
    expect(flatten((await inspect()).items).filter(node => node.hovered)).toEqual([]);

    // Appearance changes resolve semantic CALayer colors and keep neutral hover achromatic.
    for (const appearance of ["dark", "light"]) {
      send({ op: "test_appearance", appearance });
      const resting = flatten((await inspect()).items);
      const neutral = resting.find(node => node.id === "quit")!;
      expect(neutral.background?.[3]).toBeCloseTo(0.09, 2);
      expect(neutral.border?.[3]).toBeCloseTo(0.14, 2);
      expect(neutral.border_width).toBe(1);
      const card = resting.find(node => node.width === 360 && node.height === 58)!;
      expect(card.background?.[3]).toBeCloseTo(0.07, 2);
      expect(card.border?.[3]).toBeCloseTo(0.11, 2);
      expect(card.background?.[0]).toBe(appearance === "dark" ? 1 : 0);
      send({ op: "test_hover", id: "quit", hovered: true });
      const hovered = flatten((await inspect()).items).find(node => node.id === "quit")!;
      expect(hovered.background?.[3]).toBeCloseTo(0.18, 2);
      expect(hovered.border?.[3]).toBeCloseTo(0.28, 2);
      expect(hovered.foreground?.slice(0, 3)).toEqual(appearance === "dark" ? [1, 1, 1] : [0, 0, 0]);
      send({ op: "test_hover", id: "stop:" + listener.scoped_id, hovered: true });
      const destructive = flatten((await inspect()).items).find(node => node.id === "stop:" + listener.scoped_id)!;
      expect(destructive.background?.[3]).toBeCloseTo(0.88, 2);
      expect(destructive.border?.[3]).toBeCloseTo(0.26, 2);
      expect(destructive.children.find(child => child.tint?.length)?.tint).toEqual([1, 1, 1, 1]);
      send({ op: "test_menu_close" });
    }
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
    expect(copiedNode.background?.[3]).toBeCloseTo(0.16, 2);
    expect(copiedNode.border?.[3]).toBeCloseTo(0.36, 2);
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
    const teamToggle = flatten((await inspect()).items).find(node => node.id === "scope:" + listener.scoped_id)!;
    expect(teamToggle).toMatchObject({ text: "Team", symbol: "person.2.fill", foreground: [1, 1, 1, 1] });
    expect(teamToggle.background?.[3]).toBeCloseTo(0.88, 2);
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
    bridge.banner({ ...payload, target_harness: "custom", effective_wake: false });
    await waitFor(() => diagnostics.some(line => line.includes('"semantic_label":"Passive"')));
    const passive = JSON.parse(diagnostics.filter(line => line.startsWith("cito-ui: display ")).at(-1)!.slice("cito-ui: display ".length));
    expect(passive).toMatchObject({ action_label: "custom session · Click to dismiss", subtitle: "[Passive] alice@host · sender", display_subtitle: "[Passive] alice@host · sender" });
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
    bridge.directory([{
      hub: "personal", listener_id: "c_abcdefghijkl", public_id: "repo-abcdefghijkl", scoped_id: "personal:repo-abcdefghijkl",
      user: "alice", host_id: "h1", host_name: "host", harness: "pi", title: "peer", about: "about", repo: null, scope: "workspace", last_seen_at: "2026-01-01T00:00:00Z",
    }], 1, 1);
    await waitFor(() => diagnostics.includes("cito-ui: directory peers=1 matched=1 online=1"));
    bridge.banner(payload); await waitFor(() => diagnostics.some(line => line.startsWith("cito-ui: display")));
    const old = childOf(bridge)!;
    old.stdin.write(JSON.stringify({ op: "test_action", index: 3 }) + "\n");
    await waitFor(() => actions.length === 1); expect(actions[0]).toEqual({ op: "unlisten", hub: "personal", listener_id: listener.listener_id });
    old.kill("SIGKILL");
    await waitFor(() => childOf(bridge)?.pid !== undefined && childOf(bridge)?.pid !== old.pid);
    await waitFor(() => diagnostics.filter(line => line === "cito-ui: state hubs=1 listeners=1").length === 2);
    await waitFor(() => diagnostics.filter(line => line === "cito-ui: directory peers=1 matched=1 online=1").length === 2);
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

test("cito-ui renders available session candidates, compact empty banner, viewport-capped scroll views, and flicker-free state diffing", async () => {
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
    // menuWillOpen triggers refresh_available
    send({ op: "test_menu_open" });
    await waitFor(() => actions.length === 1);
    expect(actions[0]).toEqual({ op: "refresh_available" });
    // Compact empty strip + available sessions (<= 4 hosted directly)
    const candidates = [
      { harness: "pi", session_id: "pi-1", title: "Pi Build", cwd: "/tmp/project", endpoint_pid: 4242 },
      { harness: "codex", session_id: "codex-1", title: "Codex Review", cwd: "/tmp/review" },
    ];
    bridge.state(hubs, [], candidates);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=0 available=2"));
    const compactSnap = await inspect(), compactNodes = flatten(compactSnap.items);
    expect(compactSnap.items.find(n => n.id === "empty")).toMatchObject({ width: 380, height: 34 });
    expect(compactNodes.map(n => n.text)).toContain("No active listeners — click + Listen below");
    expect(compactSnap.items.find(n => n.id === "available_header")).toMatchObject({ width: 380, height: 24 });
    expect(compactNodes.map(n => n.text)).toContain("AVAILABLE SESSIONS");
    expect(compactSnap.items.find(n => n.id === "candidate:pi:pi-1")).toMatchObject({ width: 380, height: 36 });
    expect(compactNodes.find(n => n.id === "listen:pi:pi-1")).toMatchObject({ text: "Listen", symbol: "plus", x: 304, y: 7, width: 64, height: 22 });
    // Equatable diff guard skips rebuild() on identical state when !pendingMutation
    const rebuildsBefore = compactSnap.rebuilds!;
    send({ op: "state", hubs, listeners: [], available: candidates });
    await waitFor(() => diagnostics.filter(l => l === "cito-ui: state hubs=1 listeners=0 available=2").length === 2);
    const afterIdentical = await inspect();
    expect(afterIdentical.rebuilds).toBe(rebuildsBefore);
    // Clicking + Listen mutates button to "…" with pendingMutation = true and emits listen_session with integer endpoint_pid
    send({ op: "test_control", id: "listen:pi:pi-1" });
    await waitFor(() => actions.length === 2);
    expect(actions[1]).toEqual({ op: "listen_session", harness: "pi", session_id: "pi-1", cwd: "/tmp/project", title: "Pi Build", endpoint_pid: 4242 });
    const optimisticSnap = await inspect();
    expect(flatten(optimisticSnap.items).find(n => n.id === "listen:pi:pi-1")?.text).toBe("…");
    // Sending identical state while pendingMutation == true forces rebuild() and rolls back optimistic "…"
    send({ op: "state", hubs, listeners: [], available: candidates });
    await waitFor(() => diagnostics.filter(l => l === "cito-ui: state hubs=1 listeners=0 available=2").length === 3);
    const rolledBack = await inspect();
    expect(rolledBack.rebuilds).toBe(rebuildsBefore + 1);
    expect(flatten(rolledBack.items).find(n => n.id === "listen:pi:pi-1")?.text).toBe("Listen");
    // Viewport capping: > 4 active listeners -> active_scroll (380 x 260) and > 4 available -> available_scroll (380 x 144)
    const fiveListeners = Array.from({ length: 5 }, (_, i) => ({
      ...listener, listener_id: `c_abcdefghijk${i + 2}`, public_id: `repo-abcdefghijk${i + 2}`,
      scoped_id: `personal:repo-abcdefghijk${i + 2}`, session_id: `s-${i}`, title: `Active ${i}`,
    }));
    const sixCandidates = Array.from({ length: 6 }, (_, i) => ({
      harness: "codex", session_id: `cand-${i}`, title: `Cand ${i}`, cwd: `/tmp/c${i}`,
    }));
    bridge.state(hubs, fiveListeners, sixCandidates);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=5 available=6"));
    const cappedSnap = await inspect();
    const activeScroll = cappedSnap.items.find(n => n.id === "active_scroll")!;
    expect(activeScroll).toMatchObject({ width: 380, height: 260 });
    expect(activeScroll.children.map(c => ({ id: c.id, y: c.y }))).toEqual(
      fiveListeners.map((l, i) => ({ id: "session:" + l.scoped_id, y: i * 64 })),
    );
    const availScroll = cappedSnap.items.find(n => n.id === "available_scroll")!;
    expect(availScroll).toMatchObject({ width: 380, height: 144 });
    expect(availScroll.children.map(c => ({ id: c.id, y: c.y }))).toEqual(
      sixCandidates.map((c, i) => ({ id: `candidate:codex:${c.session_id}`, y: i * 36 })),
    );
  } finally { await bridge.close(); }
}, 15000);

test("V8 Ego-Centric menu zones (My Other Hosts, collapsible harness groups, multi-group scroll IDs, Directory launcher) and Spotlight Directory KeyPanel", async () => {
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
    const myRemote: UiRemotePeerSummary[] = Array.from({ length: 4 }, (_, i) => ({
      hub: "personal",
      listener_id: `c_remotehost0${i + 2}`,
      public_id: `remoteproj-remotehost0${i + 2}`,
      scoped_id: `personal:remoteproj-remotehost0${i + 2}`,
      user: "alice",
      host_id: `host-remote-${i}`,
      host_name: "MacBook-Personal.local",
      harness: i % 2 === 0 ? "pi" : "codex",
      title: `Remote Work ${i}`,
      about: `Remote task ${i}`,
      repo: "acme/service",
      branch: "main",
      scope: "self",
      last_seen_at: new Date(Date.now() - i * 1000).toISOString(),
      is_self_principal: true,
    }));
    const multiGroupCandidates = [
      ...Array.from({ length: 5 }, (_, i) => ({ harness: "pi", session_id: `pi-${i}`, title: `Pi ${i}`, cwd: `/tmp/pi${i}`, branch: "main", endpoint_pid: 1000 + i })),
      ...Array.from({ length: 5 }, (_, i) => ({ harness: "codex", session_id: `cx-${i}`, title: `Codex ${i}`, cwd: `/tmp/cx${i}`, branch: "feat/ui" })),
    ];
    bridge.state(hubs, [listener], multiGroupCandidates, myRemote, 412);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1 available=10"));
    const snap = await inspect(), nodes = flatten(snap.items);
    // Header dynamic subtitle and hubStrip shift
    const subtitleNode = nodes.find(n => n.id === "header_subtitle")!;
    expect(subtitleNode.text).toBe("1 listening · 4 remote");
    expect(subtitleNode.width).toBeGreaterThan(84);
    // Zone 2: My Other Hosts header + >3 scroll container + copy button
    expect(snap.items.find(n => n.id === "my_remote_header")).toMatchObject({ width: 380, height: 26 });
    expect(nodes.find(n => n.id === "toggle_my_remote")).toMatchObject({ text: "MY OTHER HOSTS · 4", symbol: "chevron.down" });
    const remoteScroll = snap.items.find(n => n.id === "my_remote_scroll")!;
    expect(remoteScroll).toMatchObject({ width: 380, height: 102 });
    expect(remoteScroll.children).toHaveLength(4);
    send({ op: "test_control", id: "copy_remote:" + myRemote[0]!.scoped_id });
    expect((await inspect()).clipboard).toBe(myRemote[0]!.scoped_id);
    // Collapse and re-expand My Other Hosts in-place
    send({ op: "test_control", id: "toggle_my_remote" });
    const collapsedRemoteSnap = await inspect();
    expect(collapsedRemoteSnap.items.find(n => n.id === "my_remote_scroll")).toBeUndefined();
    expect(flatten(collapsedRemoteSnap.items).find(n => n.id === "toggle_my_remote")?.symbol).toBe("chevron.right");
    send({ op: "test_control", id: "toggle_my_remote" });
    expect((await inspect()).items.find(n => n.id === "my_remote_scroll")).toBeDefined();

    // Zone 3: Collapsible harness groups + disambiguated scroll IDs + icon-free candidate rows at x: 24
    expect(nodes.find(n => n.id === "toggle_group:pi")).toMatchObject({ text: "Pi · 5 open", symbol: "chevron.down" });
    expect(nodes.find(n => n.id === "toggle_group:codex")).toMatchObject({ text: "Codex · 5 recent", symbol: "chevron.down" });
    expect(snap.items.find(n => n.id === "available_scroll:pi")).toMatchObject({ width: 380, height: 144 });
    expect(snap.items.find(n => n.id === "available_scroll:codex")).toMatchObject({ width: 380, height: 144 });
    const firstCand = nodes.find(n => n.id === "candidate:pi:pi-0")!;
    expect(firstCand.children.find(c => c.text?.includes("Pi 0 · /tmp/pi0 · main"))).toMatchObject({ x: 24, width: 274 });
    send({ op: "test_control", id: "toggle_group:codex" });
    const collapsedGroupSnap = await inspect();
    expect(collapsedGroupSnap.items.find(n => n.id === "available_scroll:codex")).toBeUndefined();
    expect(flatten(collapsedGroupSnap.items).find(n => n.id === "toggle_group:codex")?.symbol).toBe("chevron.right");

    // Zone 4: Network Directory Launcher Row + opening Surface 2 KeyPanel
    expect(snap.items.find(n => n.id === "directory_row")).toMatchObject({ width: 380, height: 34 });
    expect(nodes.find(n => n.id === "open_directory")).toMatchObject({ text: "Search Network Directory…", symbol: "magnifyingglass" });
    expect(nodes.find(n => n.id === "directory_online_badge")?.text).toBe("412 online");
    send({ op: "test_control", id: "open_directory" });
    await waitFor(() => actions.length === 1);
    expect(actions[0]).toEqual({ op: "query_directory", query: "", scope: "all", harness: "all" });

    // Populate DirectoryPanel via bridge.directory(...)
    const teamPeer: UiRemotePeerSummary = {
      hub: "personal",
      listener_id: "c_bobpeer23456",
      public_id: "billing-bobpeer23456",
      scoped_id: "personal:billing-bobpeer23456",
      user: "bob",
      host_id: "host-bob",
      host_name: "bob-mac.local",
      harness: "codex",
      title: "Billing API migration",
      about: "Reviewing schema changes",
      repo: "acme/billing",
      branch: "feat/v8",
      scope: "workspace",
      last_seen_at: new Date().toISOString(),
      is_self_principal: false,
    };
    bridge.directory([myRemote[0]!, teamPeer], 2, 412);
    await waitFor(() => diagnostics.some(l => l.startsWith("cito-ui: directory peers=2 matched=2 online=412")));
    const dirSnap = await inspect(), dirNodes = flatten(dirSnap.directory_items ?? []);
    expect(dirSnap).toMatchObject({
      directory_open: true,
      directory_online_count: 412,
      directory_matched_count: 2,
      directory_query: "",
      directory_scope: "all",
      directory_harness: "all",
      directory_selected_id: myRemote[0]!.scoped_id,
    });
    expect(dirNodes.find(n => n.id === "directory_panel")).toMatchObject({ width: 560, height: 440 });
    expect(dirNodes.find(n => n.id === "dir_peer:" + myRemote[0]!.scoped_id)).toMatchObject({ width: 560, height: 52, reuse_identifier: "DirectoryPeerCell" });
    expect(dirNodes.find(n => n.id === "dir_peer:" + teamPeer.scoped_id)).toMatchObject({ width: 560, height: 52, reuse_identifier: "DirectoryPeerCell" });

    // Menu lifecycle changes must not clear hover in the unchanged directory surface.
    send({ op: "test_hover", id: "dir_scope:mine", hovered: true });
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(true);
    bridge.state(hubs, [listener], multiGroupCandidates, myRemote, 413);
    await waitFor(async () => (await inspect()).rebuilds! > dirSnap.rebuilds!);
    const rebuilt = await inspect();
    expect(rebuilt.rebuilds).toBeGreaterThan(dirSnap.rebuilds!);
    expect(flatten(rebuilt.directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(true);
    send({ op: "test_menu_open" });
    await waitFor(() => actions.at(-1)?.op === "refresh_available");
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(true);
    send({ op: "test_menu_close" });
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(true);
    actions.pop(); // The menu-open refresh is independent of directory query actions.
    expect(dirNodes.find(n => n.id === "directory_summary")?.text).toBe("2 of 412 online");

    // Keyboard navigation (Down -> Return copies selected peer -> Up)
    send({ op: "test_directory_key", key: "down" });
    const afterDown = await inspect();
    expect(afterDown.directory_selected_id).toBe(teamPeer.scoped_id);
    send({ op: "test_directory_key", key: "return" });
    const afterReturn = await inspect();
    expect(afterReturn.clipboard).toBe(teamPeer.scoped_id);
    expect(flatten(afterReturn.directory_items ?? []).find(n => n.id === "copy_dir:" + teamPeer.scoped_id)).toMatchObject({ text: "✓ Copied", green: true });
    send({ op: "test_directory_key", key: "up" });
    const afterUp = await inspect();
    expect(afterUp.directory_selected_id).toBe(myRemote[0]!.scoped_id);
    expect(flatten(afterUp.directory_items ?? []).find(n => n.id === "copy_dir:" + teamPeer.scoped_id)).toMatchObject({ green: false, hovered: false });

    // Facet pills and search query hooks
    send({ op: "test_control", id: "dir_scope:mine" });
    await waitFor(() => actions.length === 2);
    expect(actions[1]).toEqual({ op: "query_directory", query: "", scope: "mine", harness: "all" });
    expect((await inspect()).directory_matched_count).toBe(1);
    send({ op: "test_control", id: "dir_scope:all" });
    send({ op: "test_control", id: "dir_harness:codex" });
    await waitFor(() => actions.length === 4);
    expect(actions[3]).toEqual({ op: "query_directory", query: "", scope: "all", harness: "codex" });
    send({ op: "test_directory_query", query: "@bob repo:billing" });
    await waitFor(() => actions.length === 5);
    expect(actions[4]).toEqual({ op: "query_directory", query: "@bob repo:billing", scope: "all", harness: "codex" });
    expect((await inspect()).directory_selected_id).toBe(teamPeer.scoped_id);

    // Escape closes DirectoryPanel, clears its hover, and emits close_directory.
    send({ op: "test_hover", id: "dir_scope:mine", hovered: true });
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(true);
    send({ op: "test_directory_key", key: "escape" });
    await waitFor(() => actions.length === 6);
    expect(actions[5]).toEqual({ op: "close_directory" });
    const closed = await inspect();
    expect(closed.directory_open).toBe(false);
    expect(flatten(closed.directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(false);
  } finally { await bridge.close(); }
}, 20000);

test("V9 validAction validation, Zone 1 switch_hub:<scoped_id> pill, and Zone 3 target_hub:<hub> selector & filtering", async () => {
  expect(validAction({ op: "switch_hub", from_hub: "personal", to_hub: "work", listener_id: "c_abcdefghijkl" })).toBe(true);
  expect(validAction({ op: "switch_hub", from_hub: "personal", to_hub: "personal", listener_id: "c_abcdefghijkl" })).toBe(false);
  expect(validAction({ op: "switch_hub", from_hub: "BAD!", to_hub: "work", listener_id: "c_abcdefghijkl" })).toBe(false);
  expect(validAction({ op: "switch_hub", from_hub: "personal", to_hub: "work", listener_id: "invalid_id" })).toBe(false);
  expect(validAction({ op: "listen_session", harness: "pi", session_id: "s1", hub: "work", scope: "workspace" })).toBe(true);
  expect(validAction({ op: "listen_session", harness: "pi", session_id: "s1", hub: "BAD!" })).toBe(false);
  expect(validAction({ op: "listen_session", harness: "pi", session_id: "s1", scope: "invalid" })).toBe(false);

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
    const twoHubs = [
      { name: "personal", status: "connected" as const, user: "alice", auth_mode: "none" as const, error: null },
      { name: "work", status: "connected" as const, user: "alice", auth_mode: "oidc" as const, error: null },
    ];
    const candidates = [
      { harness: "pi", session_id: "pi-1", title: "Pi Partial", cwd: "/tmp/pi1", endpoint_pid: 4242, listening_hubs: ["personal"] },
    ];
    bridge.state(twoHubs, [listener], candidates);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=2 listeners=1 available=1"));
    const snap = await inspect(), nodes = flatten(snap.items);

    // Zone 1: switch_hub pill rendered to the left of scope pill, title omits static " · personal" suffix
    const switchPill = nodes.find(n => n.id === "switch_hub:" + listener.scoped_id)!;
    const scopePill = nodes.find(n => n.id === "scope:" + listener.scoped_id)!;
    expect(switchPill).toMatchObject({ text: "personal", height: 22, radius: 6, y: 30 });
    expect(switchPill.x).toBe(scopePill.x - switchPill.width - 4);
    expect(nodes.map(n => n.text)).toContain("café conversation");
    expect(nodes.map(n => n.text)).not.toContain("café conversation · personal");

    // Clicking switch_hub pill emits switch_hub to next connected hub ("work")
    send({ op: "test_control", id: "switch_hub:" + listener.scoped_id });
    await waitFor(() => actions.length === 1);
    expect(actions[0]).toEqual({ op: "switch_hub", from_hub: "personal", to_hub: "work", listener_id: listener.listener_id });

    // Zone 3: effectiveTargetHub defaults to "work" (first connected hub with unmounted candidate)
    expect(nodes.find(n => n.id === "target_hub:personal")).toMatchObject({ text: "personal", height: 18, radius: 9 });
    expect(nodes.find(n => n.id === "target_hub:work")).toMatchObject({ text: "work", height: 18, radius: 9, foreground: [1, 1, 1, 1] });
    expect(nodes.find(n => n.id === "candidate:pi:pi-1")).toBeDefined();

    // Clicking + Listen in multi-hub mode includes "hub": "work"
    send({ op: "test_control", id: "listen:pi:pi-1" });
    await waitFor(() => actions.length === 2);
    expect(actions[1]).toEqual({
      op: "listen_session",
      harness: "pi",
      session_id: "pi-1",
      cwd: "/tmp/pi1",
      title: "Pi Partial",
      endpoint_pid: 4242,
      hub: "work",
    });

    // Switching target_hub to "personal" (where pi-1 is already listening) renders available_empty_target
    send({ op: "test_control", id: "target_hub:personal" });
    const personalSnap = await inspect(), personalNodes = flatten(personalSnap.items);
    expect(personalSnap.items.find(n => n.id === "available_empty_target")).toMatchObject({ width: 380, height: 28 });
    expect(personalNodes.map(n => n.text)).toContain("All sessions listening on personal");
    expect(personalNodes.find(n => n.id === "candidate:pi:pi-1")).toBeUndefined();
  } finally { await bridge.close(); }
}, 15000);
