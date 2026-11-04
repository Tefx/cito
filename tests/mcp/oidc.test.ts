import { afterEach, expect, test, vi } from "vitest";
import { readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { OidcLogin } from "../../src/mcp/oidc.js";
import { signingKey, startOidc, browser } from "../auth-callout/oidc-fixture.js";
import { fixture, waitFor } from "./support.js";
import { createServer } from "node:net";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
async function setup() {
  const key = await signingKey("ES256", "key");
  const oidc = await startOidc([key.jwk]); cleanups.push(() => oidc.stop());
  const f = await fixture({ pi: false, env: { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" } });
  cleanups.push(() => f.close());
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims()), refresh_token: "refresh-1" };
  let opened = 0;
  const opener = async (url: string, signal: AbortSignal) => { opened++; await browser(url, signal); };
  const login = new OidcLogin(f.config, opener);
  return { f, key, oidc, login, opened: () => opened, opener };
}

test("interactive S256 PKCE verifies signature and stores canonical identity with mode 0600", async () => {
  const { f, oidc, login, opened } = await setup();
  const cache = await login.recover(null);
  expect(cache).toMatchObject({ issuer: oidc.issuer, client_id: "cito", verified_principal_id: oidc.issuer + "#user-1", verified_user: "alice", refresh_token: "refresh-1" });
  expect(opened()).toBe(1);
  expect(oidc.state.codeRequests).toBe(1);
  expect((await stat(join(f.stateDir, "oidc-token-default.json"))).mode & 0o777).toBe(0o600);
  expect(await login.current()).toEqual(cache);
  expect(await login.offlinePrincipal()).toEqual({ principal_id: cache.verified_principal_id, owner_user: "alice" });
});

test("rejected unexpired token refreshes once; concurrent mutex waiters reuse a different hash", async () => {
  const { f, key, oidc, login, opener, opened } = await setup();
  const old = await login.recover(null);
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: "user-1", exp: Math.floor(Date.now() / 1000) + 3600 })), refresh_token: "refresh-2" };
  const [a, b, c] = await Promise.all([login.recover(old.token_sha256), new OidcLogin(f.config, opener).recover(old.token_sha256), new OidcLogin(f.config, opener).recover(old.token_sha256)]);
  expect(a.token_sha256).not.toBe(old.token_sha256);
  expect([b.token_sha256, c.token_sha256]).toEqual([a.token_sha256, a.token_sha256]);
  expect(oidc.state.refreshRequests).toBe(1);
  expect(opened()).toBe(1);
});

test.each([400, 401])("refresh %i falls through to PKCE within the same lock", async status => {
  const { key, oidc, login, opened } = await setup();
  const old = await login.recover(null);
  oidc.state.refreshStatus = status;
  oidc.state.tokenResponse.id_token = await key.sign(oidc.claims({ exp: Math.floor(Date.now() / 1000) + 3600 }));
  const fresh = await login.recover(old.token_sha256);
  expect(fresh.token_sha256).not.toBe(old.token_sha256);
  expect(opened()).toBe(2); expect(oidc.state.codeRequests).toBe(2); expect(oidc.state.refreshRequests).toBe(1);
});

test("refresh 503 fails without launching a browser or overwriting verified cache", async () => {
  const { f, oidc, login, opened } = await setup();
  const old = await login.recover(null), original = await readFile(join(f.stateDir, "oidc-token-default.json"), "utf8");
  oidc.state.refreshStatus = 503;
  await expect(login.recover(old.token_sha256)).rejects.toMatchObject({ code: "AUTH_UNAVAILABLE", retryable: true });
  expect(opened()).toBe(1);
  expect(await readFile(join(f.stateDir, "oidc-token-default.json"), "utf8")).toBe(original);
});

test("a different PKCE identity never inherits the previous refresh token after refresh rejection", async () => {
  const { key, oidc, login } = await setup();
  const alice = await login.recover(null);
  expect(alice.refresh_token).toBe("refresh-1");
  oidc.state.refreshStatus = 401;
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: "user-2", preferred_username: "Bob" })) };
  const bob = await login.recover(alice.token_sha256);
  expect(bob).toMatchObject({ verified_principal_id: oidc.issuer + "#user-2", verified_user: "bob" });
  expect(bob.refresh_token).toBeUndefined();
  expect(await login.cached()).not.toHaveProperty("refresh_token");
  expect(oidc.state.refreshRequests).toBe(1); expect(oidc.state.codeRequests).toBe(2);
});

test.each([false, true])("silent refresh retains an omitted token only for the same principal (identity changed: %s)", async changed => {
  const { key, oidc, login } = await setup();
  const old = await login.recover(null);
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: changed ? "other-user" : "user-1",
    exp: Math.floor(Date.now() / 1000) + 3600 })) };
  const fresh = await login.recover(old.token_sha256);
  expect(fresh.refresh_token).toBe(changed ? undefined : "refresh-1");
  expect(oidc.state.refreshRequests).toBe(1); expect(oidc.state.codeRequests).toBe(1);
});

