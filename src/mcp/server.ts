import { createServer, type IncomingMessage } from "node:http";
import type { Socket } from "node:net";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import type { CallerDetectionContext } from "../adapters/index.js";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { SUPPORTED_PROTOCOL_VERSIONS, CallToolRequestSchema, ListToolsRequestSchema, type Tool } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { CITO_VERSION, CitoError, errorResult, type CitoConfig, listenInputSchema, unlistenInputSchema, findInputSchema, sendInputSchema, inboxInputSchema } from "../shared/index.js";
import { CitoService } from "./service.js";

if (!SUPPORTED_PROTOCOL_VERSIONS.includes("2026-07-28")) SUPPORTED_PROTOCOL_VERSIONS.unshift("2026-07-28");

// Derive the advertised schema from the same validation contract used by service calls.
const descriptors = [
  ["cito_listen", "Enroll this session to receive notifications.", listenInputSchema],
  ["cito_unlisten", "Revoke this session on one hub or every active hub.", unlistenInputSchema],
  ["cito_find", "Find active opted-in sessions across configured hubs. Caller binding is optional.", findInputSchema],
  ["cito_send", "Notify an exact listener, optionally prefixed with its hub. Caller binding is optional.", sendInputSchema],
  ["cito_inbox", "Read and drain this session's local inbox by default, including offline. Set ack_all:false to peek.", inboxInputSchema],
] as const;
export const tools: Tool[] = descriptors.map(([name, description, schema]) => ({
  name, description, inputSchema: z.toJSONSchema(schema, { io: "input", unrepresentable: "any" }) as Tool["inputSchema"],
}));
export function createMcpServer(service: CitoService, metadata: (meta: Record<string, unknown>) => CallerDetectionContext = meta => ({ meta })): Server {
  const server = new Server({ name: "cito-mcp", version: CITO_VERSION }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools }));
  server.setRequestHandler(CallToolRequestSchema, async request => {
    try {
      return { ...await service.call(request.params.name, request.params.arguments ?? {}, metadata(request.params._meta ?? {})) };
    } catch (error) {
      if (error instanceof CitoError) return { ...errorResult(error.code, error.message) };
      throw error;
    }
  });
  return server;
}
const exec = promisify(execFile);
const clientPids = new WeakMap<Socket, Promise<number | undefined>>();
export function parseClientPid(output: string, remotePort: number, daemonPort: number): number | undefined {
  let pid: number | undefined; const matches = new Set<number>();
  for (const line of output.split("\n")) {
    if (line.startsWith("p")) pid = Number(line.slice(1));
    if (line === `n127.0.0.1:${remotePort}->127.0.0.1:${daemonPort}` && pid && Number.isSafeInteger(pid) && pid > 0) matches.add(pid);
  }
  if (matches.size > 1) matches.delete(process.pid);
  return matches.size === 1 ? [...matches][0] : undefined;
}
function resolveHttpClientPid(req: IncomingMessage, daemonPort: number): Promise<number | undefined> {
  let result = clientPids.get(req.socket);
  if (!result) {
    result = (async () => {
      const remote = req.socket.remotePort; if (!remote) return;
      try {
        const { stdout } = await exec("/usr/sbin/lsof", ["-nP", `-iTCP:${remote}`, "-sTCP:ESTABLISHED", "-Fpn"], { timeout: 1000 });
        return parseClientPid(stdout, remote, daemonPort);
      } catch { return undefined; }
    })();
    clientPids.set(req.socket, result);
  }
  return result;
}
export function httpMetadata(meta: Record<string, unknown>, clientPid: number | undefined,
  headers?: Record<string, string | string[] | undefined>): CallerDetectionContext {
  return { meta, headers, httpTransport: true, httpClientPid: clientPid };
}
export async function startHttpServer(config: CitoConfig, service: CitoService) {
  const sessions = new Set<Server>();
  const http = createServer((req, res) => {
    void (async () => {
      if (req.headers.origin !== undefined) {
        let allowed = false;
        try { allowed = ["127.0.0.1", "localhost"].includes(new URL(req.headers.origin).hostname); } catch {}
        if (!allowed) { res.writeHead(403).end("Forbidden"); return; }
      }
      const path = new URL(req.url ?? "/", "http://127.0.0.1").pathname;
      if (path === "/health" && req.method === "GET") {
        res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ ok: true, pid: process.pid, stateDir: config.stateDir, version: CITO_VERSION })); return;
      }
      if (path !== "/mcp") { res.writeHead(404).end("Not found"); return; }
      const pid = await resolveHttpClientPid(req, (http.address() as { port: number }).port);
      const server = createMcpServer(service, meta => httpMetadata(meta, pid, req.headers));
      const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined });
      sessions.add(server);
      // IncomingMessage.close also fires when a POST body is consumed. Dispose on
      // response/socket close so the asynchronous tool response can finish first.
      res.once("close", () => { sessions.delete(server); void server.close().catch(console.error); });
      await server.connect(transport);
      await transport.handleRequest(req, res);
    })().catch(error => { console.error(error); if (!res.headersSent) res.writeHead(500); res.end(); });
  });
  await new Promise<void>((resolve, reject) => { http.once("error", reject); http.listen(config.daemonPort, "127.0.0.1", resolve); });
  return {
    http,
    port: (http.address() as { port: number }).port,
    async close() {
      await Promise.allSettled([...sessions].map(server => server.close()));
      http.closeAllConnections();
      await new Promise<void>(resolve => http.close(() => resolve()));
    },
  };
}
export async function daemonHealth(config: Pick<CitoConfig, "stateDir" | "daemonPort">): Promise<{ pid: number; stateDir: string; version?: string } | undefined> {
  try {
    const res = await fetch(`http://127.0.0.1:${config.daemonPort}/health`, { signal: AbortSignal.timeout(500) });
    const body = await res.json() as { ok?: boolean; pid: number; stateDir: string; version?: unknown };
    if (!res.ok || body.ok !== true || body.stateDir !== config.stateDir || !Number.isSafeInteger(body.pid)) return undefined;
    return { pid: body.pid, stateDir: body.stateDir, ...(typeof body.version === "string" ? { version: body.version } : {}) };
  } catch { return undefined; }
}
