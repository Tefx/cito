import { createHash, timingSafeEqual } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { OidcVerifier } from "../auth-callout/oidc.js";
import type { HubAuthMode } from "../shared/config.js";
import { CitoError } from "../shared/errors.js";
import { localPrincipal, oidcPrincipal, type Principal } from "../shared/identity.js";
import {
  HubEngine,
  HubError,
  type HubAuditEvent,
  type HubKvEvent,
  type HubNcEvent,
  type HubSignalEvent,
} from "./engine.js";

export interface HubServerOptions {
  port?: number;
  bind?: string;
  authMode?: HubAuthMode;
  token?: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  verifier?: OidcVerifier;
  dataDir?: string;
  auditLogPath?: string;
  onAudit?: (event: HubAuditEvent) => void;
  onError?: (error: Error) => void;
  engine?: HubEngine;
}

export interface HubServer {
  url: string;
  wsUrl: string;
  port: number;
  bind: string;
  directory: string;
  env: NodeJS.ProcessEnv;
  engine: HubEngine;
  close(): Promise<void>;
  stop(): Promise<void>;
}

const MAX_RPC_BODY_BYTES = 8 * 1024 * 1024;

function extractBearerToken(req: IncomingMessage): string | undefined {
  const auth = req.headers.authorization;
  if (typeof auth === "string") {
    const match = /^Bearer\s+(.+)$/i.exec(auth.trim());
    if (match?.[1]) return match[1].trim();
  }
  const custom = req.headers["x-cito-token"];
  if (typeof custom === "string" && custom.trim()) return custom.trim();
  return undefined;
}

function safeUserHint(req: IncomingMessage): string {
  const raw = req.headers["x-cito-user"];
  const val = typeof raw === "string" ? raw.trim() : "";
  if (val && val.length <= 64 && /^[a-zA-Z0-9._-]+$/.test(val)) {
    return val;
  }
  return "local";
}

function tokensMatch(expected: string, actual: string): boolean {
  const a = createHash("sha256").update(expected, "utf8").digest();
  const b = createHash("sha256").update(actual, "utf8").digest();
  return timingSafeEqual(a, b);
}

async function readBody(req: IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  let total = 0;
  for await (const chunk of req) {
    const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    total += buf.length;
    if (total > MAX_RPC_BODY_BYTES) {
      throw new HubError(413, "INVALID_INPUT", "RPC request body too large");
    }
    chunks.push(buf);
  }
  return Buffer.concat(chunks).toString("utf8");
}

