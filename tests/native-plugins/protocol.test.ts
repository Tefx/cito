import { afterEach, describe, expect, it } from "vitest";
import { execFile, execFileSync, spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, existsSync, utimesSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createServer, type Server } from "node:http";
import { once } from "node:events";
import { createConnection, createServer as createUnixServer, type Server as UnixServer } from "node:net";
const root = resolve(import.meta.dirname, "../..");
const dirs: string[] = [];
const children: ChildProcessWithoutNullStreams[] = [];
const servers: Server[] = [];
const unixServers: UnixServer[] = [];
function fixture() {
  const home = mkdtempSync("/tmp/cito-plugin-test-"); dirs.push(home);
  const state = join(home, "state"); mkdirSync(state);
  const opener = join(home,"open");
  writeFileSync(opener, '#!/bin/sh\nprintf \'%s\\n\' "$@" >> "$OPEN_ARGS"\n', {mode:0o700});
  // Port 0 prevents hook acknowledgements from reaching a real host daemon.
  const env = { ...process.env, HOME: home, CITO_STATE_DIR: state, CITO_CONFIG_PATH: join(state, "config.json"), CITO_PI_HOME: join(home, ".pi/agent"), CITO_CLAUDE_HOME: join(home, ".claude"), CITO_CODEX_HOME: join(home, ".codex"), CITO_DAEMON_PORT: "0", CITO_ENDPOINT_PID: String(process.pid), CITO_OPEN_BIN: opener, OPEN_ARGS: join(home,"open-args"), CLAUDE_SESSION_ID: "", CODEX_THREAD_ID: "" };
  return { home, state, env, request: { state_dir: state, env } };
}
function binary(name: string) {
  return process.env.CITO_NATIVE_PLUGIN_TEST_BIN_DIR ? join(process.env.CITO_NATIVE_PLUGIN_TEST_BIN_DIR, name)
    : join(root, "plugins", name === "cito-claude-hook" ? "claude" : name.replace("cito-plugin-", ""), "bin", name);
}
function call(name: string, request: unknown, env: NodeJS.ProcessEnv): Promise<Record<string, any>> {
  return new Promise((resolve, reject) => {
    const child = execFile(binary(name), [], { env, timeout: 10_000 }, (error, stdout, stderr) => {
      if (error) { reject(new Error(`${error.message}: ${stderr}`)); return; }
      try { resolve(JSON.parse(stdout)); } catch { reject(new Error(`Invalid plugin JSON: ${stdout} ${stderr}`)); }
    });
    child.stdin!.end(JSON.stringify(request) + "\n");
  });
}
function persistent(name: string, env: NodeJS.ProcessEnv) {
  return monitor(spawn(binary(name), [], { env, stdio: "pipe" }));
}
function monitor(child: ChildProcessWithoutNullStreams) {
  children.push(child);
  const frames: any[] = []; let buffer = ""; let stderr = "";
  child.stderr.on("data", chunk => { stderr += chunk; });
  child.stdout.on("data", chunk => { buffer += chunk; let end; while ((end = buffer.indexOf("\n")) >= 0) { frames.push(JSON.parse(buffer.slice(0, end))); buffer = buffer.slice(end + 1); } });
  function send(request: unknown) { child.stdin.write(JSON.stringify(request) + "\n"); }
  async function frame(op: string) {
    const start = Date.now();
    while (Date.now() - start < 3000) { const found = frames.find(x => x.op === op); if (found) return found; await new Promise(resolve => setTimeout(resolve, 5)); }
    throw new Error(`Missing ${op}: ${JSON.stringify(frames)}; stderr=${stderr}; exit=${child.exitCode}`);
  }
  return { child, frames, send, frame };
}
function socketRequest(path: string, request: unknown): Promise<Record<string,any>> {
  return new Promise((resolve,reject)=>{
    const socket=createConnection(path); let data=""; socket.setEncoding("utf8"); socket.setTimeout(2000,()=>socket.destroy(new Error("Socket timed out")));
    socket.on("connect",()=>socket.write(JSON.stringify(request)+"\n")); socket.on("error",reject);
    socket.on("data",chunk=>{data+=chunk;if(data.includes("\n")){try{resolve(JSON.parse(data.slice(0,data.indexOf("\n"))));}catch(e){reject(e);}socket.destroy();}});
    socket.on("close",()=>{if(!data.includes("\n"))reject(new Error("Socket closed before newline"));});
  });
}
async function processFixture(f: ReturnType<typeof fixture>, harness: string, recognized = true, cwd = "/repo", entryOverride?: string) {
  const entry = harness === "pi" ? "node_modules/@example/pi-coding-agent/dist/cli.js"
    : harness === "codex" ? "node_modules/@example/codex/bin/codex.js" : "node_modules/@example/claude-code/cli.js";
  const script = join(f.home, recognized ? (entryOverride ?? entry) : "unrelated.js"); mkdirSync(dirname(script), { recursive: true });
  writeFileSync(script, `// purpose: isolated live process and socket for native cached-caller regressions.
// usage: node <fixture script>, controlled with stdin JSON lines.
// effects: creates only a temporary state-directory Unix socket.
// requires: Node.js and a temporary HOME/CITO_STATE_DIR.
const net = require("node:net"), fs = require("node:fs"), path = require("node:path"), readline = require("node:readline");
const harness = process.env.FIXTURE_HARNESS, state = process.env.CITO_STATE_DIR;
const socket = path.join(state, "sockets", harness + "-pid-" + process.pid + ".sock");
fs.mkdirSync(path.dirname(socket), { recursive: true });
let open = true;
const server = net.createServer(stream => { let data = ""; stream.on("data", chunk => { data += chunk; if (!data.includes("\\n")) return;
  stream.end(JSON.stringify(open ? { ok: true, session_id: "session-a", cwd: process.env.FIXTURE_CWD, session_name: "Fixture", endpoint_pid: process.pid } : { ok: false, error: "SESSION_NOT_OPEN" }) + "\\n"); }); });
server.listen(socket, () => console.log(JSON.stringify({ op: "ready" })));
readline.createInterface({ input: process.stdin }).on("line", line => { const req = JSON.parse(line);
  if (req.op === "offline") server.close(() => console.log(JSON.stringify({ op: "offline" })));
  if (req.op === "closed") { open = false; console.log(JSON.stringify({ op: "closed" })); }
});
setInterval(() => {}, 1000);
`);
  if (harness === "codex") {
    mkdirSync(f.env.CITO_CODEX_HOME, { recursive: true });
    execFileSync("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE threads(id TEXT, cwd TEXT, archived INTEGER, updated_at INTEGER)'); c.execute('INSERT INTO threads VALUES (?,?,?,?)', ('session-a','/repo',0,1)); c.commit(); c.close()", join(f.env.CITO_CODEX_HOME, "state_5.sqlite")]);
  }
  const client = monitor(spawn(process.execPath, [script], { env: { ...f.env, FIXTURE_HARNESS: harness, FIXTURE_CWD: cwd }, stdio: "pipe" }));
  await client.frame("ready");
  const endpoint_pid = client.child.pid!;
  const healthy = await call(`cito-plugin-${harness}`, { ...f.request, op: "resolve_caller", endpoint_pid, cwd: "/repo", require_open: true,
    ...(harness === "codex" ? { meta: { threadId: "session-a" } } : {}) }, f.env);
  expect(healthy.ok, JSON.stringify(healthy)).toBe(true);
  expect(healthy.caller.process_identity.command).toContain(script);
  return { client, endpoint_pid, cached: healthy.caller, async offline() {
    client.send({ op: "offline" }); await client.frame("offline");
    if (harness === "codex") rmSync(join(f.env.CITO_CODEX_HOME, "state_5.sqlite"));
  } };
}
afterEach(async () => {
  for (const child of children.splice(0)) if (child.exitCode == null && child.signalCode == null) { child.kill("SIGKILL"); await once(child, "exit").catch(() => {}); }
  for (const server of servers.splice(0)) { server.closeAllConnections(); await new Promise<void>(r => server.close(() => r())); }
  for (const server of unixServers.splice(0)) await new Promise<void>(resolve => server.close(() => resolve()));
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});
describe("native plugin Contract 1", () => {
  it("uses Pi package mode, removes legacy integrations, preserves unrelated settings and dry-run", async () => {
    const f = fixture(); const pi = join(f.home, ".pi/agent"); mkdirSync(join(pi, "extensions"), { recursive: true }); mkdirSync(join(pi, "mcp"));
    writeFileSync(join(pi, "settings.json"), JSON.stringify({ theme: "dark", packages: ["npm:@tefx/cito", "npm:other"] }));
    writeFileSync(join(pi, "extensions/cito-bridge.ts"), "legacy");
    const mcpPaths = [join(pi, "mcp.json"), join(pi, "mcp/global.mcp.json")];
    const stale = { mcpServers: { cito: { command: "cito-mcp" }, other: { command: "other" } }, userSetting: true };
    for (const path of mcpPaths) writeFileSync(path, JSON.stringify(stale));
    const before = readFileSync(join(pi, "settings.json"), "utf8");
    expect((await call("cito-plugin-pi", { ...f.request, op: "setup_apply", dry_run: true }, f.env)).ok).toBe(true);
    expect(readFileSync(join(pi, "settings.json"), "utf8")).toBe(before);
    for (const path of mcpPaths) expect(JSON.parse(readFileSync(path, "utf8"))).toEqual(stale);
    await call("cito-plugin-pi", { ...f.request, op: "setup_apply" }, f.env);
    expect(JSON.parse(readFileSync(join(pi, "settings.json"), "utf8"))).toEqual({ theme: "dark", packages: ["npm:other", join(f.state, "plugins/pi")] });
    expect(existsSync(join(pi, "extensions/cito-bridge.ts"))).toBe(false);
    for (const path of mcpPaths) expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ mcpServers: { other: { command: "other" } }, userSetting: true });
    expect((await call("cito-plugin-pi", { ...f.request, op: "setup_inspect" }, f.env)).configured).toBe(true);
    const again = await call("cito-plugin-pi", { ...f.request, op: "setup_apply" }, f.env);
    expect(again.actions.every((a: any) => a.status === "unchanged")).toBe(true);
    for (const path of mcpPaths) {
      writeFileSync(path, JSON.stringify(stale));
      const inspect = await call("cito-plugin-pi", { ...f.request, op: "setup_inspect" }, f.env);
      expect(inspect.configured).toBe(false); expect(inspect.paths).toContain(path);
      await call("cito-plugin-pi", { ...f.request, op: "setup_apply" }, f.env);
      expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ mcpServers: { other: { command: "other" } }, userSetting: true });
    }
    for (const path of mcpPaths) writeFileSync(path, JSON.stringify(stale));
    await call("cito-plugin-pi", { ...f.request, op: "setup_remove" }, f.env);
    expect(JSON.parse(readFileSync(join(pi, "settings.json"), "utf8"))).toEqual({ theme: "dark", packages: ["npm:other"] });
    for (const path of mcpPaths) expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ mcpServers: { other: { command: "other" } }, userSetting: true });
  });
  it("preserves multiline TOML while replacing and removing the complete MCP subtree", async () => {
    const f = fixture(); const codex = join(f.home, ".codex"); mkdirSync(codex);
    const path = join(codex, "config.toml"); const prefix = `model = "default"\nitems = [\n  "one", # a comment\n  "two",\n]\nprompt = """\n[mcp_servers.cito.fake]\nThis is a multiline string.\n"""\nliteral = '''\n[still.not.a.table]\n'''\n`;
    writeFileSync(path, prefix + `[mcp_servers.cito]\ncommand = "cito-mcp"\nargs = [\n]\n[mcp_servers.cito.env]\nKEEP = "old"\n[mcp_servers.other]\ncommand = "other"\n`);
    expect((await call("cito-plugin-codex", { ...f.request, op: "setup_apply" }, f.env)).ok).toBe(true);
    const configured = readFileSync(path, "utf8"); expect(configured).toContain(prefix); expect(configured).toContain(join(f.state, "bin/cito-mcp"));
    await call("cito-plugin-codex", { ...f.request, op: "setup_remove" }, f.env);
    const removed = readFileSync(path, "utf8"); expect(removed).toContain(prefix); expect(removed).not.toContain("KEEP ="); expect(removed).toContain("[mcp_servers.other]");
  });
  it.each([{ meta: { "openai/threadId": "thread-a" } }, { meta: { threadId: "thread-a" } }, { headers: { "x-codex-turn-metadata": '{"thread_id":"thread-a"}' } }, { headers: { "x-codex-turn-metadata": { session_id: "thread-a" } } }])("accepts Codex thread metadata variant %j", async metadata => {
    const f = fixture(); const r = await call("cito-plugin-codex", { ...f.request, ...metadata, op: "resolve_caller", cwd: "/repo" }, f.env);
    expect(r.ok).toBe(true); expect(r.caller.session_id).toBe("thread-a");
    expect((await call("cito-plugin-codex", { ...f.request, op: "resolve_caller", http_client_pid: 1234 }, f.env)).error_code).toBe("BINDING_REQUIRED");
  });
  it("reads schema-varying Codex thread metadata, rejects archived live callers and supports offline inspection", async () => {
    const f = fixture(); const dir = join(f.home, ".codex"); mkdirSync(dir); const database = join(dir, "state_5.sqlite");
    execFileSync("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE threads (id TEXT, cwd TEXT, archived INTEGER, recency_at_ms INTEGER, title TEXT, git_branch TEXT, git_origin_url TEXT)'); c.executemany('INSERT INTO threads VALUES (?,?,?,?,?,?,?)', [('active','/repo',0,200,'Active work','main','git@example.com:acme/service.git'),('archived','/repo',1,300,'Archived work','old','https://example.com/acme/service.git')]); c.commit(); c.close()", database]);
    const r = await call("cito-plugin-codex", { ...f.request, op: "resolve_caller", cli_cmd: "listen", cwd: "/repo" }, f.env);
    expect(r.caller).toMatchObject({ session_id: "active", repo: "acme/service", branch: "main", title: "Active work" });
    expect((await call("cito-plugin-codex", { ...f.request, op: "resolve_caller", cwd: "/repo", meta: { threadId: "archived" }, require_open: true }, f.env)).error_code).toBe("SESSION_NOT_OPEN");
    expect((await call("cito-plugin-codex", { ...f.request, op: "resolve_caller", cwd: "/repo", meta: { threadId: "archived" }, require_open: false }, f.env)).caller.session_id).toBe("archived");
    expect((await call("cito-plugin-codex", { ...f.request, op: "probe", session_id: "archived" }, f.env)).status).toBe("closed_definitive");
  });
  it("runs the explicit Codex queue binary with literal thread and message arguments", async () => {
    const f = fixture(); const executable = join(f.home, "codex"); const output = join(f.home, "queue-args");
    writeFileSync(executable, '#!/bin/sh\nprintf \'%s\\n\' "$@" >> "$QUEUE_ARGS"\n', { mode: 0o700 });
    const r = await call("cito-plugin-codex", { ...f.request, op: "wake", session_id: "thread-a", text: "a message with spaces", env: { ...f.env, CITO_CODEX_BIN: executable, QUEUE_ARGS: output } }, f.env);
    expect(r.outcome).toBe("delivered"); expect(readFileSync(output, "utf8").split("\n")).toEqual(["queue", "--resume", "--thread", "thread-a", "--message", "a message with spaces", ""]);
    expect(readFileSync(f.env.OPEN_ARGS,"utf8")).toBe("-g\ncodex://threads/wake/thread-a\n");
    const recovered = await call("cito-plugin-codex", {...f.request,op:"wake",session_id:"thread-a",text:"frozen",queue_written:true},f.env);
    expect(recovered).toMatchObject({outcome:"delivered_unconfirmed",queue_written:true,open_confirmed:true});
    expect(readFileSync(output,"utf8").split("\n")).toHaveLength(7);
    writeFileSync(f.env.CITO_OPEN_BIN,"#!/bin/sh\nexit 1\n",{mode:0o700});
    expect(await call("cito-plugin-codex",{...f.request,op:"wake",session_id:"thread-a",text:"frozen",queue_written:true},f.env)).toMatchObject({outcome:"delivered_unconfirmed",queue_written:true,open_confirmed:false});
    expect(readFileSync(output,"utf8").split("\n")).toHaveLength(7);
  });
  it("uses symmetric Claude hook matching and absolute plugin-owned paths", async () => {
    const f = fixture(); const dir = join(f.home, ".claude"); mkdirSync(dir);
    const path = join(dir, "settings.json"); writeFileSync(path, JSON.stringify({ theme: "dark", hooks: { Stop: [{ hooks: [{ type: "command", command: "/old/cito-claude-hook" }, { type: "command", command: "keep" }] }] } }));
    await call("cito-plugin-claude", { ...f.request, op: "setup_apply" }, f.env);
    const v = JSON.parse(readFileSync(path, "utf8")); expect(v.mcpServers.cito.command).toBe(join(f.state, "bin/cito-mcp"));
    expect(v.hooks.Stop.flatMap((g: any) => g.hooks).map((h: any) => h.command)).toEqual(["keep", join(f.state, "plugins/claude/bin/cito-claude-hook")]);
    await call("cito-plugin-claude", { ...f.request, op: "setup_remove" }, f.env);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ theme: "dark", hooks: { Stop: [{ hooks: [{ type: "command", command: "keep" }] }] } });
  });
  it("serializes concurrent Claude append/drain, isolates torn records and respects wake=false on Stop", async () => {
    const f = fixture(); const q = join(f.state, "claude/wakes"); mkdirSync(q, { recursive: true }); writeFileSync(join(q, "session-a.jsonl"), '{"batch_id":"torn"');
    await Promise.all(Array.from({ length: 12 }, (_, i) => call("cito-plugin-claude", { ...f.request, op: "wake", session_id: "session-a", batch_id: `batch-${i}`, text: `message-${i}`, wake: false }, f.env)));
    const out = await call("cito-claude-hook", { hook_event_name: "Stop", session_id: "session-a", cwd: "/repo" }, f.env);
    expect(out.decision).toBeUndefined(); expect(out.hookSpecificOutput.hookEventName).toBe("Stop");
    for (let i = 0; i < 12; i++) expect(out.hookSpecificOutput.additionalContext).toContain(`message-${i}`);
    await call("cito-plugin-claude", { ...f.request, op: "wake", session_id: "session-a", batch_id: "wake-true", text: "continue", wake: true }, f.env);
    expect(await call("cito-claude-hook", { hook_event_name: "Stop", session_id: "session-a", cwd: "/repo" }, f.env)).toMatchObject({ decision: "block", reason: "continue" });
    expect(await call("cito-claude-hook", { hook_event_name: "Stop", session_id: "session-a", cwd: "/repo" }, f.env)).toEqual({});
  });
  it("resolves a unique workspace hook session and drains legacy aliases under their own queue locks", async () => {
    const f = fixture(); const sid = "11111111-1111-4111-8111-111111111111";
    const project = join(f.home, ".claude/projects/-repo"); mkdirSync(project, { recursive: true }); writeFileSync(join(project, `${sid}.jsonl`), "{}\n");
    const aliases = join(f.state, "claude/aliases"); mkdirSync(aliases, { recursive: true }); writeFileSync(join(aliases, sid), "claude-7777\n");
    await call("cito-plugin-claude", { ...f.request, op: "update_badge", session_id: "claude-7777", badge: "🔔 cito (1)" }, f.env);
    await call("cito-plugin-claude", { ...f.request, op: "wake", session_id: "claude-7777", batch_id: "legacy", text: "legacy wake", wake: true }, f.env);
    const out = await call("cito-claude-hook", { hook_event_name: "Stop", cwd: "/repo" }, f.env);
    expect(out).toEqual({ decision: "block", reason: "legacy wake" });
    expect(readFileSync(join(f.state, "claude/badges/claude-7777"), "utf8")).toBe("🔔 cito\n");
  });
  it("keeps every wake exactly once when hooks drain concurrently with appenders", async () => {
    const f = fixture();
    const appenders = Array.from({ length: 24 }, (_, i) => call("cito-plugin-claude", { ...f.request, op: "wake", session_id: "session-race", batch_id: `race-${i}`, text: `token-${String(i).padStart(3, "0")}`, wake: false }, f.env));
    const hooks = Array.from({ length: 6 }, () => call("cito-claude-hook", { hook_event_name: "UserPromptSubmit", session_id: "session-race", cwd: "/repo" }, f.env));
    const results = await Promise.all([...appenders, ...hooks]);
    results.push(await call("cito-claude-hook", { hook_event_name: "Stop", session_id: "session-race", cwd: "/repo" }, f.env));
    const tokens = results.flatMap(r => String(r.hookSpecificOutput?.additionalContext ?? "").match(/token-\d{3}/g) ?? []).sort();
    expect(tokens).toEqual(Array.from({ length: 24 }, (_, i) => `token-${String(i).padStart(3, "0")}`));
  });
  it("terminates the Claude sidecar on parent EOF while its socket is listening", async () => {
    const f = fixture(); const p = persistent("cito-plugin-claude", { ...f.env, CLAUDE_SESSION_ID: "session-sidecar" });
    p.send({ ...f.request, op: "stdio_sidecar", endpoint_pid: process.pid, cwd: "/repo", env: { ...f.env, CLAUDE_SESSION_ID: "session-sidecar" } });
    expect(await p.frame("ready")).toMatchObject({ capabilities_patch: { experimental: { "claude/channel": {} } } });
    const start = performance.now(); p.child.stdin.end(); await once(p.child, "exit"); expect(performance.now() - start).toBeLessThan(100);
  });
  it("rejects ambiguous Claude transcripts and resolves endpoint-specific recorded identity", async () => {
    const f = fixture(); const p = await processFixture(f, "claude"); await p.offline();
    const project = join(f.home, ".claude/projects/-repo"); mkdirSync(project, { recursive: true });
    const a = "11111111-1111-4111-8111-111111111111", b = "22222222-2222-4222-8222-222222222222";
    for (const id of [a, b]) writeFileSync(join(project, `${id}.jsonl`), "{}");
    expect((await call("cito-plugin-claude", { ...f.request, op: "resolve_caller", cwd: "/repo", endpoint_pid: p.endpoint_pid }, f.env)).error_code).toBe("BINDING_REQUIRED");
    await call("cito-claude-hook", { hook_event_name: "SessionStart", session_id: a, cwd: "/repo" }, { ...f.env, CITO_ENDPOINT_PID: String(p.endpoint_pid) });
    const resolved = await call("cito-plugin-claude", { ...f.request, op: "resolve_caller", cwd: "/repo", endpoint_pid: p.endpoint_pid }, f.env);
    expect(resolved.caller.session_id).toBe(a);
    expect((await call("cito-plugin-claude", { ...f.request, op: "resolve_caller", cwd: "/repo", endpoint_pid: p.endpoint_pid, meta: { expected_harness: "claude", expected_session_id: b } }, f.env)).error_code).toBe("INVALID_INPUT");
  });
});
describe("native adapter baseline compatibility", () => {
  it("validates explicit PID claims, honors nested roots, and walks only the requested ancestry",async()=>{
    const f=fixture(), p=await processFixture(f,"pi");
    for(const meta of [{pi_pid:p.endpoint_pid},{caller_pid:p.endpoint_pid},{gateway:{parent_pid:p.endpoint_pid}}]) {
      expect((await call("cito-plugin-pi",{...f.request,op:"resolve_caller",meta},f.env)).caller).toMatchObject({endpoint_pid:p.endpoint_pid,session_id:"session-a",repo:null,branch:null});
    }
    for(const pi_pid of [0,-1,0.5,Number.MAX_SAFE_INTEGER+1,"12"]) expect((await call("cito-plugin-pi",{...f.request,op:"resolve_caller",meta:{pi_pid}},f.env)).ok).toBe(false);
    expect((await call("cito-plugin-pi",{...f.request,op:"resolve_caller",meta:{pi_pid:p.endpoint_pid,caller_pid:1}},f.env)).error_code).toBe("INVALID_INPUT");
    expect((await call("cito-plugin-pi",{...f.request,op:"resolve_caller",meta:{nested:{parent_pid:2147483647}}},f.env)).ok).toBe(false);
    await p.offline();
    const path=join(f.state,`sockets/pi-pid-${process.pid}.sock`);
    const server=createUnixServer(s=>s.once("data",()=>s.end(JSON.stringify({ok:true,session_id:"ancestor",cwd:f.home})+"\n")));unixServers.push(server);
    await new Promise<void>(r=>server.listen(path,r));
    const ancestry=await call("cito-plugin-pi",{...f.request,op:"resolve_caller",http_client_pid:p.endpoint_pid},f.env);
    expect(ancestry.caller).toMatchObject({endpoint_pid:process.pid,session_id:"ancestor"});
  },15_000);
  it("rejects incomplete newline frames rather than accepting a partial identity",async()=>{
    const f=fixture();mkdirSync(join(f.state,"sockets")); const path=join(f.state,"sockets/pi-pid-43210.sock");
    const server=createUnixServer(s=>s.once("data",()=>s.end('{"ok":true,"session_id":"bad","cwd":"/repo"}')));unixServers.push(server);await new Promise<void>(r=>server.listen(path,r));
    expect((await call("cito-plugin-pi",{...f.request,op:"resolve_caller",meta:{pi_pid:43210}},f.env)).ok).toBe(false);
  });
  it("rejects Codex metadata plus explicit bridge claims before any state access",async()=>{
    const f=fixture();
    for(const meta of [{codexThreadId:"a","openai/threadId":"b"},{codexThreadId:"a",pi_pid:123},{endpoint_pid:1,caller_pid:2},{codexThreadId:"a",caller_pid:123},{codexThreadId:"a",endpoint_pid:123}]) {
      expect((await call("cito-plugin-codex",{...f.request,op:"resolve_caller",meta},f.env)).error_code).toBe("INVALID_INPUT");
    }
    expect((await call("cito-plugin-codex",{...f.request,op:"resolve_caller",meta:{expected_harness:"codex",expected_session_id:"a",endpoint_pid:123}},f.env)).ok).toBe(false);
    expect(existsSync(join(f.home,".codex/state_5.sqlite"))).toBe(false);
  });
  it("discovers bounded nonarchived user threads through a matching isolated desktop process",async()=>{
    const f=fixture(),p=await processFixture(f,"codex",true,"/repo","Codex.app/Contents/MacOS/Codex");
    execFileSync("python3",["-c","import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('ALTER TABLE threads ADD COLUMN thread_source TEXT'); c.executemany('INSERT INTO threads VALUES (?,?,?,?,?)', [('thread-'+str(i),'/repo',0,i*1000,'user') for i in range(1,11)]+[('subagent','/repo',0,999999,'subagent'),('archived','/repo',1,999999,'user')]); c.commit(); c.close()",join(f.env.CITO_CODEX_HOME,"state_5.sqlite")]);
    const result=await call("cito-plugin-codex",{...f.request,op:"discover_sessions",endpoint_pid:p.endpoint_pid},f.env);
    expect(result.sessions).toHaveLength(8);expect(result.sessions[0].session_id).toBe("thread-10");expect(result.sessions.some((r:any)=>r.session_id==="subagent"||r.session_id==="archived")).toBe(false);
    expect((await call("cito-plugin-codex",{...f.request,op:"resolve_caller",http_transport:true},f.env)).ok).toBe(false);
  });
  it("normalizes case-preserving Git origins and reads unborn HEAD while clearing inherited Git overrides",async()=>{
    const f=fixture(),p=await processFixture(f,"pi",true,f.home);
    execFileSync("git",["init","-b","feature",f.home],{stdio:"ignore"});execFileSync("git",["-C",f.home,"remote","add","origin","git@example.com:Owner/Repo.git"]);
    for(const origin of ["git@example.com:Owner/Repo.git","https://example.com/Owner/Repo.git","ssh://git@example.com/Owner/Repo.git","https://example.com/Owner/Repo.git/"]) {
      execFileSync("git",["-C",f.home,"remote","set-url","origin",origin]);
      expect((await call("cito-plugin-pi",{...f.request,op:"resolve_caller",endpoint_pid:p.endpoint_pid},{...f.env,GIT_DIR:"/missing"})).caller).toMatchObject({repo:"Owner/Repo",branch:"feature"});
    }
    expect((await call("cito-plugin-codex",{...f.request,op:"resolve_caller",cwd:f.home,meta:{codexThreadId:"thread-a"}},f.env)).caller).toMatchObject({repo:"Owner/Repo",branch:"feature"});
  },15_000);
  it("preserves Claude first-turn aliases until clear, deduplicates delivery, and cleans owned badges/sockets on EOF",async()=>{
    const f=fixture(),p=persistent("cito-plugin-claude",f.env),sid="11111111-1111-4111-8111-111111111111",next="22222222-2222-4222-8222-222222222222";
    const path=join(f.state,`sockets/claude-pid-${process.pid}.sock`),project=join(f.home,".claude/projects/-repo");
    p.send({...f.request,op:"stdio_sidecar",endpoint_pid:process.pid,cwd:"/repo"});await p.frame("ready");
    const fallback=`claude-${process.pid}`;expect((await socketRequest(path,{op:"identify"})).session_id).toBe(fallback);
    mkdirSync(project,{recursive:true});writeFileSync(join(project,sid+".jsonl"),JSON.stringify({type:"custom-title",customTitle:"Human title"})+"\n");
    expect(await socketRequest(path,{op:"identify"})).toMatchObject({session_id:fallback,session_name:"Human title"});
    expect(readFileSync(join(f.state,"claude/aliases",sid),"utf8").trim()).toBe(fallback);
    expect(await socketRequest(path,{op:"status",session_id:fallback,badge:"badge"})).toMatchObject({ok:true});
    const frame={op:"deliver",session_id:fallback,batch_id:"batch",content:"literal <card>",wake:true};
    expect(await socketRequest(path,frame)).toMatchObject({ok:true,deduplicated:false});expect(await socketRequest(path,frame)).toMatchObject({deduplicated:true});
    expect(readFileSync(join(f.state,"claude/wakes",fallback+".jsonl"),"utf8").trim().split("\n")).toHaveLength(1);
    writeFileSync(join(project,next+".jsonl"),"{}\n");utimesSync(join(project,next+".jsonl"),Date.now()/1000+2,Date.now()/1000+2);
    expect((await socketRequest(path,{op:"identify"})).session_id).toBe(next);expect(await socketRequest(path,frame)).toMatchObject({ok:false,error:"SESSION_MISMATCH"});
    p.child.stdin.end();await once(p.child,"exit");expect(existsSync(path)).toBe(false);expect(existsSync(join(f.state,"claude/badges",fallback))).toBe(false);
  });
  it("persists and acknowledges delivery even when channel stdout is backpressured",async()=>{
    const f=fixture(),p=persistent("cito-plugin-claude",f.env),path=join(f.state,`sockets/claude-pid-${process.pid}.sock`);
    p.send({...f.request,op:"stdio_sidecar",endpoint_pid:process.pid,cwd:"/repo",env:{...f.env,CLAUDE_SESSION_ID:"session-a"}});await p.frame("ready");await socketRequest(path,{op:"identify"});
    p.child.stdout.pause();const content="x".repeat(1024*1024),start=performance.now();
    expect(await socketRequest(path,{op:"deliver",session_id:"session-a",batch_id:"backpressure",content,wake:true})).toMatchObject({ok:true,deduplicated:false});
    expect(performance.now()-start).toBeLessThan(300);
    expect(JSON.parse(readFileSync(join(f.state,"claude/wakes/session-a.jsonl"),"utf8")).content).toBe(content);
    p.child.stdout.resume();
  });
  it("lets the first hook drain a pending fallback queue before another identify and fences a later clear",async()=>{
    const f=fixture(),p=persistent("cito-plugin-claude",f.env),path=join(f.state,`sockets/claude-pid-${process.pid}.sock`),fallback=`claude-${process.pid}`;
    const sid="11111111-1111-4111-8111-111111111111",next="22222222-2222-4222-8222-222222222222",project=join(f.home,".claude/projects/-repo");
    p.send({...f.request,op:"stdio_sidecar",endpoint_pid:process.pid,cwd:"/repo"});await p.frame("ready");await socketRequest(path,{op:"identify"});
    await socketRequest(path,{op:"deliver",session_id:fallback,batch_id:"first",content:"first turn",wake:true});
    mkdirSync(project,{recursive:true});writeFileSync(join(project,sid+".jsonl"),"{}\n");
    expect(await call("cito-claude-hook",{hook_event_name:"SessionStart",session_id:sid,cwd:"/repo"},f.env)).toMatchObject({hookSpecificOutput:{additionalContext:"first turn"}});
    expect((await socketRequest(path,{op:"identify"})).session_id).toBe(fallback);
    writeFileSync(join(project,next+".jsonl"),"{}\n");utimesSync(join(project,next+".jsonl"),Date.now()/1000+2,Date.now()/1000+2);
    expect(await call("cito-claude-hook",{hook_event_name:"SessionStart",session_id:next,cwd:"/repo"},f.env)).toEqual({});
    expect(existsSync(join(f.state,"claude/aliases",next))).toBe(false);expect((await socketRequest(path,{op:"identify"})).session_id).toBe(next);
  });
  it("keeps a live Claude socket owner, reclaims abandoned files, and cleans badges on SIGTERM",async()=>{
    const f=fixture(),p=persistent("cito-plugin-claude",f.env),path=join(f.state,`sockets/claude-pid-${process.pid}.sock`);
    p.send({...f.request,op:"stdio_sidecar",endpoint_pid:process.pid,cwd:"/repo",env:{...f.env,CLAUDE_SESSION_ID:"session-a"}});await p.frame("ready");
    const collision=await new Promise<string>((resolve,reject)=>{const c=execFile(binary("cito-plugin-claude"),[],{env:f.env},(e,out,err)=>e?reject(e):resolve(out+err));c.stdin!.end(JSON.stringify({...f.request,op:"stdio_sidecar",endpoint_pid:process.pid,cwd:"/repo"})+"\n");});
    expect(collision).toMatch(/live.*owns/);expect((await socketRequest(path,{op:"ping"})).ok).toBe(true);
    await socketRequest(path,{op:"status",session_id:"session-a",badge:"badge"});p.child.kill("SIGTERM");await once(p.child,"exit");
    expect(existsSync(path)).toBe(false);expect(existsSync(join(f.state,"claude/badges/session-a"))).toBe(false);
    writeFileSync(path,"abandoned");const recovered=persistent("cito-plugin-claude",f.env);recovered.send({...f.request,op:"stdio_sidecar",endpoint_pid:process.pid,cwd:"/repo"});await recovered.frame("ready");expect((await socketRequest(path,{op:"ping"})).ok).toBe(true);
  });
  it.each(["",JSON.stringify({hook_event_name:"Stop",cwd:"/repo"})])("treats an empty/unbound hook as a silent no-op: %s",async input=>{
    const f=fixture();const result=await new Promise<{stdout:string,stderr:string}>((resolve,reject)=>{const c=execFile(binary("cito-claude-hook"),[],{env:f.env},(e,stdout,stderr)=>e?reject(e):resolve({stdout,stderr}));c.stdin!.end(input);});
    expect(result).toEqual({stdout:"{}\n",stderr:""});
  });
});
describe("native cached caller identity (F53)", () => {
  it.each(["pi", "codex", "claude"])("%s reuses only a verified offline cache for the same live process", async harness => {
    const f = fixture(); const p = await processFixture(f, harness); await p.offline();
    const request = { ...f.request, op: "resolve_caller", endpoint_pid: p.endpoint_pid, cwd: "/repo", require_open: false, cached_caller: p.cached };
    const accepted = await call(`cito-plugin-${harness}`, { ...request, meta: { expected_harness: harness, expected_session_id: "session-a" } }, f.env);
    expect(accepted.ok, JSON.stringify(accepted)).toBe(true); expect(accepted.caller).toEqual(p.cached);
    for (const meta of [{ expected_harness: "another-harness" }, { expected_session_id: "session-other" }]) {
      expect((await call(`cito-plugin-${harness}`, { ...request, meta }, f.env)).error_code).toBe("INVALID_INPUT");
    }
    expect((await call(`cito-plugin-${harness}`, { ...request, explicit_session_id: "session-other" }, f.env)).error_code).toBe("INVALID_INPUT");
    expect((await call(`cito-plugin-${harness}`, { ...request, cached_caller: { ...p.cached, harness: "another-harness" } }, f.env)).error_code).toBe("INVALID_INPUT");
    expect((await call(`cito-plugin-${harness}`, { ...request, meta: { expected_session_id: "session-a" }, cached_caller: { ...p.cached, session_id: "session-other" } }, f.env)).error_code).toBe("INVALID_INPUT");
    for (const process_identity of [{ ...p.cached.process_identity, command: p.cached.process_identity.command + " --session-other" }, { ...p.cached.process_identity, started_at: "another invocation" }, {}]) {
      expect((await call(`cito-plugin-${harness}`, { ...request, cached_caller: { ...p.cached, process_identity } }, f.env)).error_code).toBe("BINDING_REQUIRED");
    }
    expect((await call(`cito-plugin-${harness}`, { ...request, require_open: true }, f.env)).ok).toBe(false);
    p.client.child.kill("SIGKILL"); await once(p.client.child, "exit");
    expect((await call(`cito-plugin-${harness}`, request, f.env)).error_code).toBe("BINDING_REQUIRED");
  }, 15_000);
  it.each(["pi", "codex", "claude"])("%s checks the real process command even when the cached fingerprint matches", async harness => {
    const f = fixture(); const p = await processFixture(f, harness, false); await p.offline();
    const r = await call(`cito-plugin-${harness}`, { ...f.request, op: "resolve_caller", endpoint_pid: p.endpoint_pid, cwd: "/repo", require_open: false, cached_caller: p.cached }, f.env);
    expect(r.error_code).toBe("BINDING_REQUIRED"); expect(r.message).toContain("process command does not match");
    if (harness === "claude") {
      await call("cito-claude-hook", { hook_event_name: "SessionStart", session_id: "session-a", cwd: "/repo" }, { ...f.env, CITO_ENDPOINT_PID: String(p.endpoint_pid) });
      const diskBinding = await call("cito-plugin-claude", { ...f.request, op: "resolve_caller", endpoint_pid: p.endpoint_pid, cwd: "/repo", require_open: false }, f.env);
      expect(diskBinding.error_code).toBe("BINDING_REQUIRED"); expect(diskBinding.message).toContain("process command does not match");
    }
  }, 15_000);
  it.each(["pi", "claude"])("%s never substitutes a cache for a reachable bridge reporting no open session", async harness => {
    const f = fixture(); const p = await processFixture(f, harness); p.client.send({ op: "closed" }); await p.client.frame("closed");
    const r = await call(`cito-plugin-${harness}`, { ...f.request, op: "resolve_caller", endpoint_pid: p.endpoint_pid, cwd: "/repo", require_open: false, cached_caller: p.cached }, f.env);
    expect(r.ok).toBe(false);
    expect(r.error_code).toBe(harness === "pi" ? "PI_BRIDGE_UNAVAILABLE" : "SESSION_NOT_OPEN");
  }, 15_000);
});
describe("native Telegram Contract 2", () => {
  it("formats real bot instructions, chunks Unicode with every ID, isolates updates, waits for reply_ack and exits on EOF during network polling", async () => {
    const f = fixture(); let poll = 0; const sent: any[] = []; let hold = false; let polling = false;
    const server = createServer(async (req, res) => {
      const chunks: Buffer[] = []; for await (const chunk of req) chunks.push(Buffer.from(chunk)); const body = JSON.parse(Buffer.concat(chunks).toString("utf8")); const method = req.url!.split("/").at(-1);
      if (method === "getMe") { res.end(JSON.stringify({ ok: true, result: { username: "example_bot" } })); return; }
      if (method === "sendMessage") { sent.push(body); res.end(JSON.stringify({ ok: true, result: { message_id: 100 + sent.length } })); return; }
      if (method === "getUpdates") { polling = true; if (hold) return;
        res.end(JSON.stringify({ ok: true, result: poll++ === 0 ? [
          { update_id: 10, message: { from: { id: 9 }, chat: { id: 9, type: "group" }, text: "/start cito-abcdefghij" } },
          { update_id: 11, message: { from: { id: 9 }, chat: { id: 10, type: "private" }, text: "/start cito-abcdefghij" } },
          { update_id: 12, message: { from: { id: 9 }, chat: { id: 9, type: "private" }, text: "/start cito-abcdefghij" } },
          { update_id: 13, message: { chat: { id: 9, type: "private" }, text: "malformed" } },
          { update_id: 14, message: { from: { id: 9 }, chat: { id: 9, type: "private" }, text: "/start cito-abcdefghij" } },
          { update_id: 15, message: { from: { id: 9 }, chat: { id: 9, type: "private" }, text: "ship", reply_to_message: { message_id: 101 } } },
        ] : [] })); return; }
      res.end(JSON.stringify({ ok: true, result: true }));
    }); servers.push(server); await new Promise<void>(r => server.listen(0, "127.0.0.1", r)); const address = server.address() as { port: number };
    const config = { botToken: "test", apiBaseUrl: `http://127.0.0.1:${address.port}` }; const p = persistent("cito-plugin-telegram", f.env);
    p.send({ op: "format_pair", req_id: "pair", code: "cito-abcdefghij", hub_config: config });
    expect(await p.frame("rpc_result")).toMatchObject({ ok: true, instruction: 'Send "/start cito-abcdefghij" to @example_bot', bot_username: "example_bot" });
    p.send({ op: "configure", hub_config: config, bindings: [], cursor: "10" });
    const pair = await p.frame("pair_redeem"); expect(pair).toMatchObject({ external_chat_id: "9", external_user_id: "9", is_private_chat: true });
    expect(p.frames.filter(x => x.op === "pair_redeem")).toHaveLength(1);
    const reply = await p.frame("inbound_reply"); expect(reply.update_id).toBe("15");
    expect(p.frames.filter(x => x.op === "cursor_save").every(x => Number(x.cursor) <= 12)).toBe(true);
    p.send({ op: "pair_ack", code: "cito-abcdefghij", ok: true, owner_user: "alice" });
    await new Promise(r => setTimeout(r, 20)); expect(p.frames.filter(x => x.op === "cursor_save").every(x => Number(x.cursor) <= 15)).toBe(true);
    hold = true; polling = false; p.send({ op: "reply_ack", update_id: "15", ok: true, target_display: "alice-agent" });
    for (let i = 0; i < 500 && !p.frames.some(x => x.op === "cursor_save" && x.cursor === "16"); i++) await new Promise(r => setTimeout(r, 5));
    expect(p.frames.some(x => x.op === "cursor_save" && x.cursor === "16")).toBe(true);
    p.send({ op: "deliver", delivery_id: "large", config: { chatId: "9" }, signal: { from: { title: "work" }, message: "😀".repeat(9000) } });
    const delivered = await p.frame("delivered"); expect(delivered.ok, JSON.stringify(delivered)).toBe(true); expect(delivered.external_msg_ids).toHaveLength(3); expect(delivered.external_msg_id).toBe(delivered.external_msg_ids.at(-1));
    expect(sent.filter(x => x.text.includes("😀")).every(x => [...x.text].length <= 4096)).toBe(true);
    for (let i = 0; i < 500 && !polling; i++) await new Promise(r => setTimeout(r, 5)); expect(polling).toBe(true);
    const start = performance.now(); p.child.stdin.end(); await once(p.child, "exit"); expect(performance.now() - start).toBeLessThan(100);
  });
});
