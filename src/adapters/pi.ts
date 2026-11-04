import { execFile } from "node:child_process";
import { stat } from "node:fs/promises";
import { createConnection } from "node:net";
import { join } from "node:path";
import { promisify } from "node:util";
import { CitoError, loadConfig, type CitoConfig } from "../shared/index.js";
import { gitMetadata, type RepoMetadata } from "./git.js";

const exec = promisify(execFile);
type PiConfig = Pick<CitoConfig, "stateDir">;
export interface PiIdentity {
  pi_pid: number;
  session_id: string;
  session_name: string;
  cwd: string;
  is_streaming: boolean;
  bridge_epoch: string;
  human_event_count: number;
}
export interface PiCaller extends PiIdentity, RepoMetadata { harness: "pi" }
export interface PiEndpoint { pi_pid: number; session_id: string }
export interface PiDelivery { ok: true; delivered: true; deduplicated: boolean }
/** Preserve the protocol reason while using the shared public MCP error vocabulary. */
export class PiBridgeError extends CitoError {
  constructor(readonly bridgeError: string) {
    super(bridgeError === "SESSION_MISMATCH" || bridgeError === "SESSION_NOT_OPEN"
      ? "SESSION_NOT_OPEN" : "PI_BRIDGE_UNAVAILABLE", `Pi bridge: ${bridgeError}`);
  }
}
export function piSocketPath(stateDir: string, pid: number): string {
  return join(stateDir, "sockets", `pi-pid-${pid}.sock`);
}

/** One request, one reply, total deadline (not an idle timeout). */
async function requestPi(path: string, request: object, timeout: number, signal?: AbortSignal): Promise<Record<string, unknown>> {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    let buffer = Buffer.alloc(0);
    let settled = false;
    const finish = (error?: unknown, value?: Record<string, unknown>) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      socket.destroy();
      if (error) reject(error); else resolve(value!);
    };
    const abort = () => finish(signal!.reason ?? new DOMException("Aborted", "AbortError"));
    const timer = setTimeout(() => finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Pi bridge timed out")), timeout);
    signal?.addEventListener("abort", abort, { once: true });
    socket.on("error", error => finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Pi bridge connection failed", { cause: error })));
    socket.on("close", () => finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Pi bridge closed before replying")));
    socket.on("connect", () => {
      if (signal?.aborted) { abort(); return; }
      socket.write(JSON.stringify(request) + "\n");
    });
    socket.on("data", chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length > 4 * 1024 * 1024) { finish(new PiBridgeError("INVALID_RESPONSE")); return; }
      const end = buffer.indexOf(10);
      if (end < 0) return;
      try {
        const reply: unknown = JSON.parse(buffer.subarray(0, end).toString("utf8"));
        if (!reply || typeof reply !== "object" || Array.isArray(reply)) throw new Error("Expected object");
        const record = reply as Record<string, unknown>;
        if (record.ok === false && typeof record.error === "string") { finish(new PiBridgeError(record.error)); return; }
        if (record.ok !== true) throw new Error("Missing ok");
        finish(undefined, record);
      } catch (error) { finish(new CitoError("PI_BRIDGE_UNAVAILABLE", "Invalid Pi bridge reply", { cause: error })); }
    });
  });
}
function identity(record: Record<string, unknown>, pid: number): PiIdentity {
  if (record.pi_pid !== pid || typeof record.session_id !== "string" || !record.session_id
    || typeof record.session_name !== "string" || typeof record.cwd !== "string" || !record.cwd
    || typeof record.is_streaming !== "boolean" || typeof record.bridge_epoch !== "string" || !record.bridge_epoch
    || !Number.isSafeInteger(record.human_event_count) || (record.human_event_count as number) < 0) {
    throw new PiBridgeError("INVALID_RESPONSE");
  }
  // All fields have been checked above; keep unknown fields out of the public identity.
  return { pi_pid: pid, session_id: record.session_id, session_name: record.session_name, cwd: record.cwd,
    is_streaming: record.is_streaming, bridge_epoch: record.bridge_epoch, human_event_count: record.human_event_count as number };
}
export async function identifyPi(pid: number, config: PiConfig = loadConfig(), signal?: AbortSignal): Promise<PiIdentity> {
  return identity(await requestPi(piSocketPath(config.stateDir, pid), { op: "identify" }, 1500, signal), pid);
}
async function socketExists(path: string): Promise<boolean> {
  try { return (await stat(path)).isSocket(); }
  catch (error) {
    if (["ENOENT", "ENOTDIR"].includes((error as NodeJS.ErrnoException).code ?? "")) return false;
    throw new CitoError("PI_BRIDGE_UNAVAILABLE", "Cannot inspect Pi socket", { cause: error });
  }
}
export async function resolvePiCaller(meta: Record<string, unknown> = {}, config: PiConfig = loadConfig()): Promise<PiCaller> {
  const hint = meta.pi_pid ?? meta.caller_pid;
  let pid: number | undefined;
  if (hint !== undefined) {
    if (typeof hint !== "number" || !Number.isSafeInteger(hint) || hint <= 0) throw new PiBridgeError("INVALID_PID");
    pid = hint;
  } else {
    const nestedRoots: number[] = [];
    for (const value of Object.values(meta)) {
      if (!value || typeof value !== "object" || Array.isArray(value)) continue;
      const obj = value as Record<string, unknown>;
      for (const candidate of [obj.pi_pid, obj.caller_pid, obj.parent_pid]) {
        if (typeof candidate === "number" && Number.isSafeInteger(candidate) && candidate > 0 && !nestedRoots.includes(candidate)) {
          nestedRoots.push(candidate);
        }
      }
    }
    const roots = [...nestedRoots, ...(nestedRoots.includes(process.ppid) ? [] : [process.ppid])];
    outer: for (let ancestor of roots) {
      for (let depth = 0; depth < 6 && ancestor > 0; depth++) {
        if (await socketExists(piSocketPath(config.stateDir, ancestor))) { pid = ancestor; break outer; }
        try {
          const { stdout } = await exec("/bin/ps", ["-o", "ppid=", "-p", String(ancestor)], { timeout: 1000 });
          const parent = Number(stdout.trim());
          if (!Number.isSafeInteger(parent) || parent <= 0 || parent === ancestor) break;
          ancestor = parent;
        } catch { break; }
      }
    }
  }
  if (pid === undefined) throw new CitoError("BINDING_REQUIRED", "No Pi ancestor bridge found");
  const info = await identifyPi(pid, config);
  return { harness: "pi", ...info, ...await gitMetadata(info.cwd, info.session_name) };
}
export async function setPiStatus(endpoint: PiEndpoint, badge: string | null | undefined,
  config: PiConfig = loadConfig(), signal?: AbortSignal): Promise<void> {
  await requestPi(piSocketPath(config.stateDir, endpoint.pi_pid),
    { op: "status", session_id: endpoint.session_id, badge }, 1500, signal);
}
export async function deliverPiWake(endpoint: PiEndpoint, batchId: string, content: string,
  config: PiConfig = loadConfig(), signal?: AbortSignal): Promise<PiDelivery> {
  const reply = await requestPi(piSocketPath(config.stateDir, endpoint.pi_pid),
    { op: "deliver", session_id: endpoint.session_id, batch_id: batchId, wake: true, content }, 3000, signal);
  if (reply.delivered !== true || typeof reply.deduplicated !== "boolean") throw new PiBridgeError("INVALID_RESPONSE");
  return { ok: true, delivered: true, deduplicated: reply.deduplicated };
}