test("overall login-budget expiry during JWKS verification reports OIDC_LOGIN_TIMEOUT", async () => {
  const deadline = new AbortController(), timeout = AbortSignal.timeout.bind(AbortSignal), request = globalThis.fetch;
  vi.spyOn(AbortSignal, "timeout").mockImplementation(ms => ms === 45_000 ? deadline.signal : timeout(ms));
  let verifying = false;
  vi.spyOn(globalThis, "fetch").mockImplementation((url, options) => {
    if (String(url).endsWith("/jwks")) {
      verifying = true;
      deadline.abort(new DOMException("Login deadline expired", "TimeoutError"));
    }
    return request(url, options);
  });
  const { f, login } = await setup();
  await expect(login.recover(null)).rejects.toMatchObject({ code: "OIDC_LOGIN_TIMEOUT", retryable: true });
  expect(verifying).toBe(true);
  expect(await login.cached()).toBeUndefined();
  await expect(stat(join(f.stateDir, "oidc-token-default.json"))).rejects.toMatchObject({ code: "ENOENT" });
});

test("hub caches and PKCE scopes are isolated, with no legacy default-cache fallback", async () => {
  const { f, oidc, opener, login } = await setup();
  const initial = await login.recover(null);
  const personal = new OidcLogin({ ...f.config, oidcScopes: ["openid", "custom"] }, async (url, signal) => {
    expect(new URL(url).searchParams.get("scope")).toBe("openid custom");
    await opener(url, signal);
  }, "personal");
  expect(await personal.cached()).toBeUndefined();
  await personal.recover(null);
  expect(await new OidcLogin(f.config, opener, "rda").cached()).toBeUndefined();
  expect((await personal.offlinePrincipal()).principal_id).toBe(oidc.issuer + "#user-1");
  expect((await stat(join(f.stateDir, "oidc-token-personal.json"))).mode & 0o777).toBe(0o600);
  const { rename } = await import("node:fs/promises");
  await rename(join(f.stateDir, "oidc-token-default.json"), join(f.stateDir, "oidc-token.json"));
  expect(await login.cached()).toBeUndefined();
});

test("discovery negotiates advertised default scopes, honors explicit scopes and rejects providers without openid", async () => {
  const { f, oidc, opener } = await setup();
  oidc.state.scopesSupported = ["openid", "email"];
  const negotiated = new OidcLogin(f.config, async (url, signal) => {
    expect(new URL(url).searchParams.get("scope")).toBe("openid email"); await opener(url, signal);
  }, "negotiated");
  await negotiated.recover(null);
  const explicit = new OidcLogin({ ...f.config, oidcScopes: ["openid", "custom"] }, async (url, signal) => {
    expect(new URL(url).searchParams.get("scope")).toBe("openid custom"); await opener(url, signal);
  }, "explicit");
  await explicit.recover(null);
  oidc.state.scopesSupported = ["email"];
  await expect(new OidcLogin(f.config, opener, "missing-openid").recover(null)).rejects.toMatchObject({ code: "AUTH_UNAVAILABLE" });
});

test("fixed callback ports serialize independent hub logins and abort queued or active logins without opening another browser", async () => {
  const { f, opener } = await setup();
  const server = createServer(); await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as { port: number }).port; await new Promise<void>(resolve => server.close(() => resolve()));
  let firstUrl = "", opened = 0, release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const config = { ...f.config, oidcRedirectPort: port };
  const first = new OidcLogin(config, async (url, signal) => {
    opened++; firstUrl = url; await gate; await opener(url, signal);
  }, "a").recover(null);
  try {
    await waitFor(() => !!firstUrl); expect(new URL(new URL(firstUrl).searchParams.get("redirect_uri")!).port).toBe(String(port));
    const controller = new AbortController();
    const second = new OidcLogin(config, async (url, signal) => { opened++; await opener(url, signal); }, "b").recover(null, true, controller.signal);
    const rejected = expect(second).rejects.toMatchObject({ name: "AbortError" }); controller.abort(); await rejected;
    expect(opened).toBe(1); release(); await first;
    const activeAbort = new AbortController(); let active = false;
    const third = new OidcLogin(config, async () => { active = true; await new Promise(() => {}); }, "c").recover(null, true, activeAbort.signal);
    const cancelled = expect(third).rejects.toMatchObject({ name: "AbortError" });
    await waitFor(() => active); activeAbort.abort(); await cancelled;
    // Rebinding the exact port proves that the cancelled callback server closed.
    const fourth = new OidcLogin(config, opener, "d"); await fourth.recover(null);
  } finally { release(); await first; }
});

test("locally invalid signatures never acquire a verified identity cache", async () => {
  const { f, oidc, login } = await setup();
  const wrong = await signingKey("ES256", "key");
  oidc.state.tokenResponse.id_token = await wrong.sign(oidc.claims());
  await expect(login.recover(null)).rejects.toMatchObject({ code: "UNAUTHORIZED" });
  expect(await login.cached()).toBeUndefined();
  await writeFile(join(f.stateDir, "oidc-token-default.json"), "{}");
  await expect(login.offlinePrincipal()).rejects.toMatchObject({ code: "UNAUTHORIZED" });
});
