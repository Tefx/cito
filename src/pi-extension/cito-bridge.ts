import { chmodSync, mkdirSync, unlinkSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { stripVTControlCharacters } from "node:util";

/** Structural subset of Pi 1.5.0's public API: no host package is needed to load this file. */
export interface BridgeContext {
  cwd: string;
  sessionManager: { getSessionId(): string | undefined; getSessionName(): string | undefined };
  ui: { setStatus(key: string, value: string | undefined): void };
}
interface NotificationTheme {
  fg(color: "accent" | "borderAccent", text: string): string;
  bg(color: "customMessageBg", text: string): string;
  bold(text: string): string;
}
export type NotificationRenderer = (message: { content: unknown }, options: { expanded: boolean; outputPad?: number },
  theme: NotificationTheme) => { render(width: number): string[]; invalidate(): void };
export interface BridgeAPI {
  registerMessageRenderer?(customType: string, renderer: NotificationRenderer): void;
  on(event: "session_start" | "session_shutdown" | "session_info_changed" | "session_tree",
    handler: (event: unknown, ctx: BridgeContext) => void | Promise<void>): unknown;
  sendMessage(message: { customType: string; content: string; display: boolean },
    options: { triggerTurn: boolean; deliverAs: "followUp" }): void;
}
const MAX_REQUEST_BYTES = 4 * 1024 * 1024;

// This extension is copied on its own into Pi: keep rendering free of host-package imports.
const graphemes = new Intl.Segmenter(undefined, { granularity: "grapheme" });
function columns(value: string): number {
  if (/^[\p{Mark}\u200d\ufe0f]*$/u.test(value)) return 0;
  return /[\p{Emoji_Presentation}\u1100-\u115f\u2329\u232a\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe10-\ufe19\ufe30-\ufe6f\uff01-\uff60\uffe0-\uffe6\u{20000}-\u{3fffd}]/u.test(value)
    || value.includes("\ufe0f") ? 2 : 1;
}
function wrapLine(value: string, width: number): { text: string; width: number }[] {
  const lines: { text: string; width: number }[] = [];
  let line = "", used = 0;
  for (const { segment } of graphemes.segment(value)) {
    const size = columns(segment);
    if (used + size > width && line) { lines.push({ text: line, width: used }); line = ""; used = 0; }
    if (size > width) continue;
    line += segment; used += size;
  }
  lines.push({ text: line, width: used });
  return lines;
}
function cardLines(content: unknown): string[] {
  const text = typeof content === "string" ? content : Array.isArray(content)
    ? content.filter(part => part?.type === "text").map(part => part.text).join("\n") : "";
  const clean = stripVTControlCharacters(text).replace(/\t/g, "  ").replace(/\r/g, "");
  return clean.startsWith("🔔 Cito") ? clean.split("\n") : ["🔔 Cito", "", ...clean.split("\n")];
}
const notificationRenderer: NotificationRenderer = (message, options, theme) => ({
  render(width) {
    if (width < 1) return [];
    const pad = Math.min(Math.max(0, Math.floor(options.outputPad ?? 0)), Math.floor((width - 1) / 2));
    const inner = width - 2 * pad;
    const lines = cardLines(message.content);
    while (lines.at(-1) === "") lines.pop();
    return lines.flatMap((line, index) => {
      const bar = index > 0 && inner >= 3;
      return wrapLine(line, inner - (bar ? 2 : 0)).map(part => {
        const styled = index === 0 ? theme.fg("accent", theme.bold(part.text)) : part.text;
        const prefix = bar ? theme.fg("borderAccent", "│ ") : "";
        return theme.bg("customMessageBg", " ".repeat(pad) + prefix + styled
          + " ".repeat(inner - (bar ? 2 : 0) - part.width + pad));
      });
    });
  },
  invalidate() {}, // Rendering is stateless; colors are reapplied on every render.
});

export default function citoBridge(pi: BridgeAPI): void {
  pi.registerMessageRenderer?.("cito-notification", notificationRenderer);
  const sessions = new Map<string, Set<string>>();
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
    const nextSessionId = ctx.sessionManager.getSessionId();
    if (nextSessionId !== sessionId) ctx.ui.setStatus("cito", undefined);
    sessionId = nextSessionId;
    if (sessionId && !sessions.has(sessionId)) sessions.set(sessionId, new Set());
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
      return { ok: true, endpoint_pid: process.pid, pi_pid: process.pid, session_id: sessionId,
        session_name: ctx.sessionManager.getSessionName() ?? "", cwd: ctx.cwd };
    }
    if (req.op !== "status" && req.op !== "deliver") return { ok: false, error: "INVALID_REQUEST" };
    if (!ctx || !sessionId || req.session_id !== sessionId) return { ok: false, error: "SESSION_MISMATCH" };
    if (req.op === "status") {
      if (req.badge != null && typeof req.badge !== "string") return { ok: false, error: "INVALID_REQUEST" };
      ctx.ui.setStatus("cito", req.badge ?? undefined);
      return { ok: true };
    }
    const processedBatchIds = sessions.get(sessionId)!;
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
