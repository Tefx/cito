import { expect, test } from "vitest";
import { startHubServer, OidcVerifier } from "../../src/hub/index.js";
import { startOidc, signingKey, browser } from "../auth-callout/oidc-fixture.js";
import { fixture, call, payload } from "../mcp/support.js";
import { OidcLogin } from "../../src/mcp/oidc.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import { CitoService } from "../../src/mcp/service.js";

test("OIDC HubServer + MCP enforces private/workspace scopes and works offline with verified cached identity", async () => {
  const key = await signingKey("ES256", "e2e"), oidc = await startOidc([key.jwk]);
  const broker = await startHubServer({ authMode: "oidc", oidcIssuer: oidc.issuer, oidcClientId: "cito" });
  const env = { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" };
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
    await broker.stop(); await oidc.stop();
    const inbox = await call(alice.service, "cito_inbox", { ack_all: true }, alice.pi.meta);
    expect(inbox).toMatchObject({ acknowledged_count: 1, signals: [{ message: "peer notification", from: { user: "bob" } }] });
  } finally {
    await bob.close(); await alice.close(); await broker.stop();
    await oidc.stop().catch(error => { if (error.code !== "ERR_SERVER_NOT_RUNNING") throw error; });
  }
}, 20_000);

test("send authorizes and publishes as the identity authenticated after rejected-token recovery", async () => {
  const key = await signingKey("ES256", "send-identity"), oidc = await startOidc([key.jwk]);
  // The sender accepts its cached token locally; the HubServer rejects it at its later clock.
  const broker = await startHubServer({
    authMode: "oidc",
    oidcIssuer: oidc.issuer,
    oidcClientId: "cito",
    verifier: new OidcVerifier({ issuer: oidc.issuer, clientId: "cito", now: () => Date.now() + 240_000 }),
  });
  const env = { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" };
  const alice = await fixture({ broker, env, service: { browserOpener: browser } });
  const sender = await fixture({ broker, pi: false, env, service: { browserOpener: browser } });
  try {
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ exp: Math.floor(Date.now() / 1000) + 3600 })), refresh_token: "alice-refresh" };
    const { listener_id: id } = await call(alice.service, "cito_listen", { about: "Alice private" }, alice.pi.meta);
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ exp: Math.floor(Date.now() / 1000) + 120 })), refresh_token: "sender-alice-refresh" };
    await new OidcLogin(sender.config, browser).recover(null);
    oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: "user-2", preferred_username: "Bob", exp: Math.floor(Date.now() / 1000) + 3600 })), refresh_token: "bob-refresh" };
    expect(payload(await sender.service.call("cito_send", { to: id, message: "private attempt" }))).toMatchObject({ error: { code: "FORBIDDEN_TARGET_SCOPE" } });
    const senderBroker = await sender.service.slots.get("default")!.network();
    expect((await senderBroker.jsm.streams.info("CITO_SIGNALS")).state.messages).toBe(0);
    expect((await new OidcLogin(sender.config, browser).cached())!.verified_principal_id).toBe(oidc.issuer + "#user-2");
    expect(oidc.state.refreshRequests).toBe(1);
    await call(alice.service, "cito_listen", { about: "Alice workspace", scope: "workspace" }, alice.pi.meta);
    const sent = await call(sender.service, "cito_send", { to: id, message: "workspace message" });
    expect(sent.status).toBe("wake_accepted");
    const wire = (await senderBroker.jsm.streams.getMessage("CITO_SIGNALS", { seq: sent.seq })).json<{ from: { principal_id: string; user: string }; sender_endpoint_key: string }>();
    expect(wire.from).toMatchObject({ principal_id: oidc.issuer + "#user-2", user: "bob" });
    expect(wire.sender_endpoint_key).toBe(oidc.issuer + "#user-2|" + sender.service.hostId + "|unbound|default");
  } finally {
    await sender.close(); await alice.close(); await broker.stop(); await oidc.stop();
  }
}, 20_000);

test("actual broker rejection of a locally unexpired token triggers one refresh; waiters reuse it", async () => {
  const key = await signingKey("ES256", "rejection"), oidc = await startOidc([key.jwk]);
  // Server clock skew creates a real JWT rejection without mocking the connection or verifier.
  const broker = await startHubServer({
    authMode: "oidc",
    oidcIssuer: oidc.issuer,
    oidcClientId: "cito",
    verifier: new OidcVerifier({ issuer: oidc.issuer, clientId: "cito", now: () => Date.now() + 240_000 }),
  });
  const env = { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" };
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
    await second?.close(); await f.close(); await broker.stop(); await oidc.stop();
  }
}, 20_000);
