import { chmodSync, mkdirSync, readFileSync, unlinkSync } from "node:fs";
import { createConnection, createServer, type Server, type Socket } from "node:net";
import { homedir } from "node:os";
import { basename, join, resolve } from "node:path";
import { stripVTControlCharacters } from "node:util";

/** Structural subset of Pi 1.5.0's public API: no host package is needed to load this file. */
export interface BridgeContext {
  cwd: string;
  sessionManager: { getSessionId(): string | undefined; getSessionName(): string | undefined };
  ui: {
    setStatus(key: string, value: string | undefined): void;
    notify?(message: string, type?: "info" | "warning" | "error"): void;
  };
}
export interface BridgeCommandOptions {
  description: string;
  getArgumentCompletions?: (prefix: string) => { value: string; label: string }[] | null;
  handler: (args: string, ctx: BridgeContext) => void | Promise<void>;
}
interface NotificationTheme {
  fg(color: "accent" | "borderAccent", text: string): string;
  bg(color: "customMessageBg", text: string): string;
  bold(text: string): string;
}
export type NotificationRenderer = (message: { content: unknown }, options: { expanded: boolean; outputPad?: number },
  theme: NotificationTheme) => { render(width: number): string[]; invalidate(): void };
export interface BridgeAPI {
  registerMcpServer?(name: string, config: { command: string; args?: string[]; env?: Record<string, string> }): void;
  registerMessageRenderer?(customType: string, renderer: NotificationRenderer): void;
  registerCommand?(name: string, options: BridgeCommandOptions): void;
  on(event: "session_start" | "session_shutdown" | "session_info_changed" | "session_tree",
    handler: (event: unknown, ctx: BridgeContext) => void | Promise<void>): unknown;
  sendMessage(message: { customType: string; content: string; display: boolean },
    options: { triggerTurn: boolean; deliverAs: "followUp" }): void;
}
const MAX_REQUEST_BYTES = 4 * 1024 * 1024;

