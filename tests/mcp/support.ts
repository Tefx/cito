import { afterAll } from "vitest";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createServer } from "node:net";
import { DatabaseSync } from "node:sqlite";
import bridge, { type BridgeAPI, type BridgeContext } from "../../src/pi-extension/cito-bridge.js";
import { ensureUiCompiled } from "../../src/ui/index.js";
import { CitoService, type ServiceOptions } from "../../src/mcp/service.js";
import { loadConfig } from "../../src/shared/index.js";
import { startNats, type TestNats } from "../shared/nats-server.js";

let cacheRoot: string | undefined;
let binary: Promise<string> | undefined;
afterAll(async () => { if (cacheRoot) await rm(cacheRoot, { recursive: true, force: true }); });
async function bannerBinary() {
  if (!binary) binary = (async () => {
    cacheRoot = await mkdtemp("/tmp/cito-mcp-banner-");
    return ensureUiCompiled(cacheRoot);
  })();
  return binary;
}
export async function freePort(): Promise<number> {
  const server = createServer(); await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as { port: number }).port;
  await new Promise<void>(resolve => server.close(() => resolve())); return port;
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
  const handlers = new Map<string, (event: unknown, ctx: BridgeContext) => unknown>();
  const messages: { content: string; options: { triggerTurn: boolean; deliverAs: string } }[] = [];
  const badges: (string | undefined)[] = [];
  let session = "pi-session-1";
  const ctx: BridgeContext = { cwd: stateDir,
    sessionManager: { getSessionId: () => session, getSessionName: () => "Pi receiver" },
    ui: { setStatus: (_key, value) => { badges.push(value); } } };
  const api: BridgeAPI = { on: (event, handler) => { handlers.set(event, handler); },
    sendMessage: (message, options) => { messages.push({ content: message.content, options }); } };
  const before = process.env.CITO_STATE_DIR;
  try { process.env.CITO_STATE_DIR = stateDir; bridge(api); }
  finally { if (before === undefined) delete process.env.CITO_STATE_DIR; else process.env.CITO_STATE_DIR = before; }
  await handlers.get("session_start")!({}, ctx);
  return { meta: { pi_pid: process.pid }, messages, badges,
    switchSession(id: string) { session = id; handlers.get("session_tree")!({}, ctx); },
    stop: async () => { await handlers.get("session_shutdown")!({}, ctx); } };
}
export async function fixture(options: { broker?: TestNats; user?: string; service?: ServiceOptions; env?: NodeJS.ProcessEnv; pi?: boolean; observeBanner?: boolean } = {}) {
  const root = await mkdtemp("/tmp/cito-mcp-");
  const stateDir = join(root, "state"), codexHome = join(root, "codex");
  await mkdir(join(stateDir, "bin"), { recursive: true }); await mkdir(codexHome);
  const broker = options.broker ?? await startNats();
  const env = { CITO_STATE_DIR: stateDir, CITO_CODEX_HOME: codexHome, CITO_NATS_URL: broker.url,
    CITO_USER: options.user ?? "alice", ...options.env };
  const uiBinary = await bannerBinary();
  const diagnostics: string[] = [];
  const config = loadConfig(env);
  config.codexDesktopRunning = async () => true;
  const serviceOptions: ServiceOptions = { diagnostic: text => diagnostics.push(text),
    ui: { binary: uiBinary, env: { CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "120" }, diagnostic: text => {
      if (text.startsWith("cito-ui: display ")) bannerDisplays.push(JSON.parse(text.slice("cito-ui: display ".length)));
      else diagnostics.push(text);
    } }, ...options.service };
  const bannerDisplays: { pid: number; title: string; subtitle: string; message: string }[] = [];
  const service = await CitoService.create(config, serviceOptions);
  const pi = options.pi === false ? undefined : await piBridge(stateDir);
  let closed = false;

  const result = { root, config, env, stateDir, codexHome, broker, service, serviceOptions, pi: pi!, diagnostics, bannerDisplays,
    async close() {
      if (closed) return; closed = true;
      await service.close(); await pi?.stop();
      if (!options.broker) await broker.stop();
      await rm(root, { recursive: true, force: true });
    } };
  try {
    if (options.observeBanner) await service.startUi();
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
  f.config.codexDesktopRunning = async () => live;
  const log = join(f.root, "commands");
  const queue = join(f.root, "codex"), opener = join(f.root, "open");
  // Avoid the codexHome directory itself as executable path.
  const queueBin = queue + "-cli";
  const script = (label: string) => `#!/usr/bin/env node\nimport { appendFileSync } from 'node:fs';\nappendFileSync(${JSON.stringify(log)}, JSON.stringify([${JSON.stringify(label)}, ...process.argv.slice(2)])+'\\n');\n`;
  await writeFile(queueBin, script("queue"), { mode: 0o700 }); await writeFile(opener, script("open"), { mode: 0o700 });
  f.config.codexBin = queueBin; f.config.openBin = opener;
  return { thread, meta: { "x-codex-turn-metadata": JSON.stringify({ thread_id: thread }) }, log, db,
    setLive(value: boolean) { live = value; },
    async close() { db.close(); } };
}
