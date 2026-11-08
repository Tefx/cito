import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { generateKeyPair, exportJWK, SignJWT, type JWK, type JWTPayload } from "jose";

/** Follow the mock provider through the real loopback PKCE callback. */
export const browser = async (url: string, signal: AbortSignal) => {
  const response = await fetch(url, { signal });
  if (!response.ok) throw new Error("Browser callback rejected");
};

export async function signingKey(alg: "RS256" | "RS384" | "RS512" | "PS256" | "ES256" | "ES384" | "EdDSA", kid: string) {
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
  const state = { scopesSupported: undefined as string[] | undefined, keys, fail: false, stall: false, discoveryRequests: 0, jwksRequests: 0,
    refreshStatus: 200, refreshRequests: 0, codeRequests: 0, authorizationRequests: 0,
    tokenResponse: {} as { id_token?: string; refresh_token?: string }, tokenFactory: undefined as undefined | ((nonce: string) => Promise<{id_token?:string;refresh_token?:string}>), nonce: "", requestedScopes: "", challenge: "", redirect: "" };
  let issuer = "";
  const server = createServer((req, res) => {
    if (req.url === "/.well-known/openid-configuration") {
      state.discoveryRequests++;
      if (state.stall) return;
      if (state.fail) { res.writeHead(503).end(); return; }
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ issuer, scopes_supported: state.scopesSupported, jwks_uri: issuer + "/jwks", token_endpoint: issuer + "/token", authorization_endpoint: issuer + "/authorize" }));
    } else if (req.url === "/jwks") {
      state.jwksRequests++;
      if (state.fail) { res.writeHead(503).end(); return; }
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ keys: state.keys }));
    } else if (req.url?.startsWith("/authorize?")) {
      const url = new URL(req.url, issuer);
      state.authorizationRequests++;
      state.nonce=url.searchParams.get("nonce")??"";state.requestedScopes=url.searchParams.get("scope")??"";
      state.challenge = url.searchParams.get("code_challenge") ?? "";
      state.redirect = url.searchParams.get("redirect_uri") ?? "";
      if (url.searchParams.get("code_challenge_method") !== "S256" || !state.challenge) { res.writeHead(400).end(); return; }
      const callback = new URL(state.redirect);
      callback.search = new URLSearchParams({ code: "fixture-code", state: url.searchParams.get("state") ?? "" }).toString();
      res.writeHead(302, { location: callback.toString() }).end();
    } else if (req.url === "/token" && req.method === "POST") {
      let body = "";
      req.setEncoding("utf8").on("data", chunk => { body += chunk; });
      req.on("end", async () => {
        const params = new URLSearchParams(body);
        if (params.get("grant_type") === "refresh_token") {
          state.refreshRequests++;
          if (state.refreshStatus !== 200) { res.writeHead(state.refreshStatus).end(); return; }
        } else {
          state.codeRequests++;
          const challenge = createHash("sha256").update(params.get("code_verifier") ?? "").digest("base64url");
          if (params.get("code") !== "fixture-code" || challenge !== state.challenge || params.get("redirect_uri") !== state.redirect) { res.writeHead(400).end(); return; }
        }
        res.setHeader("content-type", "application/json"); res.end(JSON.stringify(state.tokenFactory?await state.tokenFactory(state.nonce):state.tokenResponse));
      });
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
