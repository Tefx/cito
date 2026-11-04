import { randomUUID } from "node:crypto";
import { chmodSync, mkdirSync, unlinkSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

/** Structural subset of Pi 1.5.0's public API: no host package is needed to load this file. */
export interface BridgeContext {
  cwd: string;
  isIdle(): boolean;
  sessionManager: { getSessionId(): string | undefined; getSessionName(): string | undefined };
  ui: { setStatus(key: string, value: string | undefined): void };
}
export interface BridgeAPI {
  on(event: "session_start" | "session_shutdown" | "session_info_changed" | "session_tree" | "input",
    handler: (event: { source?: string }, ctx: BridgeContext) => void | Promise<void>): unknown;
  sendMessage(message: { customType: string; content: string; display: boolean },
    options: { triggerTurn: boolean; deliverAs: "followUp" }): void;
}
const MAX_REQUEST_BYTES = 4 * 1024 * 1024;

export default function citoBridge(pi: BridgeAPI): void {
  const bridge_epoch = randomUUID();
  const sessions = new Map<string, { human_event_count: number; processedBatchIds: Set<string> }>();
  let sessionId: string | undefined;
  let context: BridgeContext | undefined;
  let server: Server | undefined;
  let ownsSocket = false;
  const connections = new Set<Socket>();
  const stateDir = process.env.CITO_STATE_DIR ?? join(homedir(), ".cito");
  const directory = join(stateDir === "~" ? homedir() : stateDir.startsWith("~/")
    ? join(homedir(), stateDir.slice(2)) : resolve(stateDir), "sockets");
  const socketPath = join(directory, `pi-pid-${process.pid}.sock`);

  function update(ctx: BridgeContext): void {
    sessionId = ctx.sessionManager.getSessionId();
    if (sessionId && !sessions.has(sessionId)) sessions.set(sessionId, { human_event_count: 0, processedBatchIds: new Set() });
    context = ctx;
  }
  function unlinkSocket(): void {
    if (!ownsSocket) return;
    try { unlinkSync(socketPath); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
    ownsSocket = false;
  }
  function handle(req: Record<string, unknown>): Record<string, unknown> {
    const ctx = context;
    if (ctx) update(ctx);
    if (req.op === "identify") {
      if (!ctx || !sessionId) return { ok: false, error: "SESSION_NOT_OPEN" };
      return { ok: true, pi_pid: process.pid, session_id: sessionId,
        session_name: ctx.sessionManager.getSessionName() ?? "", cwd: ctx.cwd,
        is_streaming: !ctx.isIdle(), bridge_epoch, human_event_count: sessions.get(sessionId)!.human_event_count };
    }
    if (req.op !== "status" && req.op !== "deliver") return { ok: false, error: "INVALID_REQUEST" };
    if (!ctx || !sessionId || req.session_id !== sessionId) return { ok: false, error: "SESSION_MISMATCH" };
    if (req.op === "status") {
      if (req.badge != null && typeof req.badge !== "string") return { ok: false, error: "INVALID_REQUEST" };
      ctx.ui.setStatus("cito", req.badge ?? undefined);
      return { ok: true };
    }
    const { processedBatchIds } = sessions.get(sessionId)!;
    if (typeof req.batch_id !== "string" || !req.batch_id) return { ok: false, error: "INVALID_REQUEST" };
    if (processedBatchIds.has(req.batch_id)) return { ok: true, delivered: true, deduplicated: true };
    if (typeof req.content !== "string" || typeof req.wake !== "boolean") return { ok: false, error: "INVALID_REQUEST" };
    // Pi 1.5.0 sendMessage is synchronous. No await between the effect and recording its ID.
    pi.sendMessage({ customType: "cito-notification", content: req.content, display: true },
      { triggerTurn: req.wake, deliverAs: "followUp" });
    processedBatchIds.add(req.batch_id);
    if (processedBatchIds.size > 200) processedBatchIds.delete(processedBatchIds.values().next().value!);
    return { ok: true, delivered: true, deduplicated: false };
  }

  pi.on("session_start", async (_event, ctx) => {
    update(ctx);
    if (server) return;
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    chmodSync(directory, 0o700);
    const instance = createServer(socket => {
      connections.add(socket);
      socket.on("close", () => connections.delete(socket));
      socket.on("error", () => socket.destroy());
      socket.setTimeout(3000, () => socket.destroy());
      let buffer = Buffer.alloc(0);
      let answered = false;
      socket.on("data", chunk => {
        if (answered) return;
        buffer = Buffer.concat([buffer, chunk]);
        if (buffer.length > MAX_REQUEST_BYTES) { socket.destroy(); return; }
        const end = buffer.indexOf(10);
        if (end < 0) return;
        answered = true;
        let response: Record<string, unknown>;
        let id: unknown;
        try {
          const req: unknown = JSON.parse(buffer.subarray(0, end).toString("utf8"));
          if (!req || typeof req !== "object" || Array.isArray(req)) throw new Error("Expected object");
          const record = req as Record<string, unknown>;
          id = record.id;
          response = handle(record);
        } catch { response = { ok: false, error: "BRIDGE_ERROR" }; }
        socket.end(JSON.stringify({ ...(id === undefined ? {} : { id }), ...response }) + "\n");
      });
    });
    server = instance;
    try {
      await new Promise<void>((resolve, reject) => {
        instance.once("error", reject);
        instance.listen(socketPath, () => {
          ownsSocket = true;
          instance.removeListener("error", reject);
          resolve();
        });
      });
      chmodSync(socketPath, 0o600);
      process.once("exit", unlinkSocket);
      // Listen errors after startup must not become unhandled process errors.
      instance.on("error", () => { for (const socket of connections) socket.destroy(); });
    } catch (error) {
      server = undefined;
      instance.close();
      unlinkSocket();
      throw error;
    }
  });
  pi.on("input", (event, ctx) => {
    update(ctx);
    if (event.source === "interactive" && sessionId) sessions.get(sessionId)!.human_event_count += 1;
  });
  pi.on("session_info_changed", (_event, ctx) => update(ctx));
  pi.on("session_tree", (_event, ctx) => update(ctx));
  pi.on("session_shutdown", async () => {
    context = undefined;
    for (const socket of connections) socket.destroy();
    const instance = server;
    server = undefined;
    if (instance) await new Promise<void>(resolve => instance.close(() => resolve()));
    unlinkSocket();
    process.removeListener("exit", unlinkSocket);
  });
}
