import { spawn, execFile, type ChildProcessWithoutNullStreams } from "node:child_process";
import { promisify } from "node:util";
import { chmod, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { StringDecoder } from "node:string_decoder";
import { KeyedMutex, hubNameSchema, canonicalListenerIdSchema } from "../shared/index.js";
import { getUiHarnessMetadata, type AvailableSessionSummary, type UiHarnessMetadata } from "../adapters/index.js";
export type { AvailableSessionSummary, UiHarnessMetadata } from "../adapters/index.js";

const UNSAFE_JUMP_SCHEMES = new Set([
  "javascript",
  "file",
  "data",
  "http",
  "https",
  "vbscript",
  "about",
  "blob",
]);

export function isSafeJumpUrl(url: string | null | undefined): boolean {
  if (!url || typeof url !== "string") return false;
  const trimmed = url.trim();
  if (!trimmed || /[\x00-\x20\x7f]/.test(trimmed)) return false;
  const match = /^([a-z][a-z0-9+.-]*):/i.exec(trimmed);
  if (!match) return false;
  const scheme = match[1]!.toLowerCase();
  if (UNSAFE_JUMP_SCHEMES.has(scheme)) return false;
  try {
    const parsed = new URL(trimmed);
    const parsedScheme = parsed.protocol.slice(0, -1).toLowerCase();
    return Boolean(parsedScheme) && !UNSAFE_JUMP_SCHEMES.has(parsedScheme);
  } catch {
    return false;
  }
}

export function expandJumpUrl(
  template: string | null | undefined,
  params: { session_id?: string | null; cwd?: string | null },
): string | null {
  if (!template || typeof template !== "string") return null;
  const trimmed = template.trim();
  if (!trimmed) return null;
  if (trimmed.includes("{session_id}") && (!params.session_id || !params.session_id.trim())) return null;
  if (trimmed.includes("{cwd}") && (!params.cwd || !params.cwd.trim())) return null;
  const encodedSession = params.session_id ? encodeURIComponent(params.session_id) : "";
  const pathEncodedCwd = params.cwd
    ? params.cwd.split("/").map(seg => encodeURIComponent(seg)).join("/")
    : "";
  const queryEncodedCwd = params.cwd ? encodeURIComponent(params.cwd) : "";
  const expanded = trimmed
    .replaceAll("{session_id}", encodedSession)
    .replaceAll("/{cwd}", pathEncodedCwd.startsWith("/") ? pathEncodedCwd : "/" + pathEncodedCwd)
    .replaceAll("{cwd}", queryEncodedCwd);
  return isSafeJumpUrl(expanded) ? expanded : null;
}

export interface BannerPayload {
  target_harness: string;
  target_session_id: string;
  target_title: string;
  from_user: string;
  from_host: string;
  from_title: string;
  message: string;
  effective_wake: boolean;
  count?: number;
  target_cwd?: string | null;
  jump_url?: string | null;
  jump_label?: string | null;
}
export interface UiHubSummary {
  name: string;
  status: "connected" | "connecting" | "auth_required" | "disconnected";
  user: string | null;
  auth_mode: "none" | "oidc";
  error: string | null;
}
export interface UiListenerSummary {
  hub: string;
  listener_id: string;
  public_id: string;
  scoped_id: string;
  harness: string;
  session_id: string;
  title: string;
  about?: string;
  cwd: string;
  repo: string | null;
  branch?: string | null;
  scope: "self" | "workspace";
  can_jump: boolean;
  jump_url?: string | null;
  jump_label?: string | null;
}

export interface UiRemotePeerSummary {
  hub: string;
  listener_id: string;
  public_id: string;
  scoped_id: string;
  user: string;
  host_id: string;
  host_name: string;
  harness: string;
  title: string;
  about: string;
  cwd?: string;
  repo: string | null;
  branch?: string | null;
  scope: "self" | "workspace";
  last_seen_at: string;
  is_self_principal?: boolean;
}
export type DaemonToUiMessage =
  | {
      op: "state";
      hubs: UiHubSummary[];
      listeners: UiListenerSummary[];
      available?: AvailableSessionSummary[];
      my_remote_listeners?: UiRemotePeerSummary[];
      directory_online_count?: number;
      harnesses?: UiHarnessMetadata[];
    }
  | {
      op: "directory";
      peers: UiRemotePeerSummary[];
      matched_count: number;
      online_count: number;
    }
  | { op: "banner"; payload: BannerPayload };
export type UiToDaemonAction = { op: "unlisten"; hub: string; listener_id: string }
  | { op: "unlisten_all" } | { op: "set_scope"; hub: string; listener_id: string; scope: "self" | "workspace" }
  | { op: "hub_action"; hub: string; action: "login" | "reconnect" | "disconnect" }
  | { op: "refresh_available" }
  | { op: "listen_session"; harness: string; session_id: string; endpoint_pid?: number; cwd?: string; title?: string; hub?: string; scope?: "self" | "workspace" }
  | { op: "switch_hub"; from_hub: string; to_hub: string; listener_id: string }
  | { op: "open_directory" }
  | { op: "close_directory" }
  | { op: "query_directory"; query?: string; scope?: "all" | "mine"; harness?: string }
  | { op: "quit" };
export interface UiOptions {
  env?: NodeJS.ProcessEnv;
  sourcePath?: string;
  binary?: string;
  diagnostic?: (message: string) => void;
  onAction?: (action: UiToDaemonAction) => void | Promise<void>;
}
const compileMutex = new KeyedMutex(), exec = promisify(execFile);
export async function ensureUiCompiled(stateDir: string, source?: string): Promise<string> {
  return compileMutex.run(stateDir, async () => {
    const customSource = source !== undefined;
    if (!source) {
      const direct = fileURLToPath(new URL("../macos/cito-ui.swift", import.meta.url));
      source = await stat(direct).then(() => direct, () => fileURLToPath(new URL("../../../src/macos/cito-ui.swift", import.meta.url)));
    }
    const sourceStat = await stat(source);
    const binDir = join(stateDir, "bin");
    const binary = join(binDir, "cito-ui");
    const harnessesPath = join(binDir, "harnesses.json");
    const prebuilt = !customSource && process.platform === "darwin" && process.arch === "arm64"
      ? await (async () => {
          const directPrebuilt = fileURLToPath(new URL("../../bin/cito-ui-darwin-arm64", import.meta.url));
          const distPrebuilt = fileURLToPath(new URL("../../../bin/cito-ui-darwin-arm64", import.meta.url));
          return stat(directPrebuilt).then(
            st => ({ path: directPrebuilt, stat: st }),
            () => stat(distPrebuilt).then(st => ({ path: distPrebuilt, stat: st }), () => undefined),
          );
        })()
      : undefined;
    const usePrebuilt = prebuilt !== undefined && prebuilt.stat.mtimeMs + 5_000 >= sourceStat.mtimeMs;
    const targetMtimeMs = usePrebuilt ? prebuilt.stat.mtimeMs : sourceStat.mtimeMs;
    await mkdir(binDir, { recursive: true, mode: 0o700 });
    await writeFile(harnessesPath, JSON.stringify(getUiHarnessMetadata()), { mode: 0o600 });
    if (await stat(binary).then(st => st.mtimeMs >= targetMtimeMs, () => false)) return binary;
    const scratch = await mkdtemp(join(binDir, ".ui-build-"));
    try {
      const temp = join(scratch, "cito-ui");
      if (usePrebuilt) {
        const bytes = await readFile(prebuilt.path);
        await writeFile(temp, bytes, { mode: 0o755 });
      } else {
        await exec("/usr/bin/swiftc", [source, "-o", temp], { timeout: 120_000, maxBuffer: 1024 * 1024 });
      }
      await chmod(temp, 0o755);
      await rename(temp, binary);
    } finally {
      await rm(scratch, { recursive: true, force: true });
    }
    return binary;
  });
}
export function validAction(value: unknown): value is UiToDaemonAction {
  if (!value || typeof value !== "object") return false;
  const a = value as Record<string, unknown>;
  if (a.op === "quit" || a.op === "unlisten_all" || a.op === "refresh_available" || a.op === "open_directory" || a.op === "close_directory") return true;
  if (a.op === "query_directory") {
    if (a.query !== undefined && typeof a.query !== "string") return false;
    if (a.scope !== undefined && a.scope !== "all" && a.scope !== "mine") return false;
    if (a.harness !== undefined && (typeof a.harness !== "string" || (a.harness !== "all" && !/^[a-z][a-z0-9_-]{0,31}$/.test(a.harness)))) return false;
    return true;
  }
  if (a.op === "listen_session") {
    if (typeof a.harness !== "string" || !/^[a-z][a-z0-9_-]{0,31}$/.test(a.harness)) return false;
    if (typeof a.session_id !== "string" || !a.session_id.trim()) return false;
    if (a.endpoint_pid !== undefined && (typeof a.endpoint_pid !== "number" || !Number.isSafeInteger(a.endpoint_pid) || a.endpoint_pid <= 0)) return false;
    if (a.cwd !== undefined && typeof a.cwd !== "string") return false;
    if (a.title !== undefined && typeof a.title !== "string") return false;
    if (a.hub !== undefined && !hubNameSchema.safeParse(a.hub).success) return false;
    if (a.scope !== undefined && a.scope !== "self" && a.scope !== "workspace") return false;
    return true;
  }
  if (a.op === "switch_hub") {
    if (!hubNameSchema.safeParse(a.from_hub).success || !hubNameSchema.safeParse(a.to_hub).success) return false;
    if (a.from_hub === a.to_hub) return false;
    if (!canonicalListenerIdSchema.safeParse(a.listener_id).success) return false;
    return true;
  }
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
  private snapshot: Extract<DaemonToUiMessage, { op: "state" }> = {
    op: "state",
    hubs: [],
    listeners: [],
    available: [],
    my_remote_listeners: [],
    directory_online_count: 0,
    harnesses: getUiHarnessMetadata(),
  };
  private directorySnapshot?: Extract<DaemonToUiMessage, { op: "directory" }>;
  private pending = false;
  private directoryPending = false;
  private scheduled = false;
  private blocked = false;
  private banners: BannerPayload[] = [];
  readonly stateDir: string;
  private readonly options: UiOptions;
  constructor(stateDir: string, options: UiOptions = {}) {
    this.stateDir = stateDir;
    this.options = options;
  }
  private log(error: unknown) { (this.options.diagnostic ?? console.error)(String(error)); }
  async start(): Promise<void> {
    if (this.stopped || this.child) return;
    const binary = this.options.binary ?? await ensureUiCompiled(this.stateDir, this.options.sourcePath);
    if (this.stopped) return;
    const child = spawn(binary, [], { stdio: "pipe", env: { ...process.env, ...this.options.env } });
    this.child = child; this.blocked = false; this.pending = true; this.directoryPending = this.directorySnapshot !== undefined;
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
  state(
    hubs: UiHubSummary[],
    listeners: UiListenerSummary[],
    available: AvailableSessionSummary[] = [],
    myRemoteListeners: UiRemotePeerSummary[] = [],
    directoryOnlineCount = 0,
    harnesses?: UiHarnessMetadata[],
  ): void {
    const resolvedHarnesses = harnesses ?? getUiHarnessMetadata();
    const byHarness = new Map(resolvedHarnesses.map(h => [h.id, h]));
    const enrichedListeners = listeners.map((listener): UiListenerSummary => {
      if (listener.can_jump === false) return listener;
      const meta = byHarness.get(listener.harness);
      const jumpUrl = listener.jump_url !== undefined
        ? (isSafeJumpUrl(listener.jump_url) ? listener.jump_url : null)
        : expandJumpUrl(meta?.jump_url_template, { session_id: listener.session_id, cwd: listener.cwd });
      const jumpLabel = listener.jump_label ?? (jumpUrl && meta ? `Open in ${meta.display_name}` : null);
      return {
        ...listener,
        can_jump: Boolean(jumpUrl),
        ...(jumpUrl ? { jump_url: jumpUrl, ...(jumpLabel ? { jump_label: jumpLabel } : {}) } : { jump_url: null, jump_label: null }),
      };
    });
    this.snapshot = {
      op: "state",
      hubs,
      listeners: enrichedListeners,
      available,
      my_remote_listeners: myRemoteListeners,
      directory_online_count: directoryOnlineCount,
      harnesses: resolvedHarnesses,
    };
    this.pending = true;
    if (!this.scheduled) { this.scheduled = true; queueMicrotask(() => { this.scheduled = false; this.flush(); }); }
  }
  directory(peers: UiRemotePeerSummary[], matchedCount = peers.length, onlineCount = peers.length): void {
    const bounded = [...peers];
    while (
      bounded.length > 0 &&
      Buffer.byteLength(JSON.stringify({ op: "directory", peers: bounded, matched_count: matchedCount, online_count: onlineCount }) + "\n", "utf8") >= 128 * 1024
    ) {
      bounded.pop();
    }
    this.directorySnapshot = { op: "directory", peers: bounded, matched_count: matchedCount, online_count: onlineCount };
    this.directoryPending = true;
    if (!this.scheduled) { this.scheduled = true; queueMicrotask(() => { this.scheduled = false; this.flush(); }); }
  }
  banner(payload: BannerPayload): void {
    const harnesses = this.snapshot.harnesses ?? getUiHarnessMetadata();
    const meta = harnesses.find(h => h.id === payload.target_harness);
    const jumpUrl = payload.jump_url !== undefined
      ? (isSafeJumpUrl(payload.jump_url) ? payload.jump_url : null)
      : expandJumpUrl(meta?.jump_url_template, { session_id: payload.target_session_id, cwd: payload.target_cwd });
    const jumpLabel = payload.jump_label ?? (jumpUrl && meta ? `Open in ${meta.display_name} ↗` : null);
    const enriched: BannerPayload = {
      ...payload,
      ...(jumpUrl ? { jump_url: jumpUrl, ...(jumpLabel ? { jump_label: jumpLabel } : {}) } : {}),
    };
    this.banners.push(enriched);
    if (this.banners.length > 100) this.banners.shift();
    this.flush();
  }
  private flush(): void {
    const pipe = this.child?.stdin; if (!pipe || pipe.destroyed || this.blocked || this.stopped) return;
    const write = (msg: DaemonToUiMessage) => { this.blocked = !pipe.write(JSON.stringify(msg) + "\n"); };
    if (this.pending) { this.pending = false; write(this.snapshot); }
    if (this.directoryPending && !this.blocked && this.directorySnapshot) { this.directoryPending = false; write(this.directorySnapshot); }
    while (this.banners.length && !this.blocked) write({ op: "banner", payload: this.banners.shift()! });
  }
  async close(): Promise<void> {
    this.stopped = true; clearTimeout(this.restart);
    const child = this.child; if (!child) return;
    if (child.exitCode !== null || child.signalCode !== null) {
      this.child = undefined;
      return;
    }
    await new Promise<void>(resolve => {
      let settled = false;
      const done = () => {
        if (settled) return;
        settled = true;
        clearTimeout(timeout);
        child.removeListener("exit", done);
        child.removeListener("close", done);
        child.removeListener("error", done);
        resolve();
      };
      const timeout = setTimeout(() => { try { child.kill("SIGKILL"); } catch {} }, 2000);
      timeout.unref?.();
      child.once("exit", done);
      child.once("close", done);
      child.once("error", done);
      child.stdin.end();
    });
  }
}
