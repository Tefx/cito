import { afterAll, beforeAll, expect, test } from "vitest";
import { mkdtemp, rm, stat, utimes, writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";
import { ensureUiCompiled, UiBridge, type BannerPayload, type UiActivityItem, type UiHarnessMetadata, type UiSettingsState, type UiToDaemonAction, type UiListenerSummary, type UiRemotePeerSummary } from "../support/native-ui.js";
import { cleanroom, action, snapshot } from "../support/native-scenarios.js";
import { NativeHarness } from "../support/native-harness.js";
import { waitFor } from "../support/native-harness.js";

let root: string, binary: string, suite: NativeHarness;
const savedEnv={...process.env};
beforeAll(async () => { suite=await NativeHarness.create(); root=suite.root; Object.assign(process.env,suite.env); binary=await ensureUiCompiled(root); },120000);
afterAll(async()=>{await suite.close(); for(const key of Object.keys(process.env))if(!(key in savedEnv))delete process.env[key];Object.assign(process.env,savedEnv);});
const listener: UiListenerSummary = { hub: "personal", listener_id: "c_abcdefghijkl", public_id: "repo-abcdefghijkl", scoped_id: "personal:repo-abcdefghijkl",
  harness: "codex", session_id: "thread-1", title: "café conversation", cwd: "/tmp", repo: null, scope: "self" };
const hubs = [{ name: "personal", status: "connected" as const, user: "alice", auth_mode: "oidc" as const, error: null }];
const payload: BannerPayload = { target_harness: "codex", target_session_id: "thread-1", target_title: "café conversation", from_user: "alice", from_host: "host", from_title: "sender", message: "hello café", effective_wake: true, signal_id: "personal:sig_1", hub: "personal", from_scoped_id: "personal:sender-abcdefgh2345" };
function childOf(bridge: UiBridge) { return Reflect.get(bridge, "child") as ChildProcessWithoutNullStreams | undefined; }

test("native config mutations serialize same-file async work, release failures, and preserve unrelated keys", async () => {
 const f=await cleanroom();await f.daemon();
 await Promise.all([action(f,{op:"set_desktop_banner",enabled:false}),action(f,{op:"setup_save_hub",name:"work",url:"local://",auth_mode:"none"})]);
 const config=JSON.parse(await readFile(f.configPath,"utf8"));expect(config.hubs.work).toMatchObject({url:"local://"});expect(config.desktopBannerEnabled).toBe(false);
 expect((await action(f,{op:"set_scope",hub:"local",listener_id:"invalid",scope:"invalid"},true)).http_status).toBe(400);
 await action(f,{op:"set_desktop_banner",enabled:true});expect(JSON.parse(await readFile(f.configPath,"utf8")).desktopBannerEnabled).toBe(true);
});

test("compiled cito-ui decodes UTF-8 fragmented frames, drives actual menu actions, opens DirectoryPanel on banner click, and exits on EOF", async () => {
  const child = spawn(binary, [], { stdio: "pipe", env: { ...process.env, CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "120" } });
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
    await waitFor(() => stderr.includes("banner clicked"));
    expect(child.exitCode).toBeNull();
    for (const index of [1, 2, 3, 4, 5, 6, 7, 8]) send({ op: "test_action", index });
    await waitFor(() => stdout.trim().split("\n").length === 9);
    expect(stdout.trim().split("\n").map(line => JSON.parse(line))).toEqual([
      { op: "open_directory", mode: "activity" },
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

interface MenuNode { id: string; text?: string; tooltip?: string; symbol?: string; green?: boolean; hovered?: boolean; reuse_identifier?: string; background?: number[]; border?: number[]; border_width?: number; foreground?: number[]; tint?: number[]; width: number; height: number; x: number; y: number; radius: number; hidden?: boolean; secure?: boolean; children: MenuNode[] }
interface MenuSnapshot { icon_alpha: Record<string, number>; button_title: string; button_image_position: number; button_font_size: number; button_font_weight: number; button_font_features: { type: number; selector: number }[]; button_digit_widths: number[]; items: MenuNode[]; actions: number; count: number; available_count?: number; rebuilds?: number; directory_open?: boolean; directory_mode?: string; activity_filter?: string; unread_count?: number; composer_open?: boolean; composer_target?: string; composer_reply_signal_id?: string; directory_online_count?: number; directory_matched_count?: number; directory_query?: string; directory_scope?: string; directory_harness?: string; directory_selected_id?: string; directory_items?: MenuNode[]; settings_open?: boolean; settings_tab?: string; settings_launch_at_login?: boolean; settings_items?: MenuNode[]; icon_template: boolean; icon_width: number; icon_height: number; clipboard: string }
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
    expect(initial).toMatchObject({ actions: 9, count: 1, icon_template: true, icon_width: 18, icon_height: 18 });
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
    expect(nodes.find(node => node.id === "scope:" + listener.scoped_id)).toMatchObject({ text: "Self", symbol: "lock.fill", x: 262, y: 30, width: 62, height: 22 });
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
      scoped_id: "superlongrepositoryslugname-f8k2abcdefgh", harness: "pi",
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

test("HUD presents English wake/passive counts and Hub reply click label", async () => {
  const diagnostics: string[] = [];
  const bridge = new UiBridge(root, { binary, env: { CITO_UI_HEADLESS: "1" }, diagnostic: line => diagnostics.push(line) });
  try {
    await bridge.start();
    bridge.banner({ ...payload, count: 2 });
    await waitFor(() => diagnostics.some(line => line.includes('"semantic_label":"Notification · ×2"')));
    const wake = JSON.parse(diagnostics.find(line => line.startsWith("cito-ui: display "))!.slice("cito-ui: display ".length));
    expect(wake.action_label).toBe("Click to reply in Cito ↩");
    bridge.banner({ ...payload, target_harness: "custom", effective_wake: false });
    await waitFor(() => diagnostics.some(line => line.includes('"semantic_label":"Passive"')));
    const passive = JSON.parse(diagnostics.filter(line => line.startsWith("cito-ui: display ")).at(-1)!.slice("cito-ui: display ".length));
    expect(passive).toMatchObject({ action_label: "Click to reply in Cito ↩", subtitle: "[Passive] alice@host · sender", display_subtitle: "[Passive] alice@host · sender" });
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

test("native daemon owns the UI child, emits current snapshots, preserves state after UI crash and reaps children on shutdown", async () => {
 const f=await cleanroom();const daemon=await f.daemon({CITO_DISABLE_UI:"0"});const pi=await f.piBridge();await f.tool("cito_listen",{about:"Snapshot replay"},pi.meta);
 const first=await snapshot(f);expect(first.listeners).toHaveLength(2);
 // Darwin ps does not support --ppid; use the portable parent-column listing.
 const pids=async()=>{const result=await f.command(["-axo","pid=,ppid=,comm="],{executable:"/bin/ps"});return result.stdout.split("\n").filter(l=>l.includes("cito-ui")).map(l=>l.trim().split(/\s+/)).filter(p=>Number(p[1])===daemon.child.pid).map(p=>Number(p[0]));};
 const before=await pids();expect(before).toHaveLength(1);process.kill(before[0]!,"SIGKILL");
 await waitFor(async()=>{const current=await pids();return current.length===1&&current[0]!==before[0];},5000);
 expect((await snapshot(f)).listeners).toEqual(first.listeners);
 await daemon.stop();await waitFor(async()=>(await pids()).length===0);
},15000);

test("native daemon discards malformed/oversized UI action lines without accepting invalid state changes", async () => {
 const f=await cleanroom();await f.daemon();
 for(const body of ["bad json","x".repeat(1048577),JSON.stringify({op:"set_scope",hub:"local",listener_id:"invalid",scope:"invalid"})]){
 const response=await fetch(`http://127.0.0.1:${f.daemonPort}/ui`,{method:"POST",headers:{"content-type":"application/json"},body});expect(response.status).toBeGreaterThanOrEqual(400);}
 expect((await f.mcp("tools/list")).tools).toHaveLength(5);
 const reply=await action(f,{op:"setup_save_hub",name:"unicode",url:"local://",auth_mode:"none",title:"café"});expect(reply.http_status).toBe(200);
 expect(JSON.parse(await readFile(f.configPath,"utf8")).hubs.unicode.title).toBe("café");
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
      { harness: "claude", session_id: "claude-1", title: "Claude 1", cwd: "/tmp/claude1", branch: "main", endpoint_pid: 2001 },
    ];
    const groupMetadata:UiHarnessMetadata[]=[
      {id:"codex",display_name:"Codex",sf_symbol:"terminal",tint:"green",tier:"stable",order:1},
      {id:"claude",display_name:"Claude Code",sf_symbol:"cpu.fill",tint:"orange",tier:"experimental",order:2},
      {id:"pi",display_name:"Pi Coding Agent",sf_symbol:"terminal",tint:"purple",tier:"stable",order:0},
    ];
    bridge.state(hubs, [listener], multiGroupCandidates, myRemote, 412,groupMetadata);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1 available=11"));
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
    expect(nodes.find(n => n.id === "toggle_group:pi")).toMatchObject({ text: "Pi Coding Agent · 5 open", symbol: "chevron.down" });
    expect(nodes.find(n => n.id === "toggle_group:codex")).toMatchObject({ text: "Codex · 5 recent", symbol: "chevron.down" });
    expect(nodes.find(n => n.id === "toggle_group:claude")).toMatchObject({ text: "Claude Code · 1 open", symbol: "chevron.down", tooltip: "Claude Code (Experimental)" });
    const groupOrder = snap.items.filter(n => n.id.startsWith("group:")).map(n => n.id);
    expect(groupOrder).toEqual(["group:pi", "group:codex", "group:claude"]);
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

    // Mouse row selection via tableViewSelectionDidChange syncs directorySelectedIndex and Return copy
    send({ op: "test_control", id: "dir_peer:" + teamPeer.scoped_id });
    const afterRowSelect = await inspect();
    expect(afterRowSelect.directory_selected_id).toBe(teamPeer.scoped_id);
    send({ op: "test_directory_key", key: "return" });
    expect((await inspect()).clipboard).toBe(teamPeer.scoped_id);
    send({ op: "test_control", id: "dir_peer:" + myRemote[0]!.scoped_id });
    expect((await inspect()).directory_selected_id).toBe(myRemote[0]!.scoped_id);

    // Facet pills and search query hooks
    const facetPills=[...groupMetadata].sort((a,b)=>a.order!-b.order!).map(meta=>{
      const pill=dirNodes.find(n=>n.id==="dir_harness:"+meta.id)!;
      expect(pill.text).toBe(meta.display_name);expect(pill.width).toBeGreaterThanOrEqual(42);
      return pill;
    });
    expect(facetPills[0]!.x).toBe(256);
    for(let n=1;n<facetPills.length;n++)expect(facetPills[n]!.x).toBe(facetPills[n-1]!.x+facetPills[n-1]!.width+4);
    expect(facetPills[0]!.width).toBeGreaterThan(facetPills[1]!.width);
    expect(facetPills.at(-1)!.x+facetPills.at(-1)!.width).toBeLessThanOrEqual(544);
    send({ op: "test_control", id: "dir_scope:mine" });
    await waitFor(() => actions.length === 2);
    expect(actions[1]).toEqual({ op: "query_directory", query: "", scope: "mine", harness: "all" });
    expect((await inspect()).directory_matched_count).toBe(1);
    send({ op: "test_control", id: "dir_scope:all" });
    send({ op: "test_control", id: "dir_harness:claude" });
    await waitFor(() => actions.length === 4);
    expect(actions[3]).toEqual({ op: "query_directory", query: "", scope: "all", harness: "claude" });
    send({ op: "test_control", id: "dir_harness:codex" });
    await waitFor(() => actions.length === 5);
    expect(actions[4]).toEqual({ op: "query_directory", query: "", scope: "all", harness: "codex" });
    send({ op: "test_directory_query", query: "@bob repo:billing" });
    await waitFor(() => actions.length === 6);
    expect(actions[5]).toEqual({ op: "query_directory", query: "@bob repo:billing", scope: "all", harness: "codex" });
    expect((await inspect()).directory_selected_id).toBe(teamPeer.scoped_id);

    // Escape closes DirectoryPanel, clears its hover, and emits close_directory.
    send({ op: "test_hover", id: "dir_scope:mine", hovered: true });
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(true);
    send({ op: "test_directory_key", key: "escape" });
    await waitFor(() => actions.length === 7);
    expect(actions[6]).toEqual({ op: "close_directory" });
    const closed = await inspect();
    expect(closed.directory_open).toBe(false);
    expect(flatten(closed.directory_items ?? []).find(n => n.id === "dir_scope:mine")?.hovered).toBe(false);
  } finally { await bridge.close(); }
}, 20000);

test("V9 validAction validation, Zone 1 switch_hub:<scoped_id> pill, and Zone 3 target_hub:<hub> selector & filtering", async () => {

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

    // Symmetric-difference witness: same (harness, session_id) listening on both personal and work
    // produces 2 listener cards (listeners.count === 2) while uniqueSessionCount === 1
    const workListener = {
      ...listener,
      hub: "work",
      listener_id: "c_workabcdefgh",
      public_id: "repo-workabcdefgh",
      scoped_id: "work:repo-workabcdefgh",
    };
    bridge.state(twoHubs, [listener, workListener], []);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=2 listeners=2"));
    const dualSnap = await inspect(), dualNodes = flatten(dualSnap.items);
    expect(dualSnap.count).toBe(1);
    expect(dualSnap.button_title).toBe(" 1");
    expect(dualNodes.find(n => n.id === "header_subtitle")?.text).toBe("1 listening");
    expect(dualNodes.find(n => n.id === "session:" + listener.scoped_id)).toBeDefined();
    expect(dualNodes.find(n => n.id === "session:" + workListener.scoped_id)).toBeDefined();
  } finally { await bridge.close(); }

  // UiBridge.close() settles promptly when the child emits error (e.g. non-existent binary)
  const errDiagnostics: string[] = [];
  const errBridge = new UiBridge(root, { binary: join(root, "nonexistent-cito-ui"), diagnostic: l => errDiagnostics.push(l) });
  await errBridge.start();
  await expect(errBridge.close()).resolves.toBeUndefined();
}, 15000);

test("prebuilt native UI is an ad-hoc-signed arm64 Mach-O installed by native setup without Node",async()=>{
 const f=await cleanroom();const file=fileURLToPath(new URL("../../bin/cito-ui-darwin-arm64",import.meta.url));const bytes=await readFile(file);
 expect(bytes.readUInt32LE(0)).toBe(0xfeedfacf);expect(bytes.readUInt32LE(4)).toBe(0x0100000c);
 const signed=await f.command(["-v",file],{executable:"/usr/bin/codesign"});expect(signed.code).toBe(0);
 const before=performance.now();const result=await f.command(["setup","--enable","human","--no-login-item"]);expect(result.code).toBe(0);
 const target=join(f.stateDir,"plugins/human/bin/cito-ui");expect((await stat(target)).mode&0o111).toBeGreaterThan(0);expect((await readFile(target)).equals(bytes)).toBe(true);
 expect(performance.now()-before).toBeLessThan(5000);
});

test("cito-ui renders custom plugin badges, group headers, horizontal facet scroll overflow, and WakeBanner click opens DirectoryPanel in Activity mode with ComposerBar", async () => {
  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, {
    binary,
    env: { CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "120" },
    diagnostic: line => diagnostics.push(line),
    onAction: action => { actions.push(action); },
  });
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
    const customHarnesses: UiHarnessMetadata[] = [
      { id: "pi", display_name: "Pi", sf_symbol: "terminal.fill", tint: "indigo", tier: "stable", order: 0 },
      { id: "codex", display_name: "Codex", sf_symbol: "sparkles", tint: "teal", tier: "stable", order: 1 },
      { id: "claude", display_name: "Claude", sf_symbol: "cpu.fill", tint: "orange", tier: "experimental", order: 2 },
      { id: "cursor", display_name: "Cursor", sf_symbol: "cursorarrow.rays", tint: "purple", tier: "custom", order: 3 },
    ];
    const cursorListener: UiListenerSummary = {
      hub: "personal",
      listener_id: "c_cursor234567",
      public_id: "cursor-cursor234567",
      scoped_id: "personal:cursor-cursor234567",
      harness: "cursor",
      session_id: "cur-1",
      title: "Cursor Workspace",
      cwd: "/tmp/cursor-proj",
      repo: "acme/service",
      scope: "self",
    };
    const candidates = [
      { harness: "zed", session_id: "zed-1", title: "Zed Session", cwd: "/tmp/zed" },
      { harness: "cursor", session_id: "cur-2", title: "Cursor Recent", cwd: "/tmp/cur2" },
    ];
    bridge.state(hubs, [cursorListener], candidates, [], 5, customHarnesses);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1 available=2"));
    const snap = await inspect(), nodes = flatten(snap.items);

    // open:<scoped_id> jump button is completely absent
    expect(nodes.find(n => n.id === "open:" + cursorListener.scoped_id)).toBeUndefined();

    // Zone 3: custom plugins without endpoint_pid show "available"
    expect(nodes.find(n => n.id === "toggle_group:cursor")).toMatchObject({ text: "Cursor · 1 available" });
    expect(nodes.find(n => n.id === "toggle_group:zed")).toMatchObject({ text: "Zed · 1 available" });

    // Surface 2: single custom plugin pill at x: 430 (no overflow yet), then overflow > 544pt hosts pills in directory_harness_scroll
    send({ op: "test_control", id: "open_directory" });
    await waitFor(() => actions.length === 1);
    const dirSnap1 = await inspect(), dirNodes1 = flatten(dirSnap1.directory_items ?? []);
    expect(dirNodes1.find(n => n.id === "dir_harness:cursor")).toMatchObject({ text: "Cursor", x: 430 });
    expect(dirNodes1.find(n => n.id === "directory_harness_scroll")).toBeUndefined();

    // Add more custom plugins so total width exceeds 544pt -> hosted in horizontal NSScrollView
    const manyHarnesses: UiHarnessMetadata[] = [
      ...customHarnesses,
      { id: "windsurf", display_name: "Windsurf", sf_symbol: "wind", tint: "green", tier: "custom", order: 3 },
      { id: "zed", display_name: "Zed Editor", sf_symbol: "bolt.horizontal.fill", tint: "blue", tier: "custom", order: 3 },
    ];
    bridge.state(hubs, [cursorListener], candidates, [], 5, manyHarnesses);
    await waitFor(async () => flatten((await inspect()).directory_items ?? []).some(n => n.id === "directory_harness_scroll"));
    const dirSnap2 = await inspect(), dirNodes2 = flatten(dirSnap2.directory_items ?? []);
    expect(dirNodes2.find(n => n.id === "directory_harness_scroll")).toMatchObject({ x: 256, width: 288, height: 22 });
    expect(dirNodes2.find(n => n.id === "dir_harness:windsurf")?.text).toBe("Windsurf");
    expect(dirNodes2.find(n => n.id === "dir_harness:zed")?.text).toBe("Zed Editor");

    // Clicking WakeBanner opens DirectoryPanel in "activity" mode with composer_target set to payload.from_scoped_id
    bridge.banner({ ...payload, target_harness: "cursor", target_session_id: "cur-1", signal_id: "personal:sig_banner_99", from_scoped_id: "personal:bob-abcdefgh2345" });
    await waitFor(() => diagnostics.some(l => l.includes('"action_label":"Click to reply in Cito ↩"')));
    send({ op: "test_click" });
    await waitFor(() => actions.some(a => a.op === "open_directory" && a.mode === "activity"));
    const afterBannerClick = await inspect();
    expect(afterBannerClick).toMatchObject({
      directory_open: true,
      directory_mode: "activity",
      composer_open: true,
      composer_target: "personal:bob-abcdefgh2345",
      composer_reply_signal_id: "personal:sig_banner_99",
    });
  } finally {
    await bridge.close();
  }
}, 20000);

test("Surface 3 SettingsPanel (520x460pt) renders Agent Apps & Hubs tabs, 1-click Set Up, LAN Hub Sharing, Paste from Invite, inline Add Hub form, Launch at Login, and Escape close", async () => {
  // Native action validation is exercised through /ui in tests/mcp/ui.test.ts.

  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, {
    binary,
    env: { CITO_UI_HEADLESS: "1" },
    diagnostic: line => diagnostics.push(line),
    onAction: action => { actions.push(action); },
  });
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

    const settings1: UiSettingsState = {
      apps: [
        {
          id: "pi",
          display_name: "Pi",
          tier: "stable",
          installed: true,
          configured: false,
          status: "not_configured",
          detail: "Installed on this Mac · Click Set Up to connect",
          paths: ["/tmp/.pi/agent/settings.json"],
        },
        {
          id: "codex",
          display_name: "Codex",
          tier: "stable",
          installed: true,
          configured: true,
          status: "ready",
          detail: "Connected and ready for Cito notifications",
          paths: ["/tmp/.codex/config.toml"],
        },
        {
          id: "claude",
          display_name: "Claude",
          tier: "experimental",
          installed: true,
          configured: false,
          status: "not_configured",
          detail: "Installed on this Mac · Click Set Up to connect",
          paths: ["/tmp/.claude/settings.json"],
        },
      ],
      hubs: [
        {
          name: "local",
          url: "local://",
          auth_mode: "none",
          enabled: true,
          is_default: true,
          status: "connected",
          user: "alice",
          error: null,
        },
        {
          name: "tailnet",
          url: "http://100.64.0.10:4291",
          auth_mode: "token",
          enabled: true,
          is_default: false,
          status: "connected",
          user: "alice",
          error: null,
        },
      ],
      host_hub: {
        enabled: false,
        running: false,
        port: 4291,
        endpoint_url: "http://192.168.1.20:4291",
        token: "secret-lan-token-123",
        invite_command: "cito setup --hub alice-mac --url http://192.168.1.20:4291 --auth token --token secret-lan-token-123 --default",
        error: null,
      },
      launch_at_login: false,
    };

    bridge.state(hubs, [listener], [], [], 1, undefined, settings1);
    await waitFor(() => diagnostics.includes("cito-ui: state hubs=1 listeners=1"));

    const menuSnap = await inspect();
    const menuNodes = flatten(menuSnap.items);
    expect(menuSnap.actions).toBe(9);
    expect(menuNodes.find(n => n.id === "open_settings")).toMatchObject({
      text: "Settings…",
      symbol: "gearshape",
      x: 168,
      y: 6,
      width: 88,
      height: 24,
    });

    // Click Settings… button in footer -> opens Surface 3 SettingsPanel (520x460pt) on Agent Apps tab
    send({ op: "test_control", id: "open_settings" });
    await waitFor(() => actions.some(a => a.op === "open_settings"));
    const openSnap = await inspect();
    expect(openSnap).toMatchObject({
      settings_open: true,
      settings_tab: "apps",
      settings_launch_at_login: false,
    });
    const settingsNodes1 = flatten(openSnap.settings_items ?? []);
    expect(settingsNodes1.find(n => n.id === "settings_panel")).toMatchObject({ width: 520, height: 460 });
    expect(settingsNodes1.find(n => n.id === "settings_tab:apps")).toMatchObject({
      text: "Agent Apps",
      symbol: "square.grid.2x2",
      x: 58,
      y: 12,
      width: 118,
      height: 24,
    });
    expect(settingsNodes1.find(n => n.id === "settings_tab:channels")).toMatchObject({
      text: "Human Channels",
      symbol: "bubble.left.and.bubble.right",
      x: 182,
      y: 12,
      width: 148,
      height: 24,
    });
    expect(settingsNodes1.find(n => n.id === "settings_tab:hubs")).toMatchObject({
      text: "Hubs & Network",
      symbol: "network",
      x: 336,
      y: 12,
      width: 126,
      height: 24,
    });
    expect(settingsNodes1.find(n => n.id === "settings_app_status:pi")?.text).toBe("○ Not Set Up");
    expect(settingsNodes1.find(n => n.id === "setup_app:pi")?.text).toBe("Set Up");
    expect(settingsNodes1.find(n => n.id === "settings_app_status:codex")?.text).toBe("● Connected");
    expect(settingsNodes1.find(n => n.id === "setup_app:codex")?.text).toBe("Re-check");
    expect(settingsNodes1.find(n => n.id === "settings_app_tier:claude")).toBeDefined();

    // Click Set Up on Pi -> emits setup_apply
    send({ op: "test_control", id: "setup_app:pi" });
    await waitFor(() => actions.some(a => a.op === "setup_apply" && a.harness === "pi"));

    // Switch to Hubs & Network tab
    send({ op: "test_control", id: "settings_tab:hubs" });
    const hubsTabSnap = await inspect();
    expect(hubsTabSnap.settings_tab).toBe("hubs");
    const hubsNodes = flatten(hubsTabSnap.settings_items ?? []);
    expect(hubsNodes.find(n => n.id === "host_hub_card")).toMatchObject({ x: 20, y: 232, width: 480, height: 124 });
    expect(hubsNodes.find(n => n.id === "toggle_host_hub")?.text).toBe("Off");
    expect(hubsNodes.find(n => n.id === "host_hub_endpoint")?.text).toBe("http://192.168.1.20:4291");

    // Toggle Share Hub on Local Network -> emits set_host_hub
    send({ op: "test_control", id: "toggle_host_hub" });
    await waitFor(() => actions.some(a => a.op === "set_host_hub" && a.enabled === true));

    // Update settings state (even with unchanged hubs/listeners, receive(_:) rebuilds settingsPanel because frame.settings != settings)
    const settings2: UiSettingsState = {
      ...settings1,
      host_hub: { ...settings1.host_hub, enabled: true, running: true },
      launch_at_login: true,
    };
    bridge.state(hubs, [listener], [], [], 1, undefined, settings2);
    await waitFor(async () => flatten((await inspect()).settings_items ?? []).find(n => n.id === "toggle_host_hub")?.text === "● Sharing");

    // Copy 1-Click Join Code -> copies invite_command to clipboard and shows ✓ Copied
    send({ op: "test_control", id: "copy_hub_invite" });
    const afterCopy = await inspect();
    expect(afterCopy.clipboard).toBe(settings1.host_hub.invite_command);
    expect(flatten(afterCopy.settings_items ?? []).find(n => n.id === "copy_hub_invite")).toMatchObject({
      text: "✓ Copied",
      green: true,
    });

    // Paste from Invite -> parses invite command from clipboard and emits setup_save_hub
    send({
      op: "test_clipboard",
      text: "cito setup --hub studio-mac --url http://100.64.0.99:4291 --auth token --token lan-token-xyz --default",
      system_only: true,
    });
    send({ op: "test_control", id: "paste_hub_invite" });
    await waitFor(() =>
      actions.some(
        a =>
          a.op === "setup_save_hub" &&
          a.name === "studio-mac" &&
          a.url === "http://100.64.0.99:4291" &&
          a.auth_mode === "token" &&
          a.token === "lan-token-xyz",
      ),
    );

    // Inline + Add Hub… form with test_settings_input and submit_add_hub
    send({ op: "test_control", id: "open_add_hub" });
    const formSnap = await inspect();
    expect(flatten(formSnap.settings_items ?? []).find(n => n.id === "add_hub_form")).toBeDefined();
    send({ op: "test_control", id: "settings_hub_auth:oidc" });
    send({ op: "test_settings_input", id: "settings_hub_name", value: "team" });
    send({ op: "test_settings_input", id: "settings_hub_url", value: "https://hub.example.com" });
    send({ op: "test_settings_input", id: "settings_hub_issuer", value: "https://id.example.com" });
    send({ op: "test_settings_input", id: "settings_hub_client_id", value: "cito-client" });
    send({ op: "test_control", id: "submit_add_hub" });
    await waitFor(() =>
      actions.some(
        a =>
          a.op === "setup_save_hub" &&
          a.name === "team" &&
          a.url === "https://hub.example.com" &&
          a.auth_mode === "oidc" &&
          a.issuer === "https://id.example.com" &&
          a.client_id === "cito-client",
      ),
    );

    // Toggle hub enabled & remove non-local hub & toggle Launch at Login
    send({ op: "test_control", id: "toggle_hub:local" });
    await waitFor(() => actions.some(a => a.op === "setup_toggle_hub" && a.hub === "local" && a.enabled === false));
    send({ op: "test_control", id: "remove_hub:tailnet" });
    await waitFor(() => actions.some(a => a.op === "setup_remove_hub" && a.hub === "tailnet"));
    send({ op: "test_control", id: "settings_launch_at_login" });
    await waitFor(() => actions.some(a => a.op === "set_launch_at_login" && a.enabled === false));

    // Escape closes SettingsPanel and emits close_settings
    send({ op: "test_settings_key", key: "escape" });
    await waitFor(() => actions.some(a => a.op === "close_settings"));
    expect((await inspect()).settings_open).toBe(false);
  } finally {
    await bridge.close();
  }
}, 25000);

test("Stage 3 Directory | Activity mode switching, inline ComposerBar send_signal & ack_signals, and 3-tab SettingsPanel Human Channels", async () => {
  // Native action validation is exercised through /ui in tests/mcp/ui.test.ts.

  const diagnostics: string[] = [], actions: UiToDaemonAction[] = [];
  const bridge = new UiBridge(root, {
    binary,
    env: { CITO_UI_HEADLESS: "1" },
    diagnostic: line => diagnostics.push(line),
    onAction: action => { actions.push(action); },
  });
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

    const humanListener: UiListenerSummary = {
      hub: "personal",
      listener_id: "c_human2345678",
      public_id: "alice-human2345678",
      scoped_id: "personal:alice-human2345678",
      harness: "human",
      session_id: "default",
      title: "@alice (Me)",
      cwd: "~",
      repo: null,
      scope: "self",
      channels: ["MacBook-Pro", "Telegram"],
    };
    const peer: UiRemotePeerSummary = {
      hub: "personal",
      listener_id: "c_bobpeer23456",
      public_id: "billing-bobpeer23456",
      scoped_id: "personal:billing-bobpeer23456",
      user: "bob",
      host_id: "host-bob",
      host_name: "bob-mac.local",
      harness: "codex",
      title: "Billing API",
      about: "Schema work",
      repo: "acme/billing",
      branch: "main",
      scope: "workspace",
      last_seen_at: new Date().toISOString(),
      is_self_principal: false,
      channels: ["bob-mac"],
    };
    const activityItems: UiActivityItem[] = [
      {
        hub: "personal",
        signal_id: "personal:sig_unread_1",
        listener_id: humanListener.listener_id,
        from_user: "bob",
        from_host_name: "bob-mac",
        from_harness: "codex",
        from_title: "Billing API",
        from_scoped_id: peer.scoped_id,
        target_harness: "human",
        target_title: "@alice (Me)",
        target_scoped_id: humanListener.scoped_id,
        message: "Can you approve the migration?",
        held_reason: null,
        in_reply_to: null,
        created_at: new Date().toISOString(),
        unread: true,
        for_me: true,
      },
      {
        hub: "personal",
        signal_id: "personal:sig_other_2",
        listener_id: listener.listener_id,
        from_user: "alice",
        from_host_name: "mac",
        from_harness: "pi",
        from_title: "Pi Worker",
        from_scoped_id: "personal:pi-abcdefgh2345",
        target_harness: "codex",
        target_title: "café conversation",
        target_scoped_id: listener.scoped_id,
        message: "Tests passed",
        held_reason: null,
        in_reply_to: null,
        created_at: new Date().toISOString(),
        unread: false,
        for_me: false,
      },
    ];
    const settingsWithChannels: UiSettingsState = {
      apps: [],
      channels: [
        {
          id: "telegram",
          display_name: "Telegram",
          sf_symbol: "paperplane.fill",
          tint: "blue",
          enabled: true,
          configured: false,
          status: "hub_ready",
          detail: "Hub Service: ● Ready · Pair or enter your account below",
          display_label: null,
          hub_provided: false,
          pairing_supported: true,
          pairing_action_label: "Pair Account…",
          pairing_code: "cito-ab23cd",
          pairing_instruction: 'Send "/start cito-ab23cd" to @cito_test_bot',
          fields: [
            { key: "botToken", scope: "hub", label: "Bot Token", type: "secret", required: true, placeholder: "123:ABC", value: "123:ABC", configured: true },
            { key: "chatId", scope: "user", label: "Chat ID", type: "string", required: true, placeholder: "123456", value: "", configured: false },
          ],
        },
      ],
      desktop_banner_enabled: true,
      hubs: ["personal", "work"].map(name => ({
        name, url: "local://", auth_mode: "none" as const, enabled: true,
        is_default: name === "personal", status: "connected", user: "alice", error: null,
      })),
      host_hub: {
        enabled: false,
        running: false,
        port: 4291,
        endpoint_url: "http://127.0.0.1:4291",
        token: "",
        invite_command: "cito setup --hub lan --url http://127.0.0.1:4291 --default",
        error: null,
      },
      launch_at_login: false,
    };

    bridge.state(hubs, [listener, humanListener], [], [], 2, undefined, settingsWithChannels, 1, activityItems);
    bridge.directory([peer], 1, 2, activityItems, 1);
    await waitFor(() => diagnostics.some(l => l.startsWith("cito-ui: directory peers=1")));

    // Surface 1: Human Node card is sorted to index 0 of Zone 1 and Zone 4 shows "🔔 1 unread · 2 online"
    const s1 = await inspect();
    const s1SessionIds = s1.items.filter(n => n.id.startsWith("session:")).map(n => n.id);
    expect(s1SessionIds[0]).toBe("session:" + humanListener.scoped_id);
    const s1Nodes = flatten(s1.items);
    expect(s1Nodes.find(n => n.id === "directory_online_badge")?.text).toBe("🔔 1 unread · 2 online");

    // Clicking open_directory when unread > 0 opens DirectoryPanel directly in "activity" mode
    send({ op: "test_control", id: "open_directory" });
    const actSnap = await inspect();
    expect(actSnap).toMatchObject({
      directory_open: true,
      directory_mode: "activity",
      activity_filter: "me",
      unread_count: 1,
    });
    const actNodes = flatten(actSnap.directory_items ?? []);
    expect(actNodes.find(n => n.id === "dir_mode:activity")?.text).toBe("Activity · 1");
    expect(actNodes.find(n => n.id === "activity_row:personal:sig_unread_1")).toBeDefined();
    expect(actNodes.find(n => n.id === "directory_shortcuts")?.text).toBe("↵ Reply    ⎋ Close");

    // Filter switching: act_filter:all shows both items; act_filter:unread shows 1 item
    send({ op: "test_control", id: "act_filter:all" });
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "activity_row:personal:sig_other_2")).toBeDefined();
    send({ op: "test_directory_key", key: "down" });
    expect((await inspect()).directory_selected_id).toBe("personal:sig_other_2");
    send({ op: "test_directory_key", key: "up" });
    expect((await inspect()).directory_selected_id).toBe("personal:sig_unread_1");
    send({ op: "test_control", id: "act_filter:unread" });
    expect(flatten((await inspect()).directory_items ?? []).find(n => n.id === "activity_row:personal:sig_other_2")).toBeUndefined();

    // Clicking Reply on activity row opens ComposerBar targeting peer.scoped_id with in_reply_to
    send({ op: "test_control", id: "reply_activity:personal:sig_unread_1" });
    const replySnap = await inspect();
    expect(replySnap).toMatchObject({
      composer_open: true,
      composer_target: peer.scoped_id,
      composer_reply_signal_id: "personal:sig_unread_1",
    });
    expect(flatten(replySnap.directory_items ?? []).find(n => n.id === "directory_shortcuts")?.text).toBe("↵ Send    ⎋ Cancel Reply");
    send({ op: "test_composer_input", value: "Approved, go ahead!" });
    send({ op: "test_control", id: "composer_send" });
    await waitFor(() => actions.some(a => a.op === "send_signal"));
    expect(actions.find(a => a.op === "send_signal")).toEqual({
      op: "send_signal",
      hub: "personal",
      to: peer.scoped_id,
      message: "Approved, go ahead!",
      in_reply_to: "personal:sig_unread_1",
    });

    send({ op: "composer_result", ok: false, error: "Hub unavailable. Retry." });
    const failedSend = await inspect();
    expect(failedSend.composer_open).toBe(true);
    expect(flatten(failedSend.directory_items ?? []).find(n => n.id === "composer_input")?.text).toBe("Approved, go ahead!");
    expect(flatten(failedSend.directory_items ?? []).find(n => n.id === "composer_error")?.text).toBe("Hub unavailable. Retry.");
    // An Activity-mode state update must preserve the independently loaded directory.
    bridge.state(hubs, [listener, humanListener], [], [], 2, undefined, settingsWithChannels, 1, activityItems);

    // Switch to Directory mode, click Message on peer row, and cancel composer
    send({ op: "test_control", id: "dir_mode:directory" });
    const dirModeSnap = await inspect();
    expect(dirModeSnap.directory_mode).toBe("directory");
    expect(flatten(dirModeSnap.directory_items ?? []).find(n => n.id === "msg_dir:" + peer.scoped_id)).toBeDefined();
    send({ op: "test_control", id: "msg_dir:" + peer.scoped_id });
    expect((await inspect()).composer_target).toBe(peer.scoped_id);
    send({ op: "test_control", id: "composer_cancel" });
    const cancelledComposer = await inspect();
    expect(cancelledComposer.composer_open).toBe(false);
    expect(flatten(cancelledComposer.directory_items ?? []).find(n => n.id === "directory_shortcuts")?.text).toBe("↵ Copy Selected ID    ⎋ Close");

    // Switch back to Activity mode and click ✓ Read All -> emits ack_signals
    send({ op: "test_control", id: "dir_mode:activity" });
    send({ op: "test_control", id: "activity_mark_all_read" });
    await waitFor(() => actions.some(a => a.op === "ack_signals" && a.ack_all === true));

    // Surface 3: Open SettingsPanel and switch to Tab 2 ("channels")
    send({ op: "test_control", id: "open_settings" });
    send({ op: "test_control", id: "settings_tab:channels" });
    const chTabSnap = await inspect();
    expect(chTabSnap.settings_tab).toBe("channels");
    const chNodes = flatten(chTabSnap.settings_items ?? []);
    expect(chNodes.find(n => n.id === "toggle_desktop_banner")?.text).toBe("● Enabled");
    expect(chNodes.find(n => n.id === "settings_channel:telegram")).toBeDefined();
    expect(chNodes.find(n => n.id === "pairing_instruction:telegram")?.text).toBe('Send "/start cito-ab23cd" to @cito_test_bot');

    expect(chNodes.find(n => n.id === "channel_field:telegram:botToken")).toMatchObject({ secure: true, text: "••••" });
    send({ op: "test_control", id: "channel_hub_selector:work" });
    await waitFor(() => actions.some(a => a.op === "open_settings" && a.hub === "work"));
    send({ op: "settings", hub: "work", settings: settingsWithChannels });
    send({ op: "test_settings_input", id: "channel_field:telegram:botToken", value: "new-secret" });
    const pairedSettings = structuredClone(settingsWithChannels);
    pairedSettings.channels![0]!.fields.find(f => f.key === "chatId")!.value = "server-chat";
    send({ op: "settings", hub: "work", settings: pairedSettings });
    const paired = flatten((await inspect()).settings_items ?? []);
    expect(paired.find(n => n.id === "channel_field:telegram:chatId")?.text).toBe("server-chat");
    send({ op: "test_control", id: "save_channel:telegram" });
    await waitFor(() => actions.some(a => a.op === "channel_save" && a.hub === "work" && a.values.chatId === "server-chat" && a.values.botToken === "new-secret"));
    send({ op: "test_control", id: "channel_hub_selector:personal" });
    await waitFor(() => actions.some(a => a.op === "open_settings" && a.hub === "personal"));
    send({ op: "test_control", id: "open_uninstall_confirm" });
    const confirm = flatten((await inspect()).settings_items ?? []);
    expect(confirm.find(n => n.id === "uninstall_confirm_box")).toMatchObject({hidden:false});
    expect(confirm.find(n => n.id === "settings_channels_tab")).toMatchObject({hidden:true});
    send({ op: "test_control", id: "settings_tab:hubs" });
    const switched = flatten((await inspect()).settings_items ?? []);
    expect(switched.find(n => n.id === "uninstall_confirm_box")).toBeUndefined();
    expect(switched.find(n => n.id === "settings_hubs_tab")).toMatchObject({hidden:false});
    send({ op: "test_control", id: "settings_tab:channels" });

    send({ op: "test_control", id: "toggle_desktop_banner" });
    await waitFor(() => actions.some(a => a.op === "set_desktop_banner" && a.enabled === false));

    send({ op: "test_settings_input", id: "channel_field:telegram:chatId", value: "777888" });
    send({ op: "test_control", id: "pair_channel:telegram" });
    await waitFor(() => actions.some(a => a.op === "channel_pair" && a.channel_id === "telegram"));
    send({ op: "test_control", id: "test_channel:telegram" });
    await waitFor(() => actions.some(a => a.op === "channel_test" && a.channel_id === "telegram"));
    send({ op: "test_control", id: "save_channel:telegram" });
    await waitFor(() =>
      actions.some(
        a => a.op === "channel_save" && a.channel_id === "telegram" && a.values.chatId === "777888",
      ),
    );
  } finally {
    await bridge.close();
  }
}, 25000);
