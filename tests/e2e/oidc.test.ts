import { expect, test } from "vitest";
import { createAccount } from "nkeys.js";
import { resolve } from "node:path";
import { startNats } from "../shared/nats-server.js";
import { startOidc, signingKey, browser } from "../auth-callout/oidc-fixture.js";
import { fixture, call, payload } from "../mcp/support.js";
import { loadAuthCalloutConfig } from "../../src/auth-callout/config.js";
import { startAuthCallout } from "../../src/auth-callout/responder.js";
import { OidcVerifier } from "../../src/auth-callout/oidc.js";
import { OidcLogin } from "../../src/mcp/oidc.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import { CitoService } from "../../src/mcp/service.js";

test("OIDC auth-callout + MCP enforces private/workspace scopes and works offline with verified cached identity", async () => {
  const key = await signingKey("ES256", "e2e"), oidc = await startOidc([key.jwk]), account = createAccount();
  const broker = await startNats({ configFile: resolve("deploy/nats/nats-server.oidc.conf") }, {
    CITO_AUTH_CALLOUT_PASSWORD: "test-password", CITO_AUTH_CALLOUT_ISSUER: account.getPublicKey() });
  const env = { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" };
  const config = loadAuthCalloutConfig({ ...env, CITO_NATS_URL: broker.url, ...broker.env,
    CITO_AUTH_CALLOUT_PASSWORD: "test-password", CITO_AUTH_CALLOUT_ISSUER_SEED: new TextDecoder().decode(account.getSeed()) });
  const callout = await startAuthCallout(config);
  const alice = await fixture({ broker, env, service: { browserOpener: browser } });
  const bob = await fixture({ broker, pi: false, env, service: { browserOpener: browser } });
  try {
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims()), refresh_token: "alice-refresh" };
    const { listener_id: id } = await call(alice.service, "cito_listen", { about: "OIDC private" }, alice.pi.meta);
    expect(alice.service.state.listener(toCanonicalListenerId(id))!.principal_id).toBe(oidc.issuer + "#user-1");
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: "user-2", preferred_username: "Bob" })), refresh_token: "bob-refresh" };
    expect((await call(bob.service, "cito_find")).listeners).toEqual([]);
    expect(payload(await bob.service.call("cito_send", { to: id, message: "blocked" }))).toMatchObject({ error: { code: "FORBIDDEN_TARGET_SCOPE" } });
    await call(alice.service, "cito_listen", { about: "OIDC shared", scope: "workspace" }, alice.pi.meta);
    expect((await call(bob.service, "cito_find")).listeners).toMatchObject([{ listener_id: id, user: "alice" }]);
    expect(await call(bob.service, "cito_send", { to: id, message: "peer notification" })).toMatchObject({ status: "wake_accepted", held_reason: null });
    await callout.stop(); await broker.stop(); await oidc.stop();
    const inbox = await call(alice.service, "cito_inbox", { ack_all: true }, alice.pi.meta);
    expect(inbox).toMatchObject({ acknowledged_count: 1, signals: [{ message: "peer notification", from: { user: "bob" } }] });
  } finally {
    await bob.close(); await alice.close(); await callout.stop(); await broker.stop();
    // stopOidc's server close is not idempotent, so tolerate the already-stopped case.
    await oidc.stop().catch(error => { if (error.code !== "ERR_SERVER_NOT_RUNNING") throw error; });
    config.issuer.clear(); account.clear();
  }
}, 20_000);

test("actual broker rejection of a locally unexpired token triggers one refresh; waiters reuse it", async () => {
  const key = await signingKey("ES256", "rejection"), oidc = await startOidc([key.jwk]), account = createAccount();
  const broker = await startNats({ configFile: resolve("deploy/nats/nats-server.oidc.conf") }, {
    CITO_AUTH_CALLOUT_PASSWORD: "test-password", CITO_AUTH_CALLOUT_ISSUER: account.getPublicKey() });
  const env = { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" };
  const config = loadAuthCalloutConfig({ ...env, CITO_NATS_URL: broker.url, ...broker.env,
    CITO_AUTH_CALLOUT_PASSWORD: "test-password", CITO_AUTH_CALLOUT_ISSUER_SEED: new TextDecoder().decode(account.getSeed()) });
  // Server clock skew creates a real JWT rejection without mocking the NATS connection or callout.
  const callout = await startAuthCallout(config, { verifier: new OidcVerifier({ issuer: oidc.issuer, clientId: "cito", now: () => Date.now() + 240_000 }) });
  const f = await fixture({ broker, pi: false, env, service: { browserOpener: browser } });
  let second: CitoService | undefined;
  try {
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ exp: Math.floor(Date.now() / 1000) + 120 })), refresh_token: "refresh-old" };
    const old = await new OidcLogin(f.config, browser).recover(null);
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ exp: Math.floor(Date.now() / 1000) + 3600 })), refresh_token: "refresh-new" };
    second = await CitoService.create(f.config, f.serviceOptions);
    const results = await Promise.all([call(f.service, "cito_find"), call(second, "cito_find")]);
    expect(results.map(result => result.listeners)).toEqual([[], []]);
    const fresh = await new OidcLogin(f.config, browser).cached();
    expect(fresh!.token_sha256).not.toBe(old.token_sha256);
    expect(oidc.state.refreshRequests).toBe(1);
    expect(oidc.state.authorizationRequests).toBe(1);
  } finally {
    await second?.close(); await f.close(); await callout.stop(); await broker.stop(); await oidc.stop();
    config.issuer.clear(); account.clear();
  }
}, 20_000);
