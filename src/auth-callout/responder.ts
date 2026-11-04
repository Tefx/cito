import { decode, encodeAuthorizationResponse, encodeUser, type AuthorizationRequest } from "@nats-io/jwt";
import { type Msg, type NatsConnection } from "nats";
import { fromPublic } from "nkeys.js";
import { z } from "zod";
import { CitoError } from "../shared/errors.js";
import { connectNats, provisionNats } from "../shared/nats.js";
import { oidcPrincipal } from "../shared/identity.js";
import type { AuthCalloutConfig } from "./config.js";
import { OidcVerifier } from "./oidc.js";

export const USER_PERMISSIONS = [
  "$JS.API.>", "$JS.ACK.>", "$KV.cito_listeners.>",
  "cito.signal.>", "cito.ack.>", "_INBOX.>",
];
const requestSchema = z.object({
  user_nkey: z.string().startsWith("U"),
  server_id: z.object({ id: z.string().startsWith("N"), xkey: z.string().startsWith("X").optional() }).passthrough(),
  connect_opts: z.object({ auth_token: z.string().optional() }).passthrough(),
}).passthrough();
type CalloutRequest = z.infer<typeof requestSchema>;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** Decodes and verifies the server-signed request; never trusts an unsigned JWT body. */
export function decodeRequest(data: Uint8Array, config: AuthCalloutConfig, serverXkey?: string): CalloutRequest {
  if (config.xkey) {
    if (!serverXkey) throw new Error("Encrypted callout request is missing Nats-Server-Xkey");
    const opened = config.xkey.open(data, serverXkey);
    if (!opened) throw new Error("Unable to decrypt auth callout request");
    data = opened;
  }
  const claims = decode<AuthorizationRequest>(decoder.decode(data));
  const req = requestSchema.parse(claims.nats);
  if (claims.nats.type !== "authorization_request" || claims.nats.version !== 2
    || claims.iss !== req.server_id.id || !claims.exp || claims.exp <= Date.now() / 1000) {
    throw new Error("Invalid server authorization request claims");
  }
  fromPublic(req.user_nkey);
  fromPublic(req.server_id.id);
  if (config.xkey && req.server_id.xkey !== serverXkey) throw new Error("Server XKey mismatch");
  return req;
}

export async function authorizationResponse(req: CalloutRequest, config: AuthCalloutConfig, verifier?: OidcVerifier): Promise<Uint8Array> {
  let response: { jwt: string } | { error: string };
  try {
    if (!verifier || config.authMode !== "oidc") throw new CitoError("UNAUTHORIZED", "OIDC auth callout is disabled");
    if (!req.connect_opts.auth_token) throw new CitoError("UNAUTHORIZED", "OIDC id_token is required");
    const claims = await verifier.verify(req.connect_opts.auth_token);
    const principal = oidcPrincipal(claims);
    const jwt = await encodeUser(principal.principal_id, req.user_nkey, config.issuer, {
      pub: { allow: [...USER_PERMISSIONS] },
      sub: { allow: [...USER_PERMISSIONS] },
    }, { aud: "CITO", exp: claims.exp });
    response = { jwt };
  } catch (error) {
    const code = error instanceof CitoError ? error.code : "UNAUTHORIZED";
    // No unverified token text, seed, network URL credentials or raw exception in replies.
    response = { error: code === "AUTH_UNAVAILABLE" ? "AUTH_UNAVAILABLE: retry authentication later" : "UNAUTHORIZED: OIDC token rejected" };
  }
  const jwt = await encodeAuthorizationResponse(req.user_nkey, req.server_id.id, config.issuer,
    response, { exp: Math.floor(Date.now() / 1000) + 30 });
  const bytes = encoder.encode(jwt);
  if (config.xkey) {
    if (!req.server_id.xkey) throw new Error("Missing server response XKey");
    return config.xkey.seal(bytes, req.server_id.xkey);
  }
  return bytes;
}

export interface AuthCalloutService {
  connection: NatsConnection;
  stop(): Promise<void>;
}
export async function startAuthCallout(config: AuthCalloutConfig, options: {
  verifier?: OidcVerifier;
  onError?: (error: Error) => void;
} = {}): Promise<AuthCalloutService> {
  const nc = await connectNats({
    servers: config.natsUrl,
    ...(config.authMode === "oidc" ? { user: config.serviceUser, pass: config.servicePassword } : {}),
    name: "cito-auth-callout",
    timeout: 5000,
    maxReconnectAttempts: -1,
  });
  try {
    await provisionNats(nc);
    const verifier = options.verifier ?? (config.authMode === "oidc" ? new OidcVerifier({
      issuer: config.oidcIssuer!, clientId: config.oidcClientId!,
    }) : undefined);
    const pending = new Set<Promise<void>>();
    async function handle(msg: Msg): Promise<void> {
      try {
        const request = decodeRequest(msg.data, config, msg.headers?.get("Nats-Server-Xkey"));
        msg.respond(await authorizationResponse(request, config, verifier));
      } catch {
        // An undecodable request cannot safely supply response JWT addresses.
        // An empty response makes the broker reject this connection immediately.
        msg.respond(new Uint8Array());
        options.onError?.(new Error("Malformed auth callout request rejected"));
      }
    }
    const sub = nc.subscribe("$SYS.REQ.USER.AUTH", {
      queue: "cito-auth-callout",
      callback: (error, msg) => {
        if (error) { options.onError?.(error); return; }
        const task = handle(msg);
        pending.add(task);
        void task.finally(() => pending.delete(task)).catch(() => {
          options.onError?.(new Error("Auth callout response delivery failed"));
        });
      },
    });
    await nc.flush();
    let stopping: Promise<void> | undefined;
    return {
      connection: nc,
      stop() {
        return stopping ??= (async () => {
          sub.unsubscribe();
          await Promise.allSettled([...pending]);
          if (!nc.isClosed()) await nc.drain();
        })();
      },
    };
  } catch (error) {
    await nc.close();
    throw error;
  }
}
