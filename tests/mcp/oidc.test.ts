import { afterEach, expect, test, vi } from "vitest";
import { readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { OidcLogin } from "../../src/mcp/oidc.js";
import { signingKey, startOidc, browser } from "../auth-callout/oidc-fixture.js";
import { fixture } from "./support.js";

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

test("rejected unexpired token refreshes once; concurrent flock waiters reuse a different hash", async () => {
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

test("hub caches and PKCE scopes are isolated, with a legacy default-cache fallback", async () => {
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
  expect(await login.cached()).toEqual(initial);
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
