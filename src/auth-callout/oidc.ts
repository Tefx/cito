import { createLocalJWKSet, decodeProtectedHeader, jwtVerify, type JSONWebKeySet, type JWTPayload } from "jose";
import { CitoError } from "../shared/errors.js";
import { normalizeIssuer, oidcPrincipal } from "../shared/identity.js";
import { validateHttpUrl } from "../shared/config.js";

export interface OidcVerifierOptions {
  issuer: string;
  clientId: string;
  fetch?: typeof globalThis.fetch;
  now?: () => number;
}
export type VerifiedOidcClaims = JWTPayload & { iss: string; sub: string; exp: number };

/** Shared by the callout and local MCP identity verification; no disk token/key cache. */
export class OidcVerifier {
  private readonly issuer: string;
  private readonly fetch: typeof globalThis.fetch;
  private readonly now: () => number;
  private keySet?: ReturnType<typeof createLocalJWKSet>;
  private keys: JSONWebKeySet["keys"] = [];
  private refreshAt = -Infinity;
  private refreshing?: Promise<void>;
  private unavailable = false;
  constructor(private readonly options: OidcVerifierOptions) {
    this.issuer = normalizeIssuer(validateHttpUrl(options.issuer, "OIDC issuer"));
    this.fetch = options.fetch ?? globalThis.fetch;
    this.now = options.now ?? Date.now;
    if (!options.clientId.trim()) throw new Error("OIDC clientId is required");
  }
  private async json(url: string): Promise<unknown> {
    const response = await this.fetch(url, { signal: AbortSignal.timeout(5000), redirect: "error" });
    if (!response.ok) throw new Error("OIDC HTTP status " + response.status);
    return response.json();
  }
  private async refresh(): Promise<void> {
    if (this.refreshing) return this.refreshing;
    if (this.now() - this.refreshAt < 60_000) return;
    this.refreshAt = this.now();
    this.refreshing = (async () => {
      try {
        const discovery = await this.json(this.issuer + "/.well-known/openid-configuration");
        if (typeof discovery !== "object" || !discovery || !("jwks_uri" in discovery)
          || typeof discovery.jwks_uri !== "string") throw new Error("OIDC discovery has no jwks_uri");
        if ("issuer" in discovery && discovery.issuer !== this.issuer) throw new Error("OIDC discovery issuer mismatch");
        const uri = validateHttpUrl(discovery.jwks_uri, "jwks_uri");
        const jwks = await this.json(uri);
        if (typeof jwks !== "object" || !jwks || !("keys" in jwks) || !Array.isArray(jwks.keys)
          || !jwks.keys.every((key: unknown) => typeof key === "object" && key !== null && "kty" in key && typeof key.kty === "string")) {
          throw new Error("Invalid OIDC JWKS");
        }
        // The runtime shape check above establishes the JSONWebKeySet boundary.
        const set = jwks as JSONWebKeySet;
        this.keySet = createLocalJWKSet(set);
        this.keys = set.keys;
        this.unavailable = false;
      } catch (cause) {
        this.unavailable = true;
        throw new CitoError("AUTH_UNAVAILABLE", "OIDC discovery/JWKS unavailable during cache miss", { cause });
      } finally { this.refreshing = undefined; }
    })();
    return this.refreshing;
  }
  async verify(token: string): Promise<VerifiedOidcClaims> {
    try {
      const header = decodeProtectedHeader(token);
      if (typeof header.alg !== "string" || !["RS256", "ES256"].includes(header.alg) || typeof header.kid !== "string" || !header.kid) {
        throw new Error("OIDC requires RS256/ES256 and kid");
      }
      if (!this.keySet || !this.keys.some(key => key.kid === header.kid)) await this.refresh();
      if (!this.keySet || !this.keys.some(key => key.kid === header.kid)) {
        if (this.unavailable) throw new CitoError("AUTH_UNAVAILABLE", "OIDC keys unavailable; refresh cooldown active");
        throw new Error("Unknown OIDC signing kid");
      }
      const { payload } = await jwtVerify(token, this.keySet, {
        algorithms: ["RS256", "ES256"], issuer: this.issuer, audience: this.options.clientId,
        requiredClaims: ["iss", "sub", "exp", "aud"], clockTolerance: 30,
        currentDate: new Date(this.now()),
      });
      // jwtVerify checks exp's type and strict > now - tolerance.
      if (typeof payload.iss !== "string" || typeof payload.sub !== "string" || !payload.sub.trim()
        || typeof payload.exp !== "number") throw new Error("Invalid OIDC identity claims");
      oidcPrincipal(payload);
      return payload as VerifiedOidcClaims;
    } catch (cause) {
      if (cause instanceof CitoError && cause.code === "AUTH_UNAVAILABLE") throw cause;
      throw new CitoError("UNAUTHORIZED", "OIDC token rejected", { cause });
    }
  }
}
