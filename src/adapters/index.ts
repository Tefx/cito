import { execFile } from "node:child_process";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { CitoError, harnessIdSchema, loadConfig, type CitoConfig } from "../shared/index.js";
import { CodexDesktopAdapter, codexThreadId, readCodexThread, assertCodexThreadOpen } from "./codex.js";
import { SocketBridgeAdapter, identifyBridge } from "./pi.js";
import { gitMetadata, type RepoMetadata } from "./git.js";
import type { HarnessAdapter } from "./types.js";
export * from "./codex.js";
export * from "./git.js";
export * from "./pi.js";
export * from "./types.js";
export * from "./wake-card.js";

export const ADAPTER_REGISTRY = new Map<string, HarnessAdapter>();
export function registerAdapter(adapter: HarnessAdapter): void {
  if (!harnessIdSchema.safeParse(adapter.id).success) throw new CitoError("UNSUPPORTED_HARNESS", "Invalid adapter id");
  ADAPTER_REGISTRY.set(adapter.id, adapter);
}
registerAdapter(new CodexDesktopAdapter());
registerAdapter(new SocketBridgeAdapter("pi"));
export function getAdapter(harness: string): HarnessAdapter | undefined {
  if (!harnessIdSchema.safeParse(harness).success) return;
  if (!ADAPTER_REGISTRY.has(harness)) registerAdapter(new SocketBridgeAdapter(harness));
  return ADAPTER_REGISTRY.get(harness);
}
export interface Caller extends RepoMetadata { harness: string; session_id: string; cwd: string; endpoint_pid?: number }
export interface CachedSessionQuery { harness?: string; sessionId?: string; endpointPid?: number }
export interface ResolveCallerOptions {
  requireOpen?: boolean;
  lookupCachedSession?: (query: CachedSessionQuery) => Caller | undefined | Promise<Caller | undefined>;
}
const pidKeys = ["endpoint_pid", "pi_pid", "caller_pid"];
function explicitPid(meta: Record<string, unknown>): number | undefined {
  const values = pidKeys.filter(key => meta[key] !== undefined).map(key => meta[key]);
  if (values.some(v => typeof v !== "number" || !Number.isSafeInteger(v) || v <= 0)) {
    throw new CitoError("PI_BRIDGE_UNAVAILABLE", "Invalid endpoint PID");
  }
  if (new Set(values).size > 1) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  return values[0] as number | undefined;
}
export function extractCallerPidRoots(meta: Record<string, unknown> = {}): number[] {
  const pid = explicitPid(meta); if (pid !== undefined) return [pid];
  const roots = new Set<number>(), seen = new Set<object>();
  const walk = (value: unknown) => {
    if (!value || typeof value !== "object" || seen.has(value)) return;
    seen.add(value);
    for (const [key, child] of Object.entries(value)) {
      if ([...pidKeys, "parent_pid"].includes(key) && typeof child === "number" && Number.isSafeInteger(child) && child > 0) roots.add(child);
      else if (child && typeof child === "object") walk(child);
    }
  };
  for (const value of Object.values(meta)) walk(value);
  return roots.size ? [...roots] : [process.ppid];
}
const exec = promisify(execFile);
/** Validate mutually exclusive explicit claims before reading either driver's state. */
export async function resolveCaller(meta: Record<string, unknown> = {}, config: CitoConfig = loadConfig(), options: ResolveCallerOptions = {}): Promise<Caller> {
  const pid = explicitPid(meta);
  let thread: string | undefined;
  try { thread = codexThreadId(meta); }
  catch (error) { if (!(error instanceof CitoError) || error.code !== "BINDING_REQUIRED") throw error; }
  if (thread && pid !== undefined) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  if (thread) {
    try {
      const caller = readCodexThread(thread, config, { allowArchived: options.requireOpen === false });
      if (options.requireOpen !== false) await assertCodexThreadOpen(thread, config);
      return caller;
    } catch (error) {
      if (options.requireOpen === false && error instanceof CitoError && error.code === "SESSION_NOT_OPEN") {
        const cached = await options.lookupCachedSession?.({ harness: "codex", sessionId: thread });
        if (cached) return cached;
      }
      throw error;
    }
  }
  const roots = extractCallerPidRoots(meta);
  let failure: unknown;
  for (let ancestor of roots) {
    for (let depth = 0; depth <= 5 && ancestor > 0; depth++) {
      let names: string[] = [];
      try { names = await readdir(join(config.stateDir, "sockets")); }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      const pattern = new RegExp(`^([a-z][a-z0-9_-]{0,31})-pid-${ancestor}\\.sock$`);
      const harnesses = names.flatMap(name => { const match = pattern.exec(name); return match ? [match[1]!] : []; });
      harnesses.sort((a, b) => Number(ADAPTER_REGISTRY.get(b) instanceof SocketBridgeAdapter)
        - Number(ADAPTER_REGISTRY.get(a) instanceof SocketBridgeAdapter) || a.localeCompare(b));
      for (const harness of harnesses) {
        try {
          const info = await identifyBridge(harness, ancestor, config);
          getAdapter(harness);
          return { harness, endpoint_pid: info.endpoint_pid, session_id: info.session_id, cwd: info.cwd,
            ...await gitMetadata(info.cwd, info.session_name) };
        } catch (error) { failure = error; }
      }
      if (options.requireOpen === false) {
        // The lookup enforces uniqueness across every retained status and hub.
        const cached = await options.lookupCachedSession?.({ endpointPid: ancestor });
        if (cached) return cached;
      }
      try {
        const { stdout } = await exec("/bin/ps", ["-o", "ppid=", "-p", String(ancestor)], { timeout: 1000 });
        const parent = Number(stdout.trim());
        if (!Number.isSafeInteger(parent) || parent <= 0 || parent === ancestor) break;
        ancestor = parent;
      } catch { break; }
    }
  }
  if (options.requireOpen !== false && failure instanceof CitoError && failure.code === "SESSION_NOT_OPEN") throw failure;
  throw new CitoError(pid !== undefined || failure || options.requireOpen === false ? "PI_BRIDGE_UNAVAILABLE" : "BINDING_REQUIRED",
    "No caller socket bridge found", { cause: failure });
}
