import { createServer, type Socket } from "node:net";
import { chmod, mkdir, unlink } from "node:fs/promises";
import { unlinkSync } from "node:fs";
import { basename, dirname } from "node:path";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import type { CitoConfig } from "../shared/index.js";
import { bridgeSocketPath, requestBridge } from "./pi.js";

/** Experimental stdio channel transport; native host lifecycle hooks are not installed. */
export async function startStdioChannelBridge(config: CitoConfig, transport: { send(message: JSONRPCMessage): Promise<void> }) {
  const endpoint = process.ppid, session = process.env.CLAUDE_SESSION_ID || `claude-${endpoint}`;
  const cwd = process.cwd(), path = bridgeSocketPath(config.stateDir, "claude", endpoint);
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  await chmod(dirname(path), 0o700);
  const delivered = new Set<string>(), clients = new Set<Socket>();
  let queue = Promise.resolve(), owned = false, closed = false;
  const handle = async (request: Record<string, unknown>): Promise<object> => {
    if (closed) return { ok: false, error: "BRIDGE_CLOSED" };
    if (request.op === "identify") return { ok: true, endpoint_pid: endpoint, session_id: session, session_name: basename(cwd), cwd };
    if (request.session_id !== session) return { ok: false, error: "SESSION_MISMATCH" };
    if (request.op === "status") return { ok: true };
    if (request.op !== "deliver" || typeof request.batch_id !== "string" || !request.batch_id
      || typeof request.content !== "string") return { ok: false, error: "INVALID_REQUEST" };
    const deduplicated = delivered.has(request.batch_id);
    if (!deduplicated) {
      await transport.send({ jsonrpc: "2.0", method: "notifications/claude/channel",
        params: { content: request.content, meta: { origin: "cito" } } });
      if (closed) return { ok: false, error: "BRIDGE_CLOSED" };
      delivered.add(request.batch_id);
      if (delivered.size > 200) delivered.delete(delivered.values().next().value!);
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
      if (buffer.length > 4 * 1024 * 1024) { socket.destroy(); return; }
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
  const onExit = () => { if (owned) { try { unlinkSync(path); } catch {} } };
  try {
    try { await listen(); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EADDRINUSE") throw error;
      // Reclaim only an abandoned socket. A live forwarder retains its endpoint.
      try { await requestBridge(path, { op: "identify" }, 500); throw error; }
      catch (probe) {
        if (!["ENOENT", "ECONNREFUSED"].includes(((probe as Error).cause as NodeJS.ErrnoException)?.code ?? "")) throw error;
      }
      await unlink(path).catch(error => { if (error.code !== "ENOENT") throw error; });
      await listen();
    }
    owned = true; await chmod(path, 0o600); process.once("exit", onExit);
  } catch (error) { server.close(); onExit(); throw error; }
  let closing: Promise<void> | undefined;
  return {
    /** Only the initialize result gains channel capability; tool results pass through unchanged. */
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
        // A blocked stdout drain must not retain the socket or delay shutdown.
        onExit(); owned = false; process.removeListener("exit", onExit);
      })();
    },
  };
}
