import { execFile } from "node:child_process";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { CitoError, harnessIdSchema, loadConfig, type CitoConfig } from "../shared/index.js";
import { CodexDesktopAdapter } from "./codex.js";
import { SocketBridgeAdapter, identifyBridge } from "./pi.js";
import { gitMetadata } from "./git.js";
import type { Caller, CallerDetectionContext, CallerInput, HarnessAdapter } from "./types.js";
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
registerAdapter(new SocketBridgeAdapter("claude", { confirmDelivery: false }));
export function getAdapter(harness: string): HarnessAdapter | undefined {
  if (!harnessIdSchema.safeParse(harness).success) return;
  if (!ADAPTER_REGISTRY.has(harness)) registerAdapter(new SocketBridgeAdapter(harness));
  return ADAPTER_REGISTRY.get(harness);
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
  const meta = { ...ctx.meta, http_client_pid: ctx.httpClientPid, http_transport: ctx.httpTransport };
  const pid = explicitPid(meta);
  let explicit: Caller | undefined;
  for (const adapter of ADAPTER_REGISTRY.values()) {
    const caller = await adapter.detectExplicitCaller?.(ctx, config);
    if (!caller) continue;
    if (pid !== undefined || explicit) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    explicit = caller;
  }
  if (explicit) return explicit;
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
