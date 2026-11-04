import { afterAll } from "vitest";
import { mkdtemp, mkdir, rm, copyFile, chmod, writeFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { DatabaseSync } from "node:sqlite";
import bridge, { type BridgeAPI, type BridgeContext } from "../../src/pi-extension/cito-bridge.js";
import { ensureBannerCompiled } from "../../src/banner/index.js";
import { CitoService, type ServiceOptions } from "../../src/mcp/service.js";
import { loadConfig } from "../../src/shared/index.js";
import { startNats, type TestNats } from "../shared/nats-server.js";
import { unixServer } from "../adapters/support.js";

const exec = promisify(execFile);
let cacheRoot: string | undefined;
let binary: Promise<string> | undefined;
afterAll(async () => { if (cacheRoot) await rm(cacheRoot, { recursive: true, force: true }); });
async function bannerBinary() {
  if (!binary) binary = (async () => {
    cacheRoot = await mkdtemp("/tmp/cito-mcp-banner-");
    return ensureBannerCompiled(cacheRoot);
  })();
  return binary;
}
export async function waitFor(check: () => boolean | Promise<boolean>, timeout = 5000) {
  const deadline = performance.now() + timeout;
  while (performance.now() < deadline) { if (await check()) return; await delay(10); }
  throw new Error("Condition timed out");
}
export function payload<T = Record<string, unknown>>(result: { content: { text: string }[] }): T {
  return JSON.parse(result.content[0]!.text) as T;
}
export async function call(service: CitoService, name: string, args: unknown = {}, meta: Record<string, unknown> = {}) {
  const result = await service.call(name, args, meta);
  if (result.isError) throw new Error(result.content[0]!.text);
  return payload<Record<string, any>>(result); // JSON tool payloads are asserted by each behavior test.
}
export async function piBridge(stateDir: string) {
  const handlers = new Map<string, (event: { source?: string }, ctx: BridgeContext) => unknown>();
  const messages: { content: string; options: { triggerTurn: boolean; deliverAs: string } }[] = [];
  const badges: (string | undefined)[] = [];
  let session = "pi-session-1";
  const ctx: BridgeContext = { cwd: stateDir, isIdle: () => true,
    sessionManager: { getSessionId: () => session, getSessionName: () => "Pi receiver" },
    ui: { setStatus: (_key, value) => { badges.push(value); } } };
  const api: BridgeAPI = { on: (event, handler) => { handlers.set(event, handler); },
    sendMessage: (message, options) => { messages.push({ content: message.content, options }); } };
  const before = process.env.CITO_STATE_DIR;
  try { process.env.CITO_STATE_DIR = stateDir; bridge(api); }
  finally { if (before === undefined) delete process.env.CITO_STATE_DIR; else process.env.CITO_STATE_DIR = before; }
  await handlers.get("session_start")!({}, ctx);
  return { meta: { pi_pid: process.pid }, messages, badges,
    input(source = "interactive") { handlers.get("input")!({ source }, ctx); },
    switchSession(id: string) { session = id; handlers.get("session_tree")!({}, ctx); },
    stop: async () => { await handlers.get("session_shutdown")!({}, ctx); } };
}
export async function bannerPids(binaryPath: string): Promise<number[]> {
  return (await exec("/bin/ps", ["-axo", "pid=,command="])).stdout.split("\n").flatMap(line => {
    const match = line.trim().match(/^(\d+)\s+(.+)$/); return match?.[2] === binaryPath ? [Number(match[1])] : [];
  });
}
export async function fixture(options: { broker?: TestNats; user?: string; service?: ServiceOptions; env?: NodeJS.ProcessEnv; pi?: boolean; observeBanner?: boolean } = {}) {
  const root = await mkdtemp("/tmp/cito-mcp-");
  const stateDir = join(root, "state"), codexHome = join(root, "codex");
  await mkdir(join(stateDir, "bin"), { recursive: true }); await mkdir(codexHome);
  const broker = options.broker ?? await startNats();
  const env = { CITO_STATE_DIR: stateDir, CITO_CODEX_HOME: codexHome, CITO_NATS_URL: broker.url,
    CITO_USER: options.user ?? "alice", ...options.env };
  if (process.platform === "darwin") { await copyFile(await bannerBinary(), join(stateDir, "bin/cito-banner")); await chmod(join(stateDir, "bin/cito-banner"), 0o755); }
  const diagnostics: string[] = [];
  const config = loadConfig(env);
  const serviceOptions: ServiceOptions = { diagnostic: text => diagnostics.push(text),
    banner: { env: { CITO_BANNER_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "120" }, diagnostic: text => diagnostics.push(text) }, ...options.service };
  const service = await CitoService.create(config, serviceOptions);
  const pi = options.pi === false ? undefined : await piBridge(stateDir);
  let closed = false;
  let bannerExited: Promise<unknown> | undefined;
  const bannerDisplays: { pid: number; title: string; subtitle: string; message: string }[] = [];
  const result = { root, config, env, stateDir, codexHome, broker, service, serviceOptions, pi: pi!, diagnostics, bannerDisplays,
    async close() {
      if (closed) return; closed = true;
      await service.close(); await pi?.stop();
      const binaryPath = join(stateDir, "bin/cito-banner");
      for (const pid of await bannerPids(binaryPath)) { try { process.kill(pid, "SIGKILL"); } catch {} }
      await waitFor(async () => !(await bannerPids(binaryPath)).length);
      await bannerExited;
      if (!options.broker) await broker.stop();
      await rm(root, { recursive: true, force: true });
    } };
  try {
    if (options.observeBanner) {
      if (process.platform !== "darwin") throw new Error("Native HUD observation requires macOS");
      const child = spawn(join(stateDir, "bin/cito-banner"), [], { env: { ...process.env, ...env,
        CITO_BANNER_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "10000" }, stdio: ["ignore", "ignore", "pipe"] });
      bannerExited = new Promise(resolve => child.once("close", resolve));
      let pending = "";
      child.stderr!.setEncoding("utf8").on("data", (chunk: string) => {
        pending += chunk;
        let end: number;
        while ((end = pending.indexOf("\n")) >= 0) {
          const line = pending.slice(0, end); pending = pending.slice(end + 1);
          if (line.startsWith("cito-banner: display ")) bannerDisplays.push(JSON.parse(line.slice("cito-banner: display ".length)));
          else diagnostics.push(line);
        }
      });
      await waitFor(async () => { try { return (await stat(join(stateDir, "sockets/banner.sock"))).isSocket(); } catch { return false; } });
    }
    return result;
  } catch (error) { await result.close(); throw error; }
}

/** Desktop protocol/CLI fixture; adapters still use real framing, sockets, SQLite and execFile. */
export async function codexDesktop(f: Awaited<ReturnType<typeof fixture>>) {
  const thread = "11111111-2222-4333-8444-555555555555";
  const db = new DatabaseSync(join(f.codexHome, "state_5.sqlite"));
  db.exec("CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT, title TEXT, archived INTEGER, git_branch TEXT, git_origin_url TEXT)");
  db.prepare("INSERT INTO threads VALUES (?, ?, 'Codex receiver', 0, 'main', 'git@github.com:Owner/Repo.git')").run(thread, f.root);
  let live = true;
  const socket = await unixServer(join(f.codexHome, "ipc/ipc.sock"), connection => {
    let buffer = Buffer.alloc(0);
    connection.on("data", chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32LE(0)) {
        const length = buffer.readUInt32LE(0), request = JSON.parse(buffer.subarray(4, 4 + length).toString());
        buffer = buffer.subarray(4 + length);
        const response = Buffer.from(JSON.stringify({ type: "response", requestId: request.requestId,
          resultType: request.method === "initialize" || live ? "success" : "error",
          error: live ? undefined : "no-client-found", result: { clientId: "desktop" } }));
        const header = Buffer.alloc(4); header.writeUInt32LE(response.length); connection.write(Buffer.concat([header, response]));
      }
    });
  });
  const log = join(f.root, "commands");
  const queue = join(f.root, "codex"), opener = join(f.root, "open");
  // Avoid the codexHome directory itself as executable path.
  const queueBin = queue + "-cli";
  const script = (label: string) => `#!/usr/bin/env node\nimport { appendFileSync } from 'node:fs';\nappendFileSync(${JSON.stringify(log)}, JSON.stringify([${JSON.stringify(label)}, ...process.argv.slice(2)])+'\\n');\n`;
  await writeFile(queueBin, script("queue"), { mode: 0o700 }); await writeFile(opener, script("open"), { mode: 0o700 });
  f.config.codexBin = queueBin; f.serviceOptions.codex = { openBin: opener };
  return { thread, meta: { "x-codex-turn-metadata": JSON.stringify({ thread_id: thread }) }, log, db,
    setLive(value: boolean) { live = value; },
    async close() { await socket.stop(); db.close(); } };
}
