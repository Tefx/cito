import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { constants } from "node:fs";
import { createServer } from "node:net";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DatabaseSync, type SQLInputValue } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import bridgeEntry, { type BridgeAPI, type BridgeContext } from "../../plugins/pi/cito-bridge.js";

export const projectRoot = fileURLToPath(new URL("../../", import.meta.url));
export const CITO_VERSION = "0.18.0";
export type JsonObject = Record<string, unknown>;
export const bareId = (value: unknown): string => String(value).replace(/^[^:]+:/, "");
export interface CommandResult { stdout: string; stderr: string; code: number | null; signal: NodeJS.Signals | null }
export interface NativeProcess {
  child: ChildProcessWithoutNullStreams;
  readonly stdout: string;
  readonly stderr: string;
  result: Promise<CommandResult>;
  stop(): Promise<void>;
}

export function object(value: unknown, context = "JSON response"): JsonObject {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${context} must be an object`);
  return value as JsonObject;
}
export async function waitFor(check: () => boolean | Promise<boolean>, timeout = 5_000): Promise<void> {
  const deadline = performance.now() + timeout;
  while (performance.now() < deadline) { if (await check()) return; await delay(10); }
  throw new Error(`Condition timed out after ${timeout}ms`);
}
export async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((yes, no) => { server.once("error", no); server.listen(0, "127.0.0.1", yes); });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing TCP listener address");
  await new Promise<void>((yes, no) => server.close(error => error ? no(error) : yes()));
  return address.port;
}

/** Owns native child process groups, their ports, and every mutable host path. */
export class NativeHarness {
  readonly root: string;
  readonly home: string;
  readonly stateDir: string;
  readonly configPath: string;
  readonly daemonPort: number;
  readonly env: NodeJS.ProcessEnv;
  readonly signalStores=new Map<string,{fixture:NativeHarness;database:string}>();
  private readonly children = new Set<NativeProcess>();
  private readonly closers: (() => Promise<void>)[] = [];
  private closed = false;
  private requestId = 0;

  private constructor(root: string, port: number, overrides: NodeJS.ProcessEnv) {
    this.root = root; this.home = join(root, "home"); this.stateDir = join(root, "state");
    this.configPath = join(root, "config.json"); this.daemonPort = port;
    const inherited: NodeJS.ProcessEnv = {};
    for (const key of ["PATH", "TMPDIR", "LANG", "LC_ALL", "SYSTEMROOT"]) {
      if (process.env[key] !== undefined) inherited[key] = process.env[key];
    }
    this.env = {
      ...inherited, ...overrides,
      HOME: this.home, USERPROFILE: this.home,
      XDG_CONFIG_HOME: join(root, "xdg-config"), XDG_DATA_HOME: join(root, "xdg-data"),
      XDG_CACHE_HOME: join(root, "xdg-cache"),
      CITO_STATE_DIR: this.stateDir, CITO_CONFIG_PATH: this.configPath,
      CITO_DAEMON_PORT: String(port), CITO_HUB_DATA_DIR: join(root, "hub"),
      CITO_BUILTIN_PLUGINS_DIR: join(projectRoot, "plugins"),
      CITO_CODEX_HOME: join(root, "codex"), CODEX_HOME: join(root, "codex"),
      CITO_CLAUDE_HOME: join(root, "claude"), CLAUDE_CONFIG_DIR: join(root, "claude"),
      CITO_PI_HOME: join(this.home, ".pi"), PI_CODING_AGENT_DIR: join(this.home, ".pi", "agent"),
      CITO_DISABLE_UI: overrides.CITO_DISABLE_UI ?? "1",
      CITO_UI_HEADLESS: "1", CITO_UI_BINARY: join(projectRoot, "bin", "cito-ui-darwin-arm64"),
      CITO_LAUNCHCTL_BIN: join(root, "guard-bin", "launchctl"),
      CITO_OPEN_BIN:join(root,"guard-bin","open"),CITO_BROWSER_BIN:join(root,"guard-bin","browser"),CITO_CODEX_BIN:join(root,"guard-bin","codex"),
      CITO_USER: overrides.CITO_USER ?? "alice",
      PATH: `${join(root, "guard-bin")}:${inherited.PATH ?? "/usr/bin:/bin"}`,
    };
  }

  static async create(options: { env?: NodeJS.ProcessEnv; config?: JsonObject } = {}): Promise<NativeHarness> {
    const root = await realpath(await mkdtemp("/tmp/cito-native-"));
    try {
      const harness = new NativeHarness(root, await freePort(), options.env ?? {});
      await Promise.all([harness.home, harness.stateDir, harness.env.CITO_CODEX_HOME!, harness.env.CITO_CLAUDE_HOME!, join(root, "guard-bin")]
        .map(path => mkdir(path, { recursive: true })));
      // Setup tests may inspect this invocation log; the user's launchd is never contacted.
      await writeFile(join(root, "guard-bin", "launchctl"), `#!/bin/sh\nprintf '%s\\n' "$*" >> ${JSON.stringify(join(root, "launchctl.log"))}\nexit 0\n`, { mode: 0o700 });
      for(const name of ["open","browser","codex"])await writeFile(join(root,"guard-bin",name),`#!/bin/sh\nprintf '%s\\n' "$*" >> ${JSON.stringify(join(root,name+".log"))}\nexit ${name==="codex"?127:0}\n`,{mode:0o700});
      await harness.writeConfig({ hubs: { local: { url: "local://", authMode: "none", user: harness.env.CITO_USER } },
        defaultHub: "local", desktopBanner: false, hostHub: { enabled: false }, ...options.config });
      return harness;
    } catch (error) { await rm(root, { recursive: true, force: true }); throw error; }
  }

  async writeConfig(config: JsonObject): Promise<void> {
    await writeFile(this.configPath, JSON.stringify({ ...config, daemonPort: this.daemonPort }, null, 2) + "\n");
  }

  async binary(relative = "bin/cito"): Promise<string> {
    const path = resolve(projectRoot, relative);
    await access(path, constants.X_OK);
    return path;
  }

  async spawn(relative: string, args: string[] = [], options: { env?: NodeJS.ProcessEnv; cwd?: string; network?: "deny" } = {}): Promise<NativeProcess> {
    if (this.closed) throw new Error("Native cleanroom is closed");
    const executable = await this.binary(relative);
    const env = { ...this.env, ...options.env };
    for (const key of ["HOME", "CITO_STATE_DIR", "CITO_CONFIG_PATH"]) {
      if (typeof env[key]!=="string" || env[key] !== this.env[key]) throw new Error(`Do not override cleanroom ${key}`);
    }
    const command=options.network==="deny"?"/usr/bin/sandbox-exec":executable;
    const argv=options.network==="deny"?["-p","(version 1) (allow default) (deny network*)",executable,...args]:args;
    const child = spawn(command,argv, { cwd: options.cwd ?? this.root, env, detached: true, stdio: "pipe" });
    let stdout = "", stderr = "";
    child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
    child.stdout.on("data", chunk => { stdout += String(chunk); });
    child.stderr.on("data", chunk => { stderr += String(chunk); });
    // Unexpected pipe closure must not crash the Vitest worker during cleanup.
    child.stdin.on("error", () => {});
    const result = new Promise<CommandResult>((yes, no) => {
      child.once("error", no);
      child.once("close", (code, signal) => yes({ stdout, stderr, code, signal }));
    });
    let stopped = false;
    const process: NativeProcess = { child, get stdout() { return stdout; }, get stderr() { return stderr; }, result,
      stop: async () => {
        if (stopped) return;
        stopped = true;
        // A completed child's numeric process-group ID can be reused by the OS.
        // Detached CLI daemons are reconciled separately through their private health endpoint.
        if(child.exitCode!==null||child.signalCode!==null){await result.catch(()=>{});return;}
        const kill = (signal: NodeJS.Signals) => {
          if (!child.pid) return;
          try { globalThis.process.kill(-child.pid, signal); } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
          }
        };
        kill("SIGTERM");
        const done=await Promise.race([result.then(()=>true,()=>true),delay(2_000).then(()=>false)]);
        if(!done)kill("SIGKILL");
        await result.catch(() => {});
      },
    };
    this.children.add(process);
    return process;
  }

  async command(args: string[], options: { executable?: string; input?: string; timeout?: number; env?: NodeJS.ProcessEnv; network?: "deny" } = {}): Promise<CommandResult> {
    const executable=options.executable??"bin/cito";
    const child = await this.spawn(executable,args,{env:options.env,network:options.network});
    child.child.stdin.end(options.input ?? "");
    let timer: ReturnType<typeof setTimeout> | undefined;let completed=false;
    try {
      const result=await Promise.race([child.result, new Promise<never>((_, no) => {
        timer = setTimeout(() => no(new Error(`Native command timed out: ${args.join(" ")}\n${child.stderr}`)), options.timeout ?? 10_000);
      })]);completed=true;return result;
    } finally { if (timer) clearTimeout(timer); if(!completed)await child.stop(); }
  }

  async piBridge(sessionId = "pi-session-1") {
    const handlers = new Map<string, (event: unknown, context: BridgeContext) => unknown>();
    const messages: { content: string; options: { triggerTurn: boolean; deliverAs: string } }[] = [];
    const badges: (string | undefined)[] = [];
    let session = sessionId;
    const context: BridgeContext = { cwd: this.root,
      sessionManager: { getSessionId: () => session, getSessionName: () => "Pi receiver" },
      ui: { setStatus: (_key, value) => { badges.push(value); } } };
    const api: BridgeAPI = { on: (event, handler) => { handlers.set(event, handler); },
      sendMessage: (message, options) => { messages.push({ content: message.content, options }); } };
    const saved = { CITO_STATE_DIR: process.env.CITO_STATE_DIR, CITO_CONFIG_PATH: process.env.CITO_CONFIG_PATH };
    // The Pi package may be loaded as ESM by the host or wrapped as CommonJS by a test loader.
    const entry: unknown = bridgeEntry;
    const bridge = typeof entry === "function" ? entry : object(entry, "Pi package entrypoint").default;
    if (typeof bridge !== "function") throw new Error("Pi package has no callable extension entrypoint");
    try { process.env.CITO_STATE_DIR = this.stateDir; process.env.CITO_CONFIG_PATH = this.configPath; bridge(api); }
    finally {
      for (const [key, value] of Object.entries(saved)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
    }
    await handlers.get("session_start")!({}, context);
    let stopped = false;
    const stop = async () => { if (stopped) return; stopped = true; await handlers.get("session_shutdown")!({}, context); };
    this.closers.push(stop);
    return { meta: { pi_pid: process.pid }, messages, badges, stop,
      async switchSession(id: string) { session = id; await handlers.get("session_tree")!({}, context); } };
  }

  async daemon(env: NodeJS.ProcessEnv = {}): Promise<NativeProcess> {
    const child = await this.spawn("bin/cito", ["daemon"],{env});
    try {
      await waitFor(async () => {
        if (child.child.exitCode !== null || child.child.signalCode !== null) throw new Error(`Native daemon exited: ${child.stderr}`);
        try {
          const health = object(await (await fetch(`http://127.0.0.1:${this.daemonPort}/health`, { signal: AbortSignal.timeout(300) })).json());
          if (health.version !== CITO_VERSION) throw new Error(`Build current native binaries before testing: daemon version ${String(health.version)}`);
          if ((health.stateDir ?? health.state_dir) !== this.stateDir) throw new Error("Daemon health belongs to another state directory");
          return health.ok === true;
        } catch (error) {
          if (error instanceof Error && /Build current|another state/.test(error.message)) throw error;
          return false;
        }
      }, 10_000);
      return child;
    } catch (error) { await child.stop(); throw error; }
  }

  async hub(options: { auth?: "none" | "token" | "oidc"; token?: string; issuer?: string; clientId?: string; env?: NodeJS.ProcessEnv } = {}): Promise<{ process: NativeProcess; url: string; port: number; rpc(op: string, params?: JsonObject, headers?: Record<string, string>): Promise<unknown> }> {
    const port = await freePort();
    const url = `http://127.0.0.1:${port}`;
    const args = ["hub", "--bind", "127.0.0.1", "--port", String(port), "--data-dir", join(this.stateDir, "hub"), "--auth", options.auth ?? "none"];
    if (options.token !== undefined) args.push("--token", options.token);
    if (options.issuer !== undefined) args.push("--issuer", options.issuer);
    if (options.clientId !== undefined) args.push("--client-id", options.clientId);
    const child = await this.spawn("bin/cito", args,{env:options.env});
    try {
      await waitFor(async () => {
        if (child.child.exitCode !== null || child.child.signalCode !== null) throw new Error(`Native hub exited: ${child.stderr}`);
        try {
          const response = await fetch(url + "/health", { signal: AbortSignal.timeout(300) });
          const health = object(await response.json());
          if (health.ok===true && health.version !== CITO_VERSION) throw new Error(`Build current native binaries before testing: hub version ${String(health.version)}`);
          return health.ok === true;
        } catch (error) {
          if (error instanceof Error && error.message.startsWith("Build current")) throw error;
          return false;
        }
      }, 10_000);
    } catch (error) { await child.stop(); throw error; }
    return { process: child, url, port, rpc: async (op, params = {}, headers = {}) => {
      const response = await fetch(url + "/v1/rpc", { method: "POST", headers: {
        "content-type": "application/json", ...(options.token ? { authorization: `Bearer ${options.token}` } : {}), ...headers,
      }, body: JSON.stringify({ op, ...params }), signal: AbortSignal.timeout(10_000) });
      const result = object(await response.json());
      if (!response.ok || result.ok !== true) throw Object.assign(new Error(JSON.stringify(result.error ?? result)), { status: response.status, code: object(result.error ?? {}).code });
      return result.result;
    } };
  }

  async mcp(method: string, params: JsonObject = {}, headers: Record<string, string> = {}): Promise<JsonObject> {
    const response = await fetch(`http://127.0.0.1:${this.daemonPort}/mcp`, {
      method: "POST", headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...headers },
      body: JSON.stringify({ jsonrpc: "2.0", id: ++this.requestId, method, params }), signal: AbortSignal.timeout(10_000),
    });
    const body = await response.text();
    if (!response.ok) throw new Error(`MCP HTTP ${response.status}: ${body}`);
    const frame = body.split("\n").find(line => line.startsWith("data:"));
    const value = object(JSON.parse(frame ? frame.slice(5).trim() : body));
    if (value.error) throw new Error(JSON.stringify(value.error));
    return object(value.result, "JSON-RPC result");
  }

  async tool(name: string, args: JsonObject = {}, meta: JsonObject = {}): Promise<JsonObject> {
    const result = await this.mcp("tools/call", { name, arguments: args, _meta: meta });
    if (!Array.isArray(result.content)) throw new Error("Tool result has no content array");
    const first = object(result.content[0], "Tool content");
    if (typeof first.text !== "string") throw new Error("Tool result has no text payload");
    if (result.isError) throw new Error(first.text);
    return object(JSON.parse(first.text), "Tool payload");
  }

  async plugin(id: string, request: JsonObject): Promise<JsonObject> {
    if (!/^[a-z0-9][a-z0-9_-]{0,31}$/.test(id)) throw new Error("Invalid plugin ID");
    const dir = join(projectRoot, "plugins", id);
    const manifest = object(JSON.parse(await readFile(join(dir, "plugin.json"), "utf8")), "Plugin manifest");
    if (!Array.isArray(manifest.command) || !manifest.command.every(item => typeof item === "string") || !manifest.command[0]) {
      throw new Error(`Plugin ${id} has no subprocess command`);
    }
    const [executable, ...args] = manifest.command as [string, ...string[]];
    const path = executable.startsWith(".") ? resolve(dir, executable) : executable;
    const result = await this.command(args, { executable: path,
      input: JSON.stringify({ state_dir: this.stateDir, plugin_dir: dir, env: this.env, ...request }) + "\n" });
    if (result.code !== 0) throw new Error(`Plugin ${id} exited ${result.code}: ${result.stderr}`);
    return object(JSON.parse(result.stdout.trim()));
  }

  query(sql: string, values: SQLInputValue[] = [], database = "state.sqlite"): JsonObject[] {
    const db = new DatabaseSync(join(this.stateDir, database), { readOnly: true });db.exec("PRAGMA busy_timeout=5000");
    try { return db.prepare(sql).all(...values); } finally { db.close(); }
  }

  mutate(sql: string, values: SQLInputValue[] = [], database = "state.sqlite"): void {
    const db = new DatabaseSync(join(this.stateDir, database));db.exec("PRAGMA busy_timeout=5000");
    try { if(values.length) db.prepare(sql).run(...values); else db.exec(sql); } finally { db.close(); }
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    const stopped = await Promise.allSettled([...this.closers.reverse().map(close => close()), ...[...this.children].reverse().map(child => child.stop())]);
    // A stdio/CLI entrypoint may start a detached daemon outside its own process group.
    try {
      const response = await fetch(`http://127.0.0.1:${this.daemonPort}/health`, { signal: AbortSignal.timeout(300) });
      const health = object(await response.json());
      if ((health.stateDir ?? health.state_dir) === this.stateDir && typeof health.pid === "number" && health.pid > 1) {
        const pid = health.pid;
        const alive = () => { try { process.kill(pid, 0); return true; } catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") return false; throw error; } };
        if (alive()) process.kill(pid, "SIGTERM");
        try { await waitFor(() => !alive(), 3_000); }
        catch { if (alive()) process.kill(pid, "SIGKILL"); await waitFor(() => !alive(), 3_000); }
      }
    } catch (error) {
      if (!(error instanceof TypeError) && (error as NodeJS.ErrnoException).code !== "ECONNREFUSED") throw error;
    }
    await rm(this.root, { recursive: true, force: true });
    const failure = stopped.find(result => result.status === "rejected");
    if (failure?.status === "rejected") throw failure.reason;
  }
}

export async function nativeFixture(options: Parameters<typeof NativeHarness.create>[0] = {}): Promise<NativeHarness> {
  return NativeHarness.create(options);
}
