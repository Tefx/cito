import { chmodSync, existsSync, mkdirSync, readFileSync, unlinkSync, watch } from "node:fs";
import { createConnection, createServer, type Server, type Socket } from "node:net";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { stripVTControlCharacters } from "node:util";

/** Structural subset of Pi 1.5.0's public API: no host package is needed to load this file. */
export interface BridgeContext {
  cwd: string;
  sessionManager: {
    getSessionId(): string | undefined;
    getSessionName(): string | undefined;
    getBranch?(): SessionEntry[];
    getEntries?(): SessionEntry[];
  };
  ui: {
    setStatus(key: string, value: string | undefined): void;
    notify?(message: string, type?: "info" | "warning" | "error"): void;
  };
}
interface SessionEntry { type: string; message?: { role: string; content?: unknown } }
interface SessionState {
  sessionId?: string; cwd: string; sessionName: string; about: string;
  batches: Map<string, Set<string>>;
}
const sessionKey = Symbol.for("cito.pi.sessionState");
const host = globalThis as typeof globalThis & { [sessionKey]?: SessionState };

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
  registerToolRenderer?(resolve: (name: string, next: () => ToolRenderers | undefined) => ToolRenderers | undefined): void;
  registerCommand?(name: string, options: BridgeCommandOptions): void;
  on(event: "session_start" | "session_shutdown" | "session_info_changed" | "session_tree",
    handler: (event: unknown, ctx: BridgeContext) => void | Promise<void>): unknown;
  sendMessage(message: { customType: string; content: string; display: boolean },
    options: { triggerTurn: boolean; deliverAs: "steer" | "followUp" }): void;
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
function contentText(content: unknown): string {
  return typeof content === "string" ? content : Array.isArray(content)
    ? content.filter(part => part?.type === "text" && typeof part.text === "string").map(part => part.text).join("\n") : "";
}
function promptAbout(manager: BridgeContext["sessionManager"]): string {
  for (const entry of manager.getBranch?.() ?? manager.getEntries?.() ?? []) {
    if (entry.type !== "message" || entry.message?.role !== "user") continue;
    const text = contentText(entry.message.content).replace(/\p{White_Space}/gu, " ").replace(/\p{Cc}/gu, "").replace(/ +/g, " ").trim();
    if (text) return Array.from(text).slice(0, 120).join("").trimEnd();
  }
  return "";
}
function cardLines(content: unknown): string[] {
  const clean = stripVTControlCharacters(contentText(content)).replace(/\t/g, "  ").replace(/[\x00-\x09\x0b-\x1f\x7f-\x9f]/g, "");
  return clean.startsWith("Cito") ? clean.split("\n") : ["Cito", "", ...clean.split("\n")];
}
const notificationRenderer: NotificationRenderer = (message, options, theme) => ({
  render(width) {
    if (width < 1) return [];
    const pad = Math.min(Math.max(0, Math.floor(options.outputPad ?? 0)), Math.floor((width - 1) / 2));
    const inner = width - 2 * pad;
    const lines = cardLines(message.content);
    while (lines.at(-1) === "") lines.pop();
    const padding = theme.bg("customMessageBg", " ".repeat(width));
    const body = lines.flatMap((line, index) => {
      const bar = index > 0 && inner >= 3;
      return wrapLine(line, inner - (bar ? 2 : 0)).map(part => {
        const styled = index === 0 ? theme.fg("accent", theme.bold(part.text)) : part.text;
        const prefix = bar ? theme.fg("borderAccent", "│ ") : "";
        return theme.bg("customMessageBg", " ".repeat(pad) + prefix + styled
          + " ".repeat(inner - (bar ? 2 : 0) - part.width + pad));
      });
    });
    return [padding, ...body, padding];
  },
  invalidate() {}, // Rendering is stateless; colors are reapplied on every render.
});

interface ToolRenderers {
  renderShell?: "default" | "self";
  renderCall?(args: Record<string, unknown>, theme: NotificationTheme, context: { expanded: boolean; outputPad: number }): ReturnType<NotificationRenderer>;
  renderResult?(result: { content: unknown }, options: { expanded: boolean }, theme: NotificationTheme, context: { isError: boolean; outputPad: number }): ReturnType<NotificationRenderer>;
}

function expandHome(path: string): string {
  return path === "~" ? homedir() : path.startsWith("~/") ? join(homedir(), path.slice(2)) : resolve(path);
}

