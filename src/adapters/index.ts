import { execFile } from "node:child_process";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { CitoError, harnessIdSchema, loadConfig, type CitoConfig } from "../shared/index.js";
import { CodexDesktopAdapter } from "./codex.js";
import { SocketBridgeAdapter, identifyBridge } from "./pi.js";
import { gitMetadata } from "./git.js";
import type { AvailableSessionSummary, Caller, CallerDetectionContext, CallerInput, HarnessAdapter } from "./types.js";
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
export async function discoverAllAvailableSessions(
  config: CitoConfig = loadConfig(),
  activeListeners: readonly { harness: string; session_id: string; endpoint_pid?: number | null }[] = [],
  signal?: AbortSignal,
): Promise<AvailableSessionSummary[]> {
  try {
    const names = await readdir(join(config.stateDir, "sockets"));
    for (const name of names) {
      const match = /^([a-z][a-z0-9_-]{0,31})-pid-[1-9][0-9]*\.sock$/.exec(name);
      if (match) getAdapter(match[1]!);
    }
  } catch { /* Missing sockets dir is normal. */ }
  const settled = await Promise.allSettled(
    [...ADAPTER_REGISTRY.values()].map(adapter => adapter.discoverAvailableSessions?.(config, signal) ?? Promise.resolve([])),
  );
  const raw = settled.flatMap(r => r.status === "fulfilled" ? r.value : []);
  const seen = new Set<string>();
  const filtered = raw.filter(c => {
    const isActive = activeListeners.some(a =>
      a.harness === c.harness && (
        a.session_id === c.session_id
        || (c.endpoint_pid !== undefined && a.endpoint_pid != null && a.endpoint_pid === c.endpoint_pid)
      ),
    );
    if (isActive) return false;
    const key = `${c.harness}:${c.session_id}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  const byHarness = new Map<string, AvailableSessionSummary[]>();
  for (const c of filtered) {
    let list = byHarness.get(c.harness);
    if (!list) { list = []; byHarness.set(c.harness, list); }
    list.push(c);
  }
  const rank = (h: string) => h === "pi" ? 0 : h === "codex" ? 1 : 2;
  const harnesses = [...byHarness.keys()].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
  const result: AvailableSessionSummary[] = [];
  for (const harness of harnesses) {
    const group = byHarness.get(harness)!;
    if (group.some(item => item.endpoint_pid !== undefined)) {
      group.sort((a, b) => (b.updated_at_ms ?? 0) - (a.updated_at_ms ?? 0) || (b.endpoint_pid ?? 0) - (a.endpoint_pid ?? 0));
    }
    for (const item of group.slice(0, 8)) {
      if (result.length >= 16) break;
      result.push(item);
    }
    if (result.length >= 16) break;
  }
  return result;
}
function verifyExpectedIdentity(caller: Caller, meta: Record<string, unknown>): Caller {
  if ((typeof meta.expected_harness === "string" && meta.expected_harness && caller.harness !== meta.expected_harness)
    || (typeof meta.expected_session_id === "string" && meta.expected_session_id && caller.session_id !== meta.expected_session_id)) {
    throw new CitoError("SESSION_NOT_OPEN", "Session changed since discovery");
  }
  return caller;
}
export type ResolveCallerOptions = Pick<CallerDetectionContext, "requireOpen" | "lookupCachedSession">;
// Preserve the legacy public error code without exposing driver naming to the service.
export function callerBindingError(message: string): CitoError {
  return new CitoError("PI_BRIDGE_UNAVAILABLE", message);
}
const pidKeys = ["endpoint_pid", "pi_pid", "caller_pid"];
function explicitPid(meta: Record<string, unknown>): number | undefined {
  const values = pidKeys.filter(key => meta[key] !== undefined).map(key => meta[key]);
  if (values.some(v => typeof v !== "number" || !Number.isSafeInteger(v) || v <= 0)) {
    throw callerBindingError("Invalid endpoint PID");
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
  if (roots.size) return [...roots];
  if (typeof meta.http_client_pid === "number" && Number.isSafeInteger(meta.http_client_pid) && meta.http_client_pid > 0) return [meta.http_client_pid];
  return meta.http_transport === true ? [] : [process.ppid];
}
const exec = promisify(execFile);
/** Resolve explicit adapter claims before implicit discovery or service effects. */
export async function resolveCaller(input: CallerInput = {}, config: CitoConfig = loadConfig(), options: ResolveCallerOptions = {}): Promise<Caller> {
  // Direct embedders may still pass legacy metadata. HTTP callers always supply an envelope.
  const ctx: CallerDetectionContext = input.meta && typeof input.meta === "object" && !Array.isArray(input.meta)
    ? { ...input as CallerDetectionContext, ...options }
    : { meta: input as Record<string, unknown>, httpClientPid: (input as Record<string, unknown>).http_client_pid as number | undefined,
      httpTransport: (input as Record<string, unknown>).http_transport === true, ...options };
  const meta: Record<string, unknown> = { ...ctx.meta, http_client_pid: ctx.httpClientPid, http_transport: ctx.httpTransport };
  const pid = explicitPid(meta);
  let explicit: Caller | undefined;
  for (const adapter of ADAPTER_REGISTRY.values()) {
    const caller = await adapter.detectExplicitCaller?.(ctx, config);
    if (!caller) continue;
    if (pid !== undefined || explicit) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    explicit = caller;
  }
  if (explicit) return verifyExpectedIdentity(explicit, meta);
  const roots = extractCallerPidRoots(meta);
  let failure: unknown;
  for (let ancestor of roots) {
    for (let depth = 0; depth <= 5 && ancestor > 0; depth++) {
      let names: string[] = [];
      try { names = await readdir(join(config.stateDir, "sockets")); }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      const pattern = new RegExp(`^([a-z][a-z0-9_-]{0,31})-pid-${ancestor}\\.sock$`);
      const harnesses = names.flatMap(name => { const match = pattern.exec(name); return match ? [match[1]!] : []; });
      const expectedHarness = typeof meta.expected_harness === "string" ? meta.expected_harness : undefined;
      harnesses.sort((a, b) => Number(b === expectedHarness) - Number(a === expectedHarness)
        || Number(ADAPTER_REGISTRY.get(b) instanceof SocketBridgeAdapter) - Number(ADAPTER_REGISTRY.get(a) instanceof SocketBridgeAdapter)
        || a.localeCompare(b));
      for (const harness of harnesses) {
        let info;
        try {
          info = await identifyBridge(harness, ancestor, config);
        } catch (error) { failure = error; continue; }
        getAdapter(harness);
        return verifyExpectedIdentity({ harness, endpoint_pid: info.endpoint_pid, session_id: info.session_id, cwd: info.cwd,
          ...await gitMetadata(info.cwd, info.session_name) }, meta);
      }
      if (ctx.requireOpen === false) {
        // The lookup enforces uniqueness across every retained status and hub.
        const cached = await ctx.lookupCachedSession?.({ endpointPid: ancestor });
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
  // Missing/unopen implicit ancestors leave outbound callers unbound. An explicit PID is a claim and must fail visibly.
  throw new CitoError(pid !== undefined ? "PI_BRIDGE_UNAVAILABLE" : "BINDING_REQUIRED",
    "No caller socket bridge found", { cause: failure });
}