// The package extension keeps rendering free of host-package imports.
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
  const stateDir = process.env.CITO_STATE_DIR ?? join(homedir(), ".cito");
  const resolvedStateDir = stateDir === "~" ? homedir() : stateDir.startsWith("~/")
    ? join(homedir(), stateDir.slice(2)) : resolve(stateDir);
  const shim = join(resolvedStateDir, "bin", "cito-mcp");
  let mcpRegistered = false;
  function registerMcpServer(): void {
    if (mcpRegistered || !pi.registerMcpServer) return;
    try {
      pi.registerMcpServer("cito", { command: shim, args: [] });
      mcpRegistered = true;
    } catch (error) {
      if (error instanceof Error && /already registered/i.test(error.message)) {
        mcpRegistered = true;
        return;
      }
      throw error;
    }
  }
  // Some hosts initialize the registry after synchronous extension loading.
  try { registerMcpServer(); }
  catch { /* session_start retries and surfaces any persistent failure. */ }
  const sessions = new Map<string, Set<string>>();
  let sessionId: string | undefined;
  let activeBadge: string | undefined;
  let lastActiveMs = Date.now();
  let context: BridgeContext | undefined;
  let server: Server | undefined;
  let ownsSocket = false;
  const connections = new Set<Socket>();
  const directory = join(resolvedStateDir, "sockets");
  const socketPath = join(directory, `pi-pid-${process.pid}.sock`);
  const subcommands = ["on", "team", "off"];
  const hubPattern = /^[a-z0-9][a-z0-9_-]{0,31}$/;
  const usage = "Usage: /cito [on [about...] | team [about...] | off]";
  const hubUsage = "Usage: /cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]";

  function defaultAbout(ctx: BridgeContext): string {
    return ctx.sessionManager.getSessionName()?.trim() || basename(ctx.cwd) || "Pi session";
  }
  async function callMcpTool(name: "cito_listen" | "cito_unlisten", args: Record<string, unknown>): Promise<Record<string, unknown>> {
    let configuredPort: unknown;
    try { configuredPort = (JSON.parse(readFileSync(join(resolvedStateDir, "config.json"), "utf8")) as { daemonPort?: unknown }).daemonPort; }
    catch {} // A missing config falls back to the environment and default port.
    const candidatePorts = [configuredPort, process.env.CITO_DAEMON_PORT]
      .filter(value => typeof value === "number" || (typeof value === "string" && value.trim() !== ""))
      .map(Number);
    const port = candidatePorts.find(value => Number.isInteger(value) && value > 0 && value <= 65535) ?? 4290;
    const res = await fetch(`http://127.0.0.1:${port}/mcp`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: {
          name,
          arguments: args,
          _meta: { endpoint_pid: process.pid },
        },
      }),
    });
    const raw = await res.text();
    const dataLines = raw.split(/\r?\n/).filter(line => line.startsWith("data:")).map(line => line.slice(5).trim()).filter(Boolean);
    const jsonText = dataLines.at(-1) ?? (raw.trim().startsWith("{") ? raw.trim() : "");
    if (!jsonText) throw new Error(raw.trim() || `HTTP ${res.status}`);
    const rpc = JSON.parse(jsonText) as { error?: { message?: unknown }; result?: { isError?: boolean; content?: { type?: string; text?: string }[] } };
    if (rpc.error) throw new Error(typeof rpc.error.message === "string" ? rpc.error.message : JSON.stringify(rpc.error));
    const text = rpc.result?.content?.find(part => part?.type === "text" && typeof part.text === "string")?.text;
    let payload: Record<string, unknown> = {};
    if (text) {
      try { payload = JSON.parse(text) as Record<string, unknown>; }
      catch { if (rpc.result?.isError || !res.ok) throw new Error(text); }
    }
    if (rpc.result?.isError || payload.ok === false || !res.ok) {
      const errObj = payload.error as { message?: unknown } | string | undefined;
      const msg = typeof errObj === "object" && errObj && typeof errObj.message === "string"
        ? errObj.message : typeof errObj === "string" ? errObj : text || `HTTP ${res.status}`;
      throw new Error(msg);
    }
    return payload;
  }

  function update(ctx: BridgeContext): void {
    const nextSessionId = ctx.sessionManager.getSessionId();
    if (nextSessionId !== sessionId) {
      activeBadge = undefined;
      ctx.ui.setStatus("cito", undefined);
      lastActiveMs = Date.now();
    }
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
    if (req.op === "ping") return { ok: true };
    if (req.op === "identify") {
      if (!ctx || !sessionId) return { ok: false, error: "SESSION_NOT_OPEN" };
      return { ok: true, endpoint_pid: process.pid, pi_pid: process.pid, session_id: sessionId,
        session_name: ctx.sessionManager.getSessionName() ?? "", cwd: ctx.cwd, updated_at_ms: lastActiveMs };
    }
    if (req.op !== "status" && req.op !== "deliver") return { ok: false, error: "INVALID_REQUEST" };
    if (!ctx || !sessionId || req.session_id !== sessionId) return { ok: false, error: "SESSION_MISMATCH" };
    if (req.op === "status") {
      if (req.badge != null && typeof req.badge !== "string") return { ok: false, error: "INVALID_REQUEST" };
      activeBadge = (req.badge as string | undefined) ?? undefined;
      ctx.ui.setStatus("cito", activeBadge);
      return { ok: true };
    }
    const processedBatchIds = sessions.get(sessionId)!;
    if (typeof req.batch_id !== "string" || !req.batch_id) return { ok: false, error: "INVALID_REQUEST" };
    if (processedBatchIds.has(req.batch_id)) return { ok: true, delivered: true, deduplicated: true };
    if (typeof req.content !== "string" || typeof req.wake !== "boolean") return { ok: false, error: "INVALID_REQUEST" };
    // Pi 1.5.0 sendMessage is synchronous. No await between the effect and recording its ID.
    pi.sendMessage({ customType: "cito-notification", content: req.content, display: true },
      { triggerTurn: req.wake, deliverAs: "followUp" });
    lastActiveMs = Date.now();
    processedBatchIds.add(req.batch_id);
    if (processedBatchIds.size > 200) processedBatchIds.delete(processedBatchIds.values().next().value!);
    return { ok: true, delivered: true, deduplicated: false };
  }

  pi.on("session_start", async (_event, ctx) => {
    registerMcpServer();
    lastActiveMs = Date.now();
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
      const listen = () => new Promise<void>((resolve, reject) => {
        instance.once("error", reject);
        instance.listen(socketPath, () => {
          ownsSocket = true;
          instance.removeListener("error", reject);
          resolve();
        });
      });
      try { await listen(); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EADDRINUSE") throw error;
        const live = await new Promise<boolean>(resolve => {
          const peer = createConnection(socketPath);
          let buffer = "";
          let finished = false;
          const finish = (alive: boolean) => {
            if (finished) return;
            finished = true;
            clearTimeout(timer);
            peer.destroy();
            resolve(alive);
          };
          const timer = setTimeout(() => finish(false), 250);
          peer.once("error", () => finish(false));
          peer.once("close", () => finish(false));
          peer.once("connect", () => peer.write('{"op":"ping"}\n'));
          peer.on("data", chunk => {
            buffer += chunk.toString("utf8");
            if (buffer.length > MAX_REQUEST_BYTES) { finish(false); return; }
            const end = buffer.indexOf("\n");
            if (end < 0) return;
            try { finish((JSON.parse(buffer.slice(0, end)) as { ok?: unknown }).ok === true); }
            catch { finish(false); }
          });
        });
        if (live) throw error;
        try { unlinkSync(socketPath); }
        catch (unlinkError) { if ((unlinkError as NodeJS.ErrnoException).code !== "ENOENT") throw unlinkError; }
        await listen();
      }
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
  pi.registerCommand?.("cito", {
    description: "Toggle or configure Cito session listening",
    getArgumentCompletions(prefix) {
      const trimmed = prefix.trim();
      if (!/\s/.test(trimmed)) {
        return subcommands.filter(cmd => cmd.startsWith(trimmed)).map(value => ({ value, label: value }));
      }
      const match = /^(on|team|off)\s+(\S*)$/.exec(trimmed);
      if (match && match[2]!.startsWith("-") && "--hub".startsWith(match[2]!)) {
        return [{ value: `${match[1]} --hub `, label: "--hub <hub>" }];
      }
      return [];
    },
    async handler(args, ctx) {
      lastActiveMs = Date.now();
      update(ctx);
      const trimmed = args.trim();
      let tool: "cito_listen" | "cito_unlisten";
      let toolArgs: Record<string, unknown>;
      let targetHub: string | undefined;
      if (!trimmed) {
        if (activeBadge) {
          tool = "cito_unlisten";
          toolArgs = {};
        } else {
          tool = "cito_listen";
          toolArgs = { about: defaultAbout(ctx), scope: "self" };
        }
      } else {
        const space = trimmed.search(/\s/);
        const sub = space < 0 ? trimmed : trimmed.slice(0, space);
        let rest = space < 0 ? "" : trimmed.slice(space).trim();
        if (sub !== "on" && sub !== "team" && sub !== "off") {
          ctx.ui.notify?.(usage, "warning");
          return;
        }
        const firstSpace = rest.search(/\s/);
        const firstToken = firstSpace < 0 ? rest : rest.slice(0, firstSpace);
        const afterFirst = firstSpace < 0 ? "" : rest.slice(firstSpace).trim();
        if (firstToken === "--hub") {
          const secondSpace = afterFirst.search(/\s/);
          const candidateHub = secondSpace < 0 ? afterFirst : afterFirst.slice(0, secondSpace);
          if (!candidateHub || !hubPattern.test(candidateHub)) {
            ctx.ui.notify?.(hubUsage, "warning");
            return;
          }
          targetHub = candidateHub;
          rest = secondSpace < 0 ? "" : afterFirst.slice(secondSpace).trim();
        } else if (firstToken.startsWith("--hub=")) {
          const candidateHub = firstToken.slice("--hub=".length);
          if (!candidateHub || !hubPattern.test(candidateHub)) {
            ctx.ui.notify?.(hubUsage, "warning");
            return;
          }
          targetHub = candidateHub;
          rest = afterFirst;
        }
        if (sub === "on") {
          tool = "cito_listen";
          toolArgs = { about: rest || defaultAbout(ctx), scope: "self", ...(targetHub ? { hub: targetHub } : {}) };
        } else if (sub === "team") {
          tool = "cito_listen";
          toolArgs = { about: rest || defaultAbout(ctx), scope: "workspace", ...(targetHub ? { hub: targetHub } : {}) };
        } else {
          if (rest) {
            ctx.ui.notify?.(targetHub ? hubUsage : usage, "warning");
            return;
          }
          tool = "cito_unlisten";
          toolArgs = targetHub ? { hub: targetHub } : {};
        }
      }
      const commandSessionId = sessionId;
      try {
        const payload = await callMcpTool(tool, toolArgs);
        if (sessionId !== commandSessionId) return;
        if (tool === "cito_listen") {
          activeBadge ??= `🔔 ${String(payload.listener_id ?? "cito")}`;
          ctx.ui.notify?.(`Cito listening as ${String(payload.listener_id)} (${String(payload.scope)})`, "info");
        } else if (targetHub) {
          ctx.ui.notify?.(`Cito stopped listening on ${targetHub}`, "info");
        } else {
          activeBadge = undefined;
          ctx.ui.notify?.("Cito stopped listening", "info");
        }
      } catch (error) {
        if (sessionId !== commandSessionId) return;
        ctx.ui.notify?.(error instanceof Error ? error.message : String(error), "error");
      }
      ctx.ui.setStatus("cito", activeBadge);
    },
  });
  pi.on("session_info_changed", (_event, ctx) => { lastActiveMs = Date.now(); update(ctx); });
  pi.on("session_tree", (_event, ctx) => { lastActiveMs = Date.now(); update(ctx); });
  pi.on("session_shutdown", async () => {
    context = undefined;
    activeBadge = undefined;
    for (const socket of connections) socket.destroy();
    const instance = server;
    server = undefined;
    if (instance) await new Promise<void>(resolve => instance.close(() => resolve()));
    unlinkSocket();
    process.removeListener("exit", unlinkSocket);
  });
}