export default function citoBridge(pi: BridgeAPI): void {
  pi.registerMessageRenderer?.("cito-notification", notificationRenderer);
  pi.registerToolRenderer?.((name, next) => {
    const action = /^mcp__cito__cito_(send|find|listen|unlisten|inbox|status)$/.exec(name)?.[1];
    if (!action) return next();
    return {
      renderShell: "self",
      renderCall(args, theme, context) {
        const subject = args.to ?? args.query ?? args.about ?? args.hub;
        return notificationRenderer({ content: `Cito · ${action}${typeof subject === "string" ? ` · ${subject}` : ""}` }, context, theme);
      },
      renderResult(result, options, theme, context) {
        let body = contentText(result.content);
        if (!options.expanded && !context.isError) {
          try {
            const receipt = JSON.parse(body);
            const summary = receipt.signal_id ?? receipt.listener_id ?? receipt.status
              ?? (Array.isArray(receipt.signals) ? `${receipt.signals.length} signals · ${receipt.acknowledged_count ?? 0} acknowledged`
                : Array.isArray(receipt.listeners) ? `${receipt.matched_count} listeners · ${receipt.online_count} online`
                : typeof receipt.registered === "boolean" ? `${receipt.registered ? "Listening" : "Not listening"} · ${receipt.unread_count} unread` : undefined);
            if (typeof summary === "string" && receipt.ok !== false && !receipt.error && !receipt.warning && !receipt.maintenance_warning && !receipt.warnings?.length && !receipt.replay_gap_detected) body = summary;
          } catch { /* Preserve non-JSON results and diagnostics. */ }
        }
        return notificationRenderer({ content: `Cito · ${action} ${context.isError ? "error" : "result"}\n${body}` }, { ...options, outputPad: context.outputPad }, theme);
      },
    };
  });
  const resolvedStateDir = expandHome(process.env.CITO_STATE_DIR ?? "~/.cito");
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
  const session: SessionState = host[sessionKey] ??= { cwd: "", sessionName: "", about: "", batches: new Map() };
  let commandGen = 0;
  let activeBadge: string | undefined;
  let lastActiveMs = Date.now();
  let context: BridgeContext | undefined;
  let server: Server | undefined;
  let ownsSocket = false;
  const connections = new Set<Socket>();
  const directory = join(resolvedStateDir, "sockets");
  const socketPath = join(directory, `pi-pid-${process.pid}.sock`);
  const piHome = expandHome(process.env.CITO_PI_HOME ?? "~/.pi/agent");
  const subcommands = ["on", "private", "team", "off"];
  const commands = new Map<string, Promise<void>>();
  const hubPattern = /^[a-z0-9][a-z0-9_-]{0,31}$/;
  const usage = "Usage: /cito [on [about...] | team [about...] | off]";
  const hubUsage = "Usage: /cito [on [--hub <hub>] [about...] | team [--hub <hub>] [about...] | off [--hub <hub>]]";

  async function callMcpTool(name: "cito_listen" | "cito_unlisten", args: Record<string, unknown>, expectedSessionId: string | undefined): Promise<Record<string, unknown>> {
    let config: unknown = {};
    const rawConfig = process.env.CITO_CONFIG_PATH ?? join(resolvedStateDir, "config.json");
    const configPath = expandHome(rawConfig);
    try { config = JSON.parse(readFileSync(configPath, "utf8")); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
        throw new Error(`Cannot read Cito config.json: ${error instanceof SyntaxError ? "Invalid JSON" : "Read failed"}`);
      }
    } // Only a missing config falls back to the environment and default port.
    if (!config || typeof config !== "object" || Array.isArray(config)) throw new Error("Cito config.json must be an object");
    const validPort = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value > 0 && value <= 65535;
    const configuredPort = (config as { daemonPort?: unknown }).daemonPort;
    if (configuredPort !== undefined && !validPort(configuredPort)) throw new Error("Invalid daemonPort in Cito config.json");
    const rawPort = process.env.CITO_DAEMON_PORT?.trim() ?? "";
    const envPort = /^\+?\d+$/.test(rawPort) ? Number(rawPort) : undefined;
    const port = validPort(envPort) ? envPort : configuredPort ?? 4290;
    const res = await fetch(`http://127.0.0.1:${port}/mcp`, {
      method: "POST",
      signal: AbortSignal.timeout(15_000),
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
          _meta: { endpoint_pid: process.pid, expected_session_id: expectedSessionId },
        },
      }),
    });
    const raw = await res.text();
    const dataLines = raw.split(/\r?\n/).filter(line => line.startsWith("data:")).map(line => line.slice(5).trim()).filter(Boolean);
    const jsonText = dataLines.at(-1) ?? (raw.trim().startsWith("{") ? raw.trim() : "");
    if (!jsonText) throw new Error(raw.trim() || `HTTP ${res.status}`);
    const rpc = JSON.parse(jsonText) as { error?: { message?: unknown }; result?: { isError?: boolean; content?: { type?: string; text?: string }[] } };
    if (rpc.error) throw new Error(typeof rpc.error.message === "string" ? rpc.error.message : JSON.stringify(rpc.error));
    const content = rpc.result?.content;
    const text = Array.isArray(content) ? content.find(part => part?.type === "text" && typeof part.text === "string")?.text : undefined;
    let payload: Record<string, unknown> | undefined;
    if (text) {
      try {
        const parsed = JSON.parse(text);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed) && Object.keys(parsed).length) payload = parsed;
      } catch { /* Plain text can carry an error, never a success receipt. */ }
    }
    if (rpc.result?.isError || payload?.ok === false || !res.ok) {
      const errObj = payload?.error as { message?: unknown } | string | undefined;
      const msg = typeof errObj === "object" && errObj && typeof errObj.message === "string"
        ? errObj.message : typeof errObj === "string" ? errObj : text || `HTTP ${res.status}`;
      throw new Error(msg);
    }
    if (!payload || (name === "cito_listen" && !(typeof payload.listener_id === "string" && payload.listener_id.trim()
      && typeof payload.scope === "string" && payload.scope.trim()))) throw new Error(`Invalid ${name} receipt`);
    return payload;
  }

  function update(ctx: BridgeContext): void {
    const next = ctx.sessionManager.getSessionId();
    if (next?.trim() && next !== session.sessionId) {
      session.sessionId = next;
      session.about = "";
      activeBadge = undefined;
      commandGen++;
      lastActiveMs = Date.now();
    }
    if (next?.trim()) {
      session.cwd = ctx.cwd;
      session.sessionName = ctx.sessionManager.getSessionName() ?? "";
      if (!session.about) session.about = promptAbout(ctx.sessionManager);
      const delivered = session.batches.get(next) ?? new Set<string>();
      session.batches.delete(next);
      session.batches.set(next, delivered);
      // Keep recent inactive sessions for deduplication without unbounded growth.
      while (session.batches.size > 128) session.batches.delete(session.batches.keys().next().value!);
    }
    context = ctx;
    renderBadge(ctx);
  }
  let renderedBadge: string | undefined;
  let renderedContext: BridgeContext | undefined;
  function renderBadge(ctx: BridgeContext | undefined): void {
    if (ctx && (ctx !== renderedContext || renderedBadge !== activeBadge)) {
      ctx.ui.setStatus("cito", activeBadge);
      renderedContext = ctx;
      renderedBadge = activeBadge;
    }
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
    if (req.op !== "identify" && req.op !== "status" && req.op !== "deliver") return { ok: false, error: "INVALID_REQUEST" };
    const currentId = session.sessionId;
    if (!currentId) return { ok: false, error: "SESSION_NOT_READY" };
    if (req.op === "identify") return { ok: true, endpoint_pid: process.pid, pi_pid: process.pid,
      session_id: currentId, session_name: session.sessionName, cwd: session.cwd, about: session.about, badge: activeBadge ?? null, updated_at_ms: lastActiveMs };
    if (req.session_id !== currentId) return { ok: false, error: "SESSION_MISMATCH" };
    if (req.op === "status") {
      if (req.badge != null && typeof req.badge !== "string") return { ok: false, error: "INVALID_REQUEST" };
      activeBadge = (req.badge as string | undefined) ?? undefined;
      renderBadge(ctx);
      return { ok: true };
    }
    const processedBatchIds = session.batches.get(currentId)!;
    if (typeof req.batch_id !== "string" || !req.batch_id) return { ok: false, error: "INVALID_REQUEST" };
    if (processedBatchIds.has(req.batch_id)) return { ok: true, delivered: true, deduplicated: true };
    if (typeof req.content !== "string" || typeof req.wake !== "boolean") return { ok: false, error: "INVALID_REQUEST" };
    // Pi 1.5.0 sendMessage is synchronous. No await between the effect and recording its ID.
    pi.sendMessage({ customType: "cito-notification", content: req.content, display: true },
      { triggerTurn: req.wake, deliverAs: req.wake ? "steer" : "followUp" });
    lastActiveMs = Date.now();
    processedBatchIds.add(req.batch_id);
    if (processedBatchIds.size > 200) processedBatchIds.delete(processedBatchIds.values().next().value!);
    return { ok: true, delivered: true, deduplicated: false };
  }

  const headless = process.argv.some(arg => ["-p", "--print", "--no-session", "--mode=rpc", "--mode=json"].includes(arg))
    || process.argv.some((arg, i) => arg === "--mode" && ["rpc", "json"].includes(process.argv[i + 1]));
  const discoveryAllowed = process.env.CITO_PI_ALLOW_HEADLESS_DISCOVERY === "1" || !headless && !!process.stdin.isTTY;
  const initialRepairDelay = 50;
  let repairDelay = initialRepairDelay;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let repairing = false;
  let serverReady: Promise<void> | undefined;
  let shuttingDown = false;
  function ensureServer(): Promise<void> {
    return serverReady ??= startServer().catch(error => { serverReady = undefined; throw error; });
  }
  async function startServer(): Promise<void> {
    if (server || shuttingDown || !discoveryAllowed) return;
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
      repairDelay = initialRepairDelay;
      clearTimeout(retry);
      retry = undefined;
      // Listen errors after startup must not become unhandled process errors.
      instance.on("error", () => { for (const socket of connections) socket.destroy(); });
    } catch (error) {
      server = undefined;
      instance.close();
      unlinkSocket();
      throw error;
    }
  }
  process.once("exit", unlinkSocket);
  // Load-time startup also serves hosts that loaded the extension after session_start.
  void ensureServer().catch(() => {}); // session_start retries and surfaces failures.
  function repairMissingSocket(): void {
    if (shuttingDown || repairing || retry || existsSync(socketPath)) return;
    let configured = false;
    try {
      const settings = JSON.parse(readFileSync(join(piHome, "settings.json"), "utf8")) as { packages?: unknown[] };
      configured = settings.packages?.some(entry => {
        if (typeof entry !== "string") return false;
        const directory = expandHome(entry);
        return directory === join(resolvedStateDir, "runtime", "plugins", "pi")
          || directory === join(resolvedStateDir, "plugins", "pi")
          || existsSync(join(directory, "cito-bridge.ts"));
      }) === true;
    } catch {} // Disconnected packages must not recreate their sockets.
    if (!configured) return;
    repairing = true;
    void (async () => {
      await serverReady?.catch(() => {});
      if (shuttingDown || existsSync(socketPath)) return;
      const stale = server;
      server = undefined;
      ownsSocket = false;
      for (const socket of connections) socket.destroy();
      if (stale) await new Promise<void>(resolve => stale.close(() => resolve()));
      if (shuttingDown) return;
      serverReady = undefined;
      await ensureServer();
    })().catch(() => {}).finally(() => {
      repairing = false;
      // Recheck even when the last unlink was delivered during bind/chmod.
      // Back off on failed binds; no timer is armed for a healthy socket.
      if (!shuttingDown && !existsSync(socketPath) && !retry) {
        retry = setTimeout(() => { retry = undefined; repairMissingSocket(); }, repairDelay).unref();
        repairDelay = Math.min(repairDelay * 2, 5000);
      }
    });
  }
  let repairWatcher: ReturnType<typeof watch> | undefined;
  function watchForRepair(): void {
    if (!discoveryAllowed || repairWatcher) return;
    try {
      repairWatcher = watch(directory, { persistent: false }, repairMissingSocket).on("error", () => {
        repairWatcher?.close();
        repairWatcher = undefined;
        repairMissingSocket();
      });
    } catch {} // Retry acquisition on the next session start.
  }
  watchForRepair();
  pi.on("session_start", async (_event, ctx) => {
    registerMcpServer();
    lastActiveMs = Date.now();
    update(ctx);
    await ensureServer();
    if (!shuttingDown && discoveryAllowed) {
      watchForRepair();
      // A filesystem watcher can coalesce or miss an unlink. A host lifecycle
      // event is also a repair opportunity; never poll a healthy, idle session.
      repairMissingSocket();
    }
  });
  pi.registerCommand?.("cito", {
    description: "Toggle or configure Cito session listening",
    getArgumentCompletions(prefix) {
      const trimmed = prefix.trim();
      if (!/\s/.test(trimmed)) {
        return subcommands.filter(cmd => cmd.startsWith(trimmed)).map(value => ({ value, label: value }));
      }
      const match = /^(on|private|team|off)\s+(\S*)$/.exec(trimmed);
      if (match && match[2]!.startsWith("-") && "--hub".startsWith(match[2]!)) {
        return [{ value: `${match[1]} --hub `, label: "--hub <hub>" }];
      }
      return [];
    },
    async handler(args, ctx) {
      if (shuttingDown) return;
      lastActiveMs = Date.now();
      update(ctx);
      const id = session.sessionId;
      if (!id) { ctx.ui.notify?.("Session not ready", "warning"); return; }
      const epoch = commandGen;
      const run = async () => {
        if (!shuttingDown && session.sessionId === id && epoch === commandGen) await execute(args, ctx, id, epoch);
      };
      const previous = commands.get(id);
      const next = previous ? previous.then(run, run) : run();
      commands.set(id, next);
      try { await next; }
      finally { if (commands.get(id) === next) commands.delete(id); }
    },
  });
  async function execute(args: string, ctx: BridgeContext, commandSessionId: string, generation: number) {
      const trimmed = args.trim();
      let tool: "cito_listen" | "cito_unlisten";
      let toolArgs: Record<string, unknown>;
      let targetHub: string | undefined;
      const about = session.about || "Pi session";
      if (!trimmed) {
        if (activeBadge) {
          tool = "cito_unlisten";
          toolArgs = {};
        } else {
          tool = "cito_listen";
          toolArgs = { about, scope: "self" };
        }
      } else {
        const space = trimmed.search(/\s/);
        const token = space < 0 ? trimmed : trimmed.slice(0, space);
        const sub = token === "private" ? "on" : token;
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
          toolArgs = { about: rest || about, scope: "self", ...(targetHub ? { hub: targetHub } : {}) };
        } else if (sub === "team") {
          tool = "cito_listen";
          toolArgs = { about: rest || about, scope: "workspace", ...(targetHub ? { hub: targetHub } : {}) };
        } else {
          if (rest) {
            ctx.ui.notify?.(targetHub ? hubUsage : usage, "warning");
            return;
          }
          tool = "cito_unlisten";
          toolArgs = targetHub ? { hub: targetHub } : {};
        }
      }
      try {
        const payload = await callMcpTool(tool, toolArgs, commandSessionId);
        if (shuttingDown || session.sessionId !== commandSessionId || generation !== commandGen) return;
        if (tool === "cito_listen") {
          const publicId = String(payload.listener_id).trim().split(":").at(-1)!;
          const slug = publicId.includes("-") ? publicId.slice(0, publicId.lastIndexOf("-")) : publicId;
          const short = Array.from(slug).slice(0, 12).join("").replace(/-+$/, "") || "cito";
          activeBadge ??= `Cito · ${short}`;
          ctx.ui.notify?.(`Cito listening as ${String(payload.listener_id)} (${String(payload.scope)})`, "info");
        } else if (targetHub) {
          ctx.ui.notify?.(`Cito stopped listening on ${targetHub}`, "info");
        } else {
          activeBadge = undefined;
          ctx.ui.notify?.("Cito stopped listening", "info");
        }
      } catch (error) {
        if (shuttingDown || session.sessionId !== commandSessionId || generation !== commandGen) return;
        ctx.ui.notify?.(error instanceof Error ? error.message : String(error), "error");
      }
      renderBadge(ctx);
  }
  pi.on("session_info_changed", (_event, ctx) => { lastActiveMs = Date.now(); update(ctx); });
  pi.on("session_tree", (_event, ctx) => { lastActiveMs = Date.now(); session.about = ""; update(ctx); });
  pi.on("session_shutdown", async event => {
    if ((event as { reason?: string })?.reason !== "reload") delete host[sessionKey];
    shuttingDown = true;
    commandGen++;
    repairWatcher?.close();
    clearTimeout(retry);
    await serverReady?.catch(() => {});
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
