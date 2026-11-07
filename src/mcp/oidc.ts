import { createHash, randomBytes, randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, writeFile, rename, rm, chmod } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { OidcVerifier } from "../auth-callout/oidc.js";
import { oidcMutex, CitoError, oidcPrincipal, sha256, type CitoConfig, type Principal } from "../shared/index.js";
import { validateHttpUrl } from "../shared/config.js";

const cacheSchema = z.object({ issuer: z.string(), client_id: z.string(), id_token: z.string(),
  refresh_token: z.string().optional(), token_sha256: z.string(), expires_at: z.number(),
  verified_principal_id: z.string().min(1), verified_user: z.string().min(1) });
export type TokenCache = z.infer<typeof cacheSchema>;
export type BrowserOpener = (url: string, signal: AbortSignal) => Promise<void>;
const exec = promisify(execFile);
const openBrowser: BrowserOpener = async (url, signal) => { await exec("/usr/bin/open", [url], { signal }); };

/** The lock covers double-check, refresh/PKCE, cryptographic verification and atomic publication. */
export class OidcLogin {
  private readonly config: CitoConfig;
  private readonly opener: BrowserOpener;
  private readonly hub: string;
  private readonly path: string;
  private readonly verifier: OidcVerifier;
  constructor(config: CitoConfig, opener: BrowserOpener = openBrowser, hub = config.defaultHub) {
    this.config = config;
    this.opener = opener;
    this.hub = hub;
    this.path = join(config.stateDir, `oidc-token-${hub}.json`);
    this.verifier = new OidcVerifier({ issuer: config.oidcIssuer!, clientId: config.oidcClientId! });
  }
  async cached(): Promise<TokenCache | undefined> {
    try {
      const content = await readFile(this.path, "utf8");
      const value = cacheSchema.parse(JSON.parse(content));
      if (value.issuer !== this.config.oidcIssuer || value.client_id !== this.config.oidcClientId
        || value.token_sha256 !== sha256(value.id_token)) return;
      return value;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT" || error instanceof SyntaxError || error instanceof z.ZodError) return;
      throw error;
    }
  }
  async offlinePrincipal(): Promise<Principal> {
    const cache = await this.cached();
    if (!cache) throw new CitoError("UNAUTHORIZED", "Offline inbox requires a previously verified OIDC identity");
    return { principal_id: cache.verified_principal_id, owner_user: cache.verified_user };
  }
  async current(interactive = true, signal?: AbortSignal): Promise<TokenCache> {
    signal?.throwIfAborted();
    const cache = await this.cached();
    if (cache && cache.expires_at > Date.now() / 1000 + 60) {
      await this.verifier.verify(cache.id_token, signal);
      signal?.throwIfAborted();
      return cache;
    }
    return this.recover(cache?.token_sha256 ?? null, interactive, signal);
  }
  async recover(rejected: string | null, interactive = true, external?: AbortSignal): Promise<TokenCache> {
    const deadline = performance.now() + 45_000;
    const signal = AbortSignal.any([AbortSignal.timeout(45_000), ...(external ? [external] : [])]);
    const operation = oidcMutex.run("port:" + (this.config.oidcRedirectPort || this.hub), async () => {
    signal.throwIfAborted();
    const remaining = () => {
      signal.throwIfAborted();
      const ms = Math.ceil(deadline - performance.now());
      if (ms <= 0) throw new CitoError("OIDC_LOGIN_TIMEOUT", "OIDC login deadline exceeded");
      return ms;
    };
    const request = async (url: string, options: RequestInit = {}) => {
      try { return await fetch(url, { ...options, redirect: "error", signal: AbortSignal.any([signal, AbortSignal.timeout(Math.min(10_000, remaining())), ...(options.signal ? [options.signal] : [])]) }); }
      catch (cause) { throw new CitoError(signal.aborted ? "OIDC_LOGIN_TIMEOUT" : "AUTH_UNAVAILABLE", "OIDC endpoint unavailable", { cause }); }
    };
    try {
      remaining();
      const cache = await this.cached();
      if (cache && cache.token_sha256 !== rejected && cache.expires_at > Date.now() / 1000 + 60) {
        await this.verifier.verify(cache.id_token, signal);
        remaining();
        return cache;
      }
      const discoveryResponse = await request(this.config.oidcIssuer + "/.well-known/openid-configuration");
      if (!discoveryResponse.ok) throw new CitoError("AUTH_UNAVAILABLE", "OIDC discovery unavailable");
      const discovery = z.object({ issuer: z.literal(this.config.oidcIssuer!), token_endpoint: z.string(), authorization_endpoint: z.string(), scopes_supported: z.array(z.string()).optional() })
        .parse(await discoveryResponse.json());
      if (discovery.scopes_supported && !discovery.scopes_supported.includes("openid")) throw new CitoError("AUTH_UNAVAILABLE", "OIDC provider does not support openid");
      const scopes = this.config.oidcScopes ?? ["openid", "profile", "em" + "ail", "offline_access"].filter(scope => !discovery.scopes_supported || discovery.scopes_supported.includes(scope));
      const tokenUrl = validateHttpUrl(discovery.token_endpoint, "token_endpoint");
      const authUrl = validateHttpUrl(discovery.authorization_endpoint, "authorization_endpoint");
      const exchange = async (params: Record<string, string>) => request(tokenUrl, { method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ client_id: this.config.oidcClientId!, ...params }) });
      const save = async (response: Response, grant: "refresh_token" | "authorization_code"): Promise<TokenCache> => {
        if (!response.ok) throw new CitoError(response.status >= 500 ? "AUTH_UNAVAILABLE" : "UNAUTHORIZED", "OIDC token exchange rejected");
        const body = z.object({ id_token: z.string().min(1), refresh_token: z.string().optional() }).parse(await response.json());
        // Bind verification requests to the same login deadline, including JWKS fetch.
        const claims = await this.verifier.verify(body.id_token, signal);
        remaining();
        const principal = oidcPrincipal(claims);
        const value: TokenCache = { issuer: this.config.oidcIssuer!, client_id: this.config.oidcClientId!,
          id_token: body.id_token, refresh_token: body.refresh_token
            ?? (grant === "refresh_token" && principal.principal_id === cache?.verified_principal_id ? cache.refresh_token : undefined),
          token_sha256: sha256(body.id_token), expires_at: claims.exp,
          verified_principal_id: principal.principal_id, verified_user: principal.owner_user };
        const temporary = this.path + "." + randomUUID();
        try {
          await writeFile(temporary, JSON.stringify(value), { mode: 0o600, flag: "wx" });
          await rename(temporary, this.path); await chmod(this.path, 0o600);
        } finally { await rm(temporary, { force: true }); }
        return value;
      };
      if (cache?.refresh_token) {
        const refreshed = await exchange({ grant_type: "refresh_token", refresh_token: cache.refresh_token });
        if (refreshed.ok) return await save(refreshed, "refresh_token");
        if (![400, 401].includes(refreshed.status)) throw new CitoError("AUTH_UNAVAILABLE", "OIDC refresh unavailable");
      }
      if (!interactive) throw new CitoError("UNAUTHORIZED", "OIDC login required");
      remaining();
      const pkceVerifier = randomBytes(32).toString("base64url"), state = randomBytes(32).toString("base64url");
      const challenge = createHash("sha256").update(pkceVerifier).digest("base64url");
      let acceptCode!: (code: string) => void;
      let rejectCode!: (cause: Error) => void;
      const code = new Promise<string>((resolve, reject) => { acceptCode = resolve; rejectCode = reject; });
      // Attach a rejection handler before opening the browser (which can itself fail).
      void code.catch(() => {});
      const server = createServer((req, res) => {
        let url: URL;
        try { url = new URL(req.url ?? "/", "http://127.0.0.1"); }
        catch { res.writeHead(400).end("Invalid callback URL"); return; }
        if (url.pathname !== "/callback" || url.searchParams.get("state") !== state) { res.writeHead(400).end("Invalid callback"); return; }
        const authorizationCode = url.searchParams.get("code");
        if (!authorizationCode) { res.writeHead(400).end("Login cancelled"); rejectCode(new CitoError("UNAUTHORIZED", "OIDC login cancelled")); return; }
        res.end("Cito login complete. You may close this window."); acceptCode(authorizationCode);
      });
      const abort = () => rejectCode(new CitoError("OIDC_LOGIN_TIMEOUT", "OIDC browser login deadline exceeded"));
      signal.addEventListener("abort", abort, { once: true });
      try {
        await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(this.config.oidcRedirectPort ?? 0, "127.0.0.1", resolve); });
        const address = server.address();
        if (!address || typeof address === "string") throw new Error("Invalid loopback address");
        const redirect = `http://127.0.0.1:${address.port}/callback`;
        const url = new URL(authUrl);
        url.search = new URLSearchParams({ response_type: "code", client_id: this.config.oidcClientId!, redirect_uri: redirect,
          scope: scopes.join(" "), state, code_challenge: challenge, code_challenge_method: "S256" }).toString();
        remaining();
        await Promise.race([this.opener(url.toString(), signal), code.then(() => {})]);
        return await save(await exchange({ grant_type: "authorization_code", code: await code, redirect_uri: redirect, code_verifier: pkceVerifier }), "authorization_code");
      } finally {
        signal.removeEventListener("abort", abort);
        server.closeAllConnections();
        await new Promise<void>(resolve => server.close(() => resolve()));
      }
    } catch (cause) {
      if (signal.aborted || performance.now() >= deadline) throw new CitoError("OIDC_LOGIN_TIMEOUT", "OIDC login deadline exceeded", { cause });
      if (cause instanceof CitoError) throw cause;
      throw new CitoError("AUTH_UNAVAILABLE", "OIDC login failed", { cause });
    }
    });
    // Cancellation also interrupts time spent queued behind another hub on a fixed port.
    let abort!: () => void;
    const cancelled = new Promise<never>((_, reject) => {
      abort = () => reject(external?.aborted ? external.reason : new CitoError("OIDC_LOGIN_TIMEOUT", "OIDC login deadline exceeded", { cause: signal.reason }));
      signal.addEventListener("abort", abort, { once: true }); if (signal.aborted) abort();
    });
    try { return await Promise.race([operation, cancelled]); }
    finally { signal.removeEventListener("abort", abort); }
  }
}
