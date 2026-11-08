import { afterEach, describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { createConnection } from "node:net";
import { createServer, type Server } from "node:http";
import citoBridge, { type BridgeAPI, type BridgeContext, type BridgeCommandOptions } from "../../plugins/pi/cito-bridge.js";
const cleanups: (() => void | Promise<void>)[] = [];
afterEach(async () => { for (const f of cleanups.splice(0).reverse()) await f(); });
function fixture(register?: BridgeAPI["registerMcpServer"]) {
  const home = mkdtempSync(join(tmpdir(), "cito-pi-bridge-"));
  const old = process.env.CITO_STATE_DIR; const oldPort = process.env.CITO_DAEMON_PORT;
  process.env.CITO_STATE_DIR = home; process.env.CITO_DAEMON_PORT = "1";
  cleanups.push(() => { if (old === undefined) delete process.env.CITO_STATE_DIR; else process.env.CITO_STATE_DIR = old; if (oldPort === undefined) delete process.env.CITO_DAEMON_PORT; else process.env.CITO_DAEMON_PORT = oldPort; rmSync(home, { recursive: true, force: true }); });
  const handlers = new Map<string, (e: unknown, c: BridgeContext) => void | Promise<void>>();
  let registration: unknown; let command: BridgeCommandOptions | undefined;
  const pi: BridgeAPI = { on: (name, fn) => handlers.set(name, fn), registerMcpServer: register ?? ((name, config) => { registration = { name, config }; }), registerCommand: (_name, opts) => { command = opts; }, sendMessage: () => {} };
  const ctx: BridgeContext = { cwd: "/repo", sessionManager: { getSessionId: () => "session-a", getSessionName: () => "work" }, ui: { setStatus: () => {} } };
  citoBridge(pi); cleanups.push(() => handlers.get("session_shutdown")?.({}, ctx));
  return { home, handlers, ctx, registration, command: () => command! };
}
function request(path: string, req: object): Promise<any> {
  return new Promise((resolve, reject) => {
    const s = createConnection(path); let data = "";
    s.on("error", reject); s.setTimeout(1000, () => { s.destroy(); reject(new Error("bridge timeout")); });
    s.on("connect", () => s.write(JSON.stringify(req) + "\n"));
    s.on("data", chunk => { data += chunk; if (data.includes("\n")) { s.destroy(); resolve(JSON.parse(data)); } });
  });
}
describe("Pi package initialization", () => {
  it("retries deferred MCP registration at session_start and surfaces persistent errors", async () => {
    let attempts = 0;
    const registrations: unknown[] = [];
    const f = fixture((name, config) => {
      if (++attempts === 1) throw new Error("Registry is not ready");
      registrations.push({ name, config });
    });
    await f.handlers.get("session_start")!({}, f.ctx);
    expect(registrations).toEqual([{name:"cito",config:{command:join(f.home,"bin/cito-mcp"),args:[]}}]);
    await f.handlers.get("session_start")!({}, f.ctx);
    expect(attempts).toBe(2);
    await f.handlers.get("session_shutdown")!({}, f.ctx);
    const broken = fixture(() => { throw new Error("Registry failed"); });
    await expect(broken.handlers.get("session_start")!({}, broken.ctx)).rejects.toThrow("Registry failed");
  });
  it("registers the absolute MCP shim on initialization and reclaims a dead Unix socket", async () => {
    const f = fixture(); expect(f.registration).toEqual({ name: "cito", config: { command: join(f.home, "bin/cito-mcp"), args: [] } });
    const dir = join(f.home, "sockets"); mkdirSync(dir); const path = join(dir, `pi-pid-${process.pid}.sock`);
    execFileSync("python3", ["-c", "import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()", path]);
    await f.handlers.get("session_start")!({}, f.ctx);
    expect(await request(path, { op: "ping" })).toEqual({ ok: true });
    expect(await request(path, { op: "identify" })).toMatchObject({ session_id: "session-a", cwd: "/repo" });
  });
  it("reads daemonPort from config before the environment fallback", async () => {
    const f = fixture(); const calls: any[] = []; const server: Server = createServer(async (req, res) => {
      let body = ""; for await (const c of req) body += c; calls.push(JSON.parse(body));
      res.end(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { content: [{ type: "text", text: JSON.stringify({ ok: true, listener_id: "alice-agent", scope: "self" }) }] } }));
    }); await new Promise<void>(r => server.listen(0, "127.0.0.1", r));
    cleanups.push(() => new Promise<void>(r => { server.closeAllConnections(); server.close(() => r()); }));
    writeFileSync(join(f.home, "config.json"), JSON.stringify({ daemonPort: (server.address() as { port: number }).port }));
    await f.command().handler("on", f.ctx);
    expect(calls).toHaveLength(1); expect(calls[0].params.name).toBe("cito_listen"); expect(calls[0].params._meta.endpoint_pid).toBe(process.pid);
    process.env.CITO_DAEMON_PORT = String((server.address() as { port: number }).port);
    writeFileSync(join(f.home, "config.json"), JSON.stringify({ daemonPort: false }));
    await f.command().handler("on", f.ctx);
    expect(calls).toHaveLength(2);
  });
});