export async function startHubServer(options: HubServerOptions = {}): Promise<HubServer> {
  const authMode: HubAuthMode = options.authMode ?? "none";
  const bind = options.bind ?? "127.0.0.1";
  const requestedPort = options.port ?? 0;

  if (authMode === "token" && !options.token?.trim()) {
    throw new Error("CITO_HUB_TOKEN is required in token mode");
  }
  if (authMode === "oidc" && !options.verifier && (!options.oidcIssuer?.trim() || !options.oidcClientId?.trim())) {
    throw new Error("CITO_OIDC_ISSUER and CITO_OIDC_CLIENT_ID are required in oidc mode");
  }

  const verifier = options.verifier ?? (
    authMode === "oidc"
      ? new OidcVerifier({ issuer: options.oidcIssuer!, clientId: options.oidcClientId! })
      : undefined
  );

  const tempDir = options.dataDir ? undefined : await mkdtemp(join(tmpdir(), "cito-hub-"));
  const directory = options.dataDir ?? tempDir!;
  const engine = options.engine ?? new HubEngine({
    dataDir: options.dataDir,
    authMode,
    auditLogPath: options.auditLogPath,
    onAudit: options.onAudit,
  });

  const authenticate = async (req: IncomingMessage): Promise<Principal> => {
    if (authMode === "none") {
      return localPrincipal(safeUserHint(req));
    }
    const bearer = extractBearerToken(req);
    if (authMode === "token") {
      if (!bearer || !tokensMatch(options.token!.trim(), bearer)) {
        throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
      }
      return localPrincipal(safeUserHint(req));
    }
    if (!bearer || !verifier) {
      throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
    }
    try {
      const claims = await verifier.verify(bearer);
      return oidcPrincipal(claims);
    } catch (error) {
      if (error instanceof CitoError && error.code === "AUTH_UNAVAILABLE") {
        throw new HubError(503, "AUTH_UNAVAILABLE", "AUTH_UNAVAILABLE: retry authentication later");
      }
      throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
    }
  };

  const sseClients = new Set<{
    res: ServerResponse;
    principal: Principal;
    cleanup: () => void;
  }>();

  const server = createServer((req, res) => {
    void (async () => {
      const url = new URL(req.url ?? "/", "http://127.0.0.1");
      if (url.pathname === "/health" && req.method === "GET") {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ ok: true, auth_mode: authMode }));
        return;
      }

      if (url.pathname === "/v1/events" && req.method === "GET") {
        const principal = await authenticate(req);
        res.writeHead(200, {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-cache, no-transform",
          connection: "keep-alive",
        });
        res.flushHeaders();
        res.write(":ok\n\n");

        const sendEvent = (payload: unknown) => {
          if (res.writableEnded || res.destroyed) return;
          res.write(`data: ${JSON.stringify(payload)}\n\n`);
        };

        const onKv = (ev: HubKvEvent) => {
          if (!engine.canReadKvRow(ev, principal)) return;
          sendEvent({
            type: "kv",
            key: ev.key,
            revision: ev.revision,
            created_ms: ev.created_ms,
            value_b64: ev.value.toString("base64"),
            operation: ev.operation,
          });
        };

        const onSignal = (ev: HubSignalEvent) => {
          const targetId = ev.subject.startsWith("cito.signal.") ? ev.subject.slice("cito.signal.".length) : "";
          if (targetId && !engine.canConsumeSignal(targetId, principal)) return;
          sendEvent({
            type: "signal",
            seq: ev.seq,
            subject: ev.subject,
            created_ms: ev.created_ms,
            data_b64: ev.data.toString("base64"),
          });
        };

        const onNc = (ev: HubNcEvent) => {
          sendEvent({
            type: "nc",
            subject: ev.subject,
            data_b64: ev.data.toString("base64"),
          });
        };

        engine.events.on("kv", onKv);
        engine.events.on("signal", onSignal);
        engine.events.on("nc", onNc);

        let cleaned = false;
        const clientEntry = {
          res,
          principal,
          cleanup: () => {
            if (cleaned) return;
            cleaned = true;
            engine.events.off("kv", onKv);
            engine.events.off("signal", onSignal);
            engine.events.off("nc", onNc);
            sseClients.delete(clientEntry);
          },
        };
        sseClients.add(clientEntry);
        req.once("close", clientEntry.cleanup);
        res.once("close", clientEntry.cleanup);
        return;
      }

      if (url.pathname === "/v1/rpc" && req.method === "POST") {
        const principal = await authenticate(req);
        const raw = await readBody(req);
        let parsed: unknown;
        try {
          parsed = JSON.parse(raw);
        } catch {
          throw new HubError(400, "INVALID_INPUT", "Invalid JSON body");
        }
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
          throw new HubError(400, "INVALID_INPUT", "Expected JSON object");
        }
        const body = parsed as Record<string, unknown>;
        const op = typeof body.op === "string" ? body.op : "";
        const result = await engine.handleRpc(op, body, principal);
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ ok: true, result }));
        return;
      }

      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: false, error: { code: "NOT_FOUND", message: "Not found" } }));
    })().catch((error: unknown) => {
      if (error instanceof HubError) {
        if (!res.headersSent) {
          res.writeHead(error.status, { "content-type": "application/json" });
          res.end(
            JSON.stringify({
              ok: false,
              error: {
                code: error.code,
                message: error.message,
                ...(error.api_error ? { api_error: error.api_error } : {}),
              },
            }),
          );
          return;
        }
      }
      options.onError?.(error instanceof Error ? error : new Error(String(error)));
      if (!res.headersSent) {
        res.writeHead(500, { "content-type": "application/json" });
        res.end(JSON.stringify({ ok: false, error: { code: "INTERNAL_ERROR", message: String(error) } }));
      } else {
        res.end();
      }
    });
  });

  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(requestedPort, bind, () => {
        server.removeListener("error", reject);
        resolve();
      });
    });
  } catch (error) {
    if (!options.engine) engine.close();
    if (tempDir) await rm(tempDir, { recursive: true, force: true });
    throw error;
  }

  const addr = server.address();
  const actualPort = typeof addr === "object" && addr ? addr.port : requestedPort;
  const connectHost = bind === "0.0.0.0" || bind === "::" ? "127.0.0.1" : bind;
  const url = `http://${connectHost}:${actualPort}`;
  const wsUrl = `ws://${connectHost}:${actualPort}`;

  let stopPromise: Promise<void> | undefined;
  const close = (): Promise<void> =>
    (stopPromise ??= (async () => {
      for (const client of sseClients) {
        client.cleanup();
        try { client.res.end(); } catch {}
      }
      sseClients.clear();
      server.closeAllConnections();
      await new Promise<void>(resolve => server.close(() => resolve()));
      if (!options.engine) engine.close();
      if (tempDir) await rm(tempDir, { recursive: true, force: true });
    })());

  return {
    url,
    wsUrl,
    port: actualPort,
    bind,
    directory,
    env: {
      CITO_STATE_DIR: join(directory, "state"),
    },
    engine,
    close,
    stop: close,
  };
}
