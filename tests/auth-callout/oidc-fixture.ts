import { createServer } from "node:http";
import { generateKeyPair, exportJWK, SignJWT, type JWK, type JWTPayload } from "jose";

export async function signingKey(alg: "RS256" | "ES256", kid: string) {
  const pair = await generateKeyPair(alg, { extractable: true });
  const jwk = { ...await exportJWK(pair.publicKey), kid, alg, use: "sig" };
  return {
    jwk,
    sign(payload: JWTPayload, header: Record<string, unknown> = {}) {
      return new SignJWT(payload).setProtectedHeader({ alg, kid, ...header }).sign(pair.privateKey);
    },
  };
}
export async function startOidc(keys: JWK[] = []) {
  const state = { keys, fail: false, stall: false, discoveryRequests: 0, jwksRequests: 0 };
  let issuer = "";
  const server = createServer((req, res) => {
    if (req.url === "/.well-known/openid-configuration") {
      state.discoveryRequests++;
      if (state.stall) return;
      if (state.fail) { res.writeHead(503).end(); return; }
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ issuer, jwks_uri: issuer + "/jwks" }));
    } else if (req.url === "/jwks") {
      state.jwksRequests++;
      if (state.fail) { res.writeHead(503).end(); return; }
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ keys: state.keys }));
    } else { res.writeHead(404).end(); }
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No OIDC fixture port");
  issuer = "http://127.0.0.1:" + address.port;
  return {
    issuer, state,
    claims(overrides: JWTPayload = {}): JWTPayload {
      return { iss: issuer, sub: "user-1", preferred_username: "Alice", aud: "cito", exp: Math.floor(Date.now() / 1000) + 600, ...overrides };
    },
    async stop() {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    },
  };
}
