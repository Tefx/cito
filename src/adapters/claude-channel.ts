import { unlinkSync } from "node:fs";
import { chmod, mkdir, unlink } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import { basename, dirname } from "node:path";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import type { CitoConfig } from "../shared/index.js";
import { CLAUDE_HARNESS, appendClaudeWake, resolveClaudeSessionId, validateClaudeSessionId, writeClaudeBadge } from "./claude.js";
import { bridgeSocketPath, requestBridge } from "./pi.js";

const MAX_REQUEST_BYTES = 4 * 1024 * 1024;
const MAX_TRACKED_BATCHES = 200;

export interface ClaudeChannelBridge {
  readonly socketPath: string;
  sessionId(): Promise<string>;
  /** Only the initialize result gains the channel capability; tool results pass through unchanged. */
  forward(message: JSONRPCMessage): JSONRPCMessage;
  close(): Promise<void>;
}
export interface ClaudeChannelOptions {
  endpointPid?: number;
  cwd?: string;
  env?: NodeJS.ProcessEnv;
}

/**
 * Host half of the Claude Code adapter. `cito-mcp` runs as a stdio MCP server inside Claude Code, so
 * its parent PID is the session endpoint the daemon binds to. Delivery appends to the session's wake
 * queue, which `cito-claude-hook` drains at the next turn boundary; the MCP notification is only a
 * live hint for an attached client and is never the durable half.
 */
export async function startClaudeChannelBridge(config: CitoConfig,
  transport: { send(message: JSONRPCMessage): Promise<void> },
  options: ClaudeChannelOptions = {}): Promise<ClaudeChannelBridge> {
  const env = options.env ?? process.env;
  const endpoint = options.endpointPid ?? process.ppid;
  const cwd = options.cwd ?? process.cwd();
  const path = bridgeSocketPath(config.stateDir, CLAUDE_HARNESS, endpoint);
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  await chmod(dirname(path), 0o700);
  const delivered = new Set<string>(), clients = new Set<Socket>();
  let queue = Promise.resolve(), owned = false, closed = false, lastActiveMs = Date.now(), badged: string | undefined;
  // Re-resolved per request: /clear starts a new transcript, which must read as a switched session.
  const sessionId = async (): Promise<string> => {
    const explicit = env.CLAUDE_SESSION_ID?.trim();
    if (explicit) return validateClaudeSessionId(explicit);
    return await resolveClaudeSessionId(cwd, config) ?? `claude-${endpoint}`;
  };
  const handle = async (request: Record<string, unknown>): Promise<object> => {
    if (closed) return { ok: false, error: "BRIDGE_CLOSED" };
    const current = await sessionId();
    if (request.op === "identify") {
      return { ok: true, endpoint_pid: endpoint, session_id: current,
        session_name: basename(cwd), cwd, updated_at_ms: lastActiveMs };
    }
    if (request.session_id !== current) return { ok: false, error: "SESSION_MISMATCH" };
    if (request.op === "status") {
      if (request.badge != null && typeof request.badge !== "string") return { ok: false, error: "INVALID_REQUEST" };
      badged = (request.badge as string | undefined) ?? undefined;
      await writeClaudeBadge(config, current, badged);
      return { ok: true };
    }
    if (request.op !== "deliver" || typeof request.batch_id !== "string" || !request.batch_id
      || typeof request.content !== "string") return { ok: false, error: "INVALID_REQUEST" };
    const deduplicated = delivered.has(request.batch_id);
    if (!deduplicated) {
      await appendClaudeWake(config, current, { batch_id: request.batch_id, content: request.content,
        wake: request.wake !== false, created_at_ms: Date.now() });
      if (closed) return { ok: false, error: "BRIDGE_CLOSED" };
      delivered.add(request.batch_id);
      if (delivered.size > MAX_TRACKED_BATCHES) delivered.delete(delivered.values().next().value!);
      lastActiveMs = Date.now();
      // A client without the channel capability simply ignores the notification.
      await transport.send({ jsonrpc: "2.0", method: "notifications/claude/channel",
        params: { content: request.content, meta: { origin: "cito", batch_id: request.batch_id } } }).catch(() => {});
    }
    return { ok: true, delivered: true, deduplicated };
  };
  const server = createServer(socket => {
    clients.add(socket); socket.once("close", () => clients.delete(socket));
    socket.on("error", () => socket.destroy()); socket.setTimeout(3000, () => socket.destroy());
    let buffer = Buffer.alloc(0), received = false;
    socket.on("data", chunk => {
      if (received) return;
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length > MAX_REQUEST_BYTES) { socket.destroy(); return; }
      const end = buffer.indexOf(10); if (end < 0) return;
      received = true;
      queue = queue.then(async () => {
        try {
          const request: unknown = JSON.parse(buffer.subarray(0, end).toString("utf8"));
          if (!request || typeof request !== "object" || Array.isArray(request)) throw new Error("Invalid request");
          socket.end(JSON.stringify(await handle(request as Record<string, unknown>)) + "\n");
        } catch { socket.end(JSON.stringify({ ok: false, error: "DELIVERY_FAILED" }) + "\n"); }
      });
    });
  });
  const listen = () => new Promise<void>((resolve, reject) => {
    const error = (cause: Error) => { server.removeListener("listening", ready); reject(cause); };
    const ready = () => { server.removeListener("error", error); resolve(); };
    server.once("error", error); server.once("listening", ready); server.listen(path);
  });
  const onExit = () => { if (owned) { try { unlinkSync(path); } catch { /* Best effort on exit. */ } } };
  try {
    try { await listen(); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EADDRINUSE") throw error;
      // Reclaim only an abandoned socket. A live bridge retains its endpoint.
      try { await requestBridge(path, { op: "identify" }, 500); throw error; }
      catch (probe) {
        if (!["ENOENT", "ECONNREFUSED"].includes(((probe as Error).cause as NodeJS.ErrnoException)?.code ?? "")) throw error;
      }
      await unlink(path).catch(error => { if (error.code !== "ENOENT") throw error; });
      await listen();
    }
    owned = true; await chmod(path, 0o600); process.once("exit", onExit);
    // A listen error after startup must not become an unhandled process error.
    server.on("error", () => { for (const client of clients) client.destroy(); });
  } catch (error) { server.close(); onExit(); throw error; }
  let closing: Promise<void> | undefined;
  return {
    socketPath: path,
    sessionId,
    forward(message: JSONRPCMessage): JSONRPCMessage {
      if ("result" in message && message.result && typeof message.result === "object" && "protocolVersion" in message.result) {
        const capabilities = message.result.capabilities as Record<string, unknown> | undefined;
        return { ...message, result: { ...message.result, capabilities: { ...capabilities,
          experimental: { ...capabilities?.experimental as object, "claude/channel": {} } } } };
      }
      return message;
    },
    close(): Promise<void> {
      return closing ??= (async () => {
        closed = true;
        for (const client of clients) client.destroy();
        await new Promise<void>(resolve => server.close(() => resolve()));
        // A stale badge outlives the session it described, so drop it with the bridge.
        if (badged !== undefined) await writeClaudeBadge(config, await sessionId(), undefined).catch(() => {});
        // A blocked stdout drain must not retain the socket or delay shutdown.
        onExit(); owned = false; process.removeListener("exit", onExit);
      })();
    },
  };
}
