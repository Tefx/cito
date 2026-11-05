import { readdir, stat } from "node:fs/promises";
import { createConnection } from "node:net";
import { join } from "node:path";
import { CitoError, loadConfig, validateHarness, type CitoConfig } from "../shared/index.js";
import type { ListenerRow } from "../shared/state-types.js";
import { gitMetadata } from "./git.js";
import type { AvailableSessionSummary, DeliverContext, DeliverOutcome, HarnessAdapter, ProbeOutcome } from "./types.js";

type BridgeConfig = Pick<CitoConfig, "stateDir">;
export interface BridgeIdentity { endpoint_pid: number; session_id: string; session_name: string; cwd: string }
class PiBridgeError extends CitoError {
  constructor(readonly bridgeError: string) {
    super(bridgeError === "SESSION_MISMATCH" || bridgeError === "SESSION_NOT_OPEN"
      ? "SESSION_NOT_OPEN" : "PI_BRIDGE_UNAVAILABLE", `Socket bridge: ${bridgeError}`);
  }
}
export function bridgeSocketPath(stateDir: string, harness: string, pid: number): string {
  return join(stateDir, "sockets", `${validateHarness(harness)}-pid-${pid}.sock`);
}

/** One newline request/reply with a total deadline and abortable socket lifetime. */
export async function requestBridge(path: string, request: object, timeout: number, signal?: AbortSignal): Promise<Record<string, unknown>> {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    let buffer = Buffer.alloc(0), settled = false;
    const finish = (error?: unknown, value?: Record<string, unknown>) => {
      if (settled) return;
      settled = true; clearTimeout(timer); signal?.removeEventListener("abort", abort); socket.destroy();
      if (error) reject(error); else resolve(value!);
    };
    const abort = () => finish(signal!.reason ?? new DOMException("Aborted", "AbortError"));
    const timer = setTimeout(() => finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Socket bridge timed out")), timeout);
    signal?.addEventListener("abort", abort, { once: true });
    socket.on("error", cause => finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Socket bridge connection failed", { cause })));
    socket.on("close", () => finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Socket bridge closed before replying")));
    socket.on("connect", () => { if (signal?.aborted) abort(); else socket.write(JSON.stringify(request) + "\n"); });
    socket.on("data", chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length > 4 * 1024 * 1024) { finish(new PiBridgeError("INVALID_RESPONSE")); return; }
      const end = buffer.indexOf(10); if (end < 0) return;
      try {
        const reply: unknown = JSON.parse(buffer.subarray(0, end).toString("utf8"));
        if (!reply || typeof reply !== "object" || Array.isArray(reply)) throw new Error("Expected object");
        const record = reply as Record<string, unknown>;
        if (record.ok === false && typeof record.error === "string") { finish(new PiBridgeError(record.error)); return; }
        if (record.ok !== true) throw new Error("Missing ok");
        finish(undefined, record);
      } catch (cause) { finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Invalid socket bridge reply", { cause })); }
    });
  });
}
export async function identifyBridge(harness: string, pid: number, config: BridgeConfig = loadConfig(), signal?: AbortSignal, timeoutMs = 1500): Promise<BridgeIdentity & Record<string, unknown>> {
  const record = await requestBridge(bridgeSocketPath(config.stateDir, harness, pid), { op: "identify" }, timeoutMs, signal);
  if ((record.endpoint_pid ?? record.pi_pid) !== pid || typeof record.session_id !== "string" || !record.session_id
    || typeof record.session_name !== "string" || typeof record.cwd !== "string" || !record.cwd) throw new PiBridgeError("INVALID_RESPONSE");
  return { ...record, endpoint_pid: pid, session_id: record.session_id, session_name: record.session_name, cwd: record.cwd };
}
export class SocketBridgeAdapter implements HarnessAdapter {
  readonly id: string;
  private readonly confirmDelivery: boolean;
  constructor(harnessId: string, { confirmDelivery = true }: { confirmDelivery?: boolean } = {}) {
    this.id = validateHarness(harnessId); this.confirmDelivery = confirmDelivery;
  }
  async discoverAvailableSessions(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]> {
    const socketsDir = join(config.stateDir, "sockets");
    let names: string[];
    try { names = await readdir(socketsDir); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
    const pattern = new RegExp(`^${this.id}-pid-([1-9][0-9]*)\\.sock$`);
    const settled = await Promise.allSettled(names.map(async (name): Promise<AvailableSessionSummary | null> => {
      const match = pattern.exec(name);
      if (!match) return null;
      const pid = Number(match[1]);
      if (!Number.isSafeInteger(pid) || pid <= 0) return null;
      try { process.kill(pid, 0); } catch { return null; }
      const st = await stat(join(socketsDir, name));
      if (!st.isSocket()) return null;
      const info = await identifyBridge(this.id, pid, config, signal, 400);
      const meta = await gitMetadata(info.cwd, info.session_name);
      const updatedAtMs = typeof info.updated_at_ms === "number" && Number.isFinite(info.updated_at_ms) && info.updated_at_ms > 0
        ? Math.round(info.updated_at_ms) : Math.round(st.mtimeMs);
      return {
        harness: this.id,
        session_id: info.session_id,
        title: meta.title,
        cwd: info.cwd,
        branch: meta.branch,
        endpoint_pid: pid,
        updated_at_ms: updatedAtMs,
      };
    }));
    const sessions = settled.flatMap(r => r.status === "fulfilled" && r.value ? [r.value] : []);
    sessions.sort((a, b) => (b.updated_at_ms ?? 0) - (a.updated_at_ms ?? 0) || (b.endpoint_pid ?? 0) - (a.endpoint_pid ?? 0));
    const seen = new Set<string>();
    return sessions.filter(s => { if (seen.has(s.session_id)) return false; seen.add(s.session_id); return true; });
  }
  async probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome> {
    try {
      if (!row.endpoint_pid) throw new PiBridgeError("INVALID_PID");
      const info = await identifyBridge(this.id, row.endpoint_pid, config, signal);
      return { status: info.session_id === row.session_id ? "alive" : "switched" };
    } catch (error) {
      if (error instanceof PiBridgeError && error.bridgeError === "SESSION_NOT_OPEN") return { status: "closed_definitive" };
      if (error instanceof Error && ["ENOENT", "ECONNREFUSED"].includes((error.cause as NodeJS.ErrnoException)?.code ?? "") && row.endpoint_pid) {
        try { process.kill(row.endpoint_pid, 0); }
        catch (cause) { if ((cause as NodeJS.ErrnoException).code === "ESRCH") return { status: "closed_definitive" }; }
      }
      return { status: "transient_error", error };
    }
  }
  async deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome> {
    let switched = false;
    const sent = await ctx.runEffect(async () => {
      if (switched) return;
      try {
        if (!row.endpoint_pid) throw new PiBridgeError("INVALID_PID");
        const reply = await requestBridge(bridgeSocketPath(ctx.config.stateDir, this.id, row.endpoint_pid),
          { op: "deliver", session_id: row.session_id, batch_id: ctx.batchId, wake: true, content: ctx.card }, 3000, ctx.signal);
        if (reply.delivered !== true) throw new PiBridgeError("INVALID_RESPONSE");
      } catch (error) {
        if (error instanceof PiBridgeError && error.bridgeError === "SESSION_MISMATCH") { switched = true; return; }
        throw error;
      }
    }, { attempts: 3, pause: [1000, 2000] });
    return switched ? "session_switched" : !sent ? "driver_unavailable" : this.confirmDelivery ? "delivered" : "delivered_unconfirmed";
  }
  async updateBadge(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void> {
    if (row.endpoint_pid) await requestBridge(bridgeSocketPath(config.stateDir, this.id, row.endpoint_pid),
      { op: "status", session_id: row.session_id, badge }, 1500, signal);
  }
}
