import { spawn, execFile, type ChildProcessWithoutNullStreams } from "node:child_process";
import { promisify } from "node:util";
import { chmod, mkdir, mkdtemp, rename, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { StringDecoder } from "node:string_decoder";
import { KeyedMutex } from "../shared/index.js";

export interface BannerPayload {
  target_harness: string; target_session_id: string; target_title: string;
  from_user: string; from_host: string; from_title: string; message: string; effective_wake: boolean; count?: number;
}
export interface UiHubSummary {
  name: string; status: "connected" | "connecting" | "auth_required" | "disconnected";
  user: string | null; auth_mode: "none" | "oidc"; error: string | null;
}
export interface UiListenerSummary {
  hub: string; listener_id: string; public_id: string; scoped_id: string; harness: string; session_id: string;
  title: string; about?: string; cwd: string; repo: string | null; branch?: string | null; scope: "self" | "workspace"; can_jump: boolean;
}
export type DaemonToUiMessage = { op: "state"; hubs: UiHubSummary[]; listeners: UiListenerSummary[] } | { op: "banner"; payload: BannerPayload };
export type UiToDaemonAction = { op: "unlisten"; hub: string; listener_id: string }
  | { op: "unlisten_all" } | { op: "set_scope"; hub: string; listener_id: string; scope: "self" | "workspace" }
  | { op: "hub_action"; hub: string; action: "login" | "reconnect" | "disconnect" } | { op: "quit" };
export interface UiOptions {
  env?: NodeJS.ProcessEnv; sourcePath?: string; binary?: string;
  diagnostic?: (message: string) => void; onAction?: (action: UiToDaemonAction) => void | Promise<void>;
}
const compileMutex = new KeyedMutex(), exec = promisify(execFile);
export async function ensureUiCompiled(stateDir: string, source?: string): Promise<string> {
  return compileMutex.run(stateDir, async () => {
    if (!source) {
      const direct = fileURLToPath(new URL("../macos/cito-ui.swift", import.meta.url));
      source = await stat(direct).then(() => direct, () => fileURLToPath(new URL("../../../src/macos/cito-ui.swift", import.meta.url)));
    }
    const binary = join(stateDir, "bin/cito-ui");
    if (await stat(binary).then(async st => st.mtimeMs >= (await stat(source!)).mtimeMs, () => false)) return binary;
    await mkdir(join(stateDir, "bin"), { recursive: true, mode: 0o700 });
    const scratch = await mkdtemp(join(stateDir, "bin/.ui-build-"));
    try {
      const temp = join(scratch, "cito-ui");
      await exec("/usr/bin/swiftc", [source, "-o", temp], { timeout: 120_000, maxBuffer: 1024 * 1024 });
      await chmod(temp, 0o755); await rename(temp, binary);
    } finally { await rm(scratch, { recursive: true, force: true }); }
    return binary;
  });
}
function validAction(value: unknown): value is UiToDaemonAction {
  if (!value || typeof value !== "object") return false;
  const a = value as Record<string, unknown>;
  if (a.op === "quit" || a.op === "unlisten_all") return true;
  if (typeof a.hub !== "string" || !a.hub) return false;
  if (a.op === "hub_action") return ["login", "reconnect", "disconnect"].includes(String(a.action));
  if (typeof a.listener_id !== "string" || !/^c_[a-z2-7]{12}$/.test(a.listener_id)) return false;
  return a.op === "unlisten" || (a.op === "set_scope" && ["self", "workspace"].includes(String(a.scope)));
}
/** One persistent child. State is replaceable; banners are bounded transient events. */
export class UiBridge {
  private child?: ChildProcessWithoutNullStreams;
  private stopped = false;
  private restart?: ReturnType<typeof setTimeout>;
  private snapshot: Extract<DaemonToUiMessage, { op: "state" }> = { op: "state", hubs: [], listeners: [] };
  private pending = false;
  private scheduled = false;
  private blocked = false;
  private banners: BannerPayload[] = [];
  constructor(readonly stateDir: string, private readonly options: UiOptions = {}) {}
  private log(error: unknown) { (this.options.diagnostic ?? console.error)(String(error)); }
  async start(): Promise<void> {
    if (this.stopped || this.child) return;
    const binary = this.options.binary ?? await ensureUiCompiled(this.stateDir, this.options.sourcePath);
    if (this.stopped) return;
    const child = spawn(binary, [], { stdio: "pipe", env: { ...process.env, ...this.options.env } });
    this.child = child; this.blocked = false; this.pending = true;
    const decoder = new StringDecoder("utf8"); let buffer = "", dropping = false;
    child.stdout.on("data", (chunk: Buffer) => {
      buffer += decoder.write(chunk);
      for (;;) {
        const end = buffer.indexOf("\n");
        if (end < 0) { if (Buffer.byteLength(buffer) > 1024 * 1024) { buffer = ""; dropping = true; this.log("cito-ui: oversized action"); } break; }
        const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
        if (dropping || Buffer.byteLength(line) > 1024 * 1024) { dropping = false; continue; }
        try {
          const action: unknown = JSON.parse(line);
          if (!validAction(action)) throw new Error("invalid action");
          void Promise.resolve(this.options.onAction?.(action)).catch(error => this.log(error));
        } catch (error) { this.log("cito-ui: invalid frame: " + String(error)); }
      }
    });
    const stderrDecoder = new StringDecoder("utf8"); let stderr = "";
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += stderrDecoder.write(chunk); let end: number;
      while ((end = stderr.indexOf("\n")) >= 0) { this.log(stderr.slice(0, end)); stderr = stderr.slice(end + 1); }
    });
    child.stdin.on("error", error => { if ((error as NodeJS.ErrnoException).code !== "EPIPE") this.log(error); });
    child.stdin.on("drain", () => { this.blocked = false; this.flush(); });
    const exited = () => {
      if (this.child !== child) return;
      this.child = undefined;
      if (!this.stopped) this.scheduleRestart();
    };
    child.once("exit", exited); child.once("error", error => { this.log(error); exited(); });
    this.flush();
  }
  private scheduleRestart(): void {
    if (this.stopped) return;
    this.restart = setTimeout(() => { void this.start().catch(error => { this.log(error); this.scheduleRestart(); }); }, 500);
  }
  state(hubs: UiHubSummary[], listeners: UiListenerSummary[]): void {
    this.snapshot = { op: "state", hubs, listeners }; this.pending = true;
    if (!this.scheduled) { this.scheduled = true; queueMicrotask(() => { this.scheduled = false; this.flush(); }); }
  }
  banner(payload: BannerPayload): void { this.banners.push(payload); if (this.banners.length > 100) this.banners.shift(); this.flush(); }
  private flush(): void {
    const pipe = this.child?.stdin; if (!pipe || pipe.destroyed || this.blocked || this.stopped) return;
    const write = (msg: DaemonToUiMessage) => { this.blocked = !pipe.write(JSON.stringify(msg) + "\n"); };
    if (this.pending) { this.pending = false; write(this.snapshot); }
    while (this.banners.length && !this.blocked) write({ op: "banner", payload: this.banners.shift()! });
  }
  async close(): Promise<void> {
    this.stopped = true; clearTimeout(this.restart);
    const child = this.child; if (!child) return;
    await new Promise<void>(resolve => {
      const timeout = setTimeout(() => child.kill("SIGKILL"), 2000);
      child.once("exit", () => { clearTimeout(timeout); resolve(); }); child.stdin.end();
    });
  }
}
