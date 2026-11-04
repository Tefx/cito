import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createAccount, createCurve, createServer, createUser } from "nkeys.js";
import { decode, encodeGeneric, type User, type AuthorizationResponse } from "@nats-io/jwt";
import { AckPolicy, connect, StringCodec } from "nats";
import { resolve } from "node:path";
import {
  loadAuthCalloutConfig, authorizationResponse, decodeRequest, startAuthCallout,
  OidcVerifier, USER_PERMISSIONS,
} from "../../src/auth-callout/index.js";
import { provisionNats } from "../../src/shared/nats.js";
import { startNats } from "../shared/nats-server.js";
import { signingKey, startOidc } from "./oidc-fixture.js";

const decoder = new TextDecoder();
const encoder = new TextEncoder();
describe("NATS auth callout wire protocol", () => {
  let key: Awaited<ReturnType<typeof signingKey>>;
  let oidc: Awaited<ReturnType<typeof startOidc>>;
  beforeAll(async () => {
    key = await signingKey("RS256", "rsa");
    oidc = await startOidc([key.jwk]);
  });
  afterAll(async () => { await oidc.stop(); });
  function config(extra: NodeJS.ProcessEnv = {}) {
    const account = createAccount();
    const c = loadAuthCalloutConfig({
      CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito",
      CITO_AUTH_CALLOUT_PASSWORD: "isolated-test-password",
      CITO_AUTH_CALLOUT_ISSUER_SEED: decoder.decode(account.getSeed()), ...extra,
    });
    account.clear();
    return c;
  }
  it("signs verifiable JWT v2 user and auth responses with exact broker scope and expiry", async () => {
    const c = config();
    const req = { user_nkey: createUser().getPublicKey(), server_id: { id: createServer().getPublicKey() },
      connect_opts: { auth_token: await key.sign(oidc.claims()) } };
    try {
      const bytes = await authorizationResponse(req, c, new OidcVerifier({ issuer: oidc.issuer, clientId: "cito" }));
      const response = decode<AuthorizationResponse>(decoder.decode(bytes));
      expect(response.iss).toBe(c.issuer.getPublicKey());
      expect(response.sub).toBe(req.user_nkey);
      expect(response.aud).toBe(req.server_id.id);
      expect(response.nats).toMatchObject({ type: "authorization_response", version: 2 });
      const user = decode<User>(response.nats.jwt!); // decode verifies NKey Ed25519 signature.
      expect(user.iss).toBe(c.issuer.getPublicKey());
      expect(user.sub).toBe(req.user_nkey);
      expect(user.aud).toBe("CITO");
      expect(user.exp).toBe(oidc.claims().exp);
      expect(user.nats.pub?.allow).toEqual(USER_PERMISSIONS);
      expect(user.nats.sub?.allow).toEqual(USER_PERMISSIONS);
      expect(user.nats.pub?.allow).toContain("$JS.ACK.>");
      expect(user.nats).toMatchObject({ type: "user", version: 2 });
      const denied = decode<AuthorizationResponse>(decoder.decode(await authorizationResponse({
        ...req, connect_opts: { auth_token: "invalid" },
      }, c, new OidcVerifier({ issuer: oidc.issuer, clientId: "cito" }))));
      expect(denied.nats.error).toContain("UNAUTHORIZED");
      expect(denied.nats.jwt).toBeUndefined();
    } finally { c.issuer.clear(); }
  });
  it("returns a signed transient rejection on discovery failure instead of crashing", async () => {
    const fixture = await startOidc();
    fixture.state.fail = true;
    const c = config();
    try {
      const req = { user_nkey: createUser().getPublicKey(), server_id: { id: createServer().getPublicKey() },
        connect_opts: { auth_token: await key.sign(fixture.claims()) } };
      const response = decode<AuthorizationResponse>(decoder.decode(await authorizationResponse(req, c,
        new OidcVerifier({ issuer: fixture.issuer, clientId: "cito" }))));
      expect(response.nats.error).toContain("AUTH_UNAVAILABLE");
      expect(response.nats.jwt).toBeUndefined();
    } finally { c.issuer.clear(); await fixture.stop(); }
  });
  it("authenticates encrypted requests and responses and rejects tampered ciphertext", async () => {
    const xkey = createCurve();
    const serverCurve = createCurve();
    const serverKey = createServer();
    const c = config({ CITO_AUTH_CALLOUT_XKEY_SEED: decoder.decode(xkey.getSeed()) });
    try {
      const req = { user_nkey: createUser().getPublicKey(),
        server_id: { id: serverKey.getPublicKey(), xkey: serverCurve.getPublicKey() },
        connect_opts: { auth_token: await key.sign(oidc.claims()) } };
      const requestJwt = await encodeGeneric("auth", serverKey, "authorization_request", req,
        { exp: Math.floor(Date.now() / 1000) + 30 });
      const sealed = serverCurve.seal(encoder.encode(requestJwt), xkey.getPublicKey());
      const decoded = decodeRequest(sealed, c, serverCurve.getPublicKey());
      expect(decoded.user_nkey).toBe(req.user_nkey);
      const encryptedResponse = await authorizationResponse(decoded, c,
        new OidcVerifier({ issuer: oidc.issuer, clientId: "cito" }));
      const response = serverCurve.open(encryptedResponse, xkey.getPublicKey());
      expect(decode<AuthorizationResponse>(decoder.decode(response!)).nats.jwt).toBeTruthy();
      const corrupted = sealed.slice();
      corrupted[corrupted.length - 1] = corrupted[corrupted.length - 1]! ^ 1;
      expect(() => decodeRequest(corrupted, c, serverCurve.getPublicKey())).toThrow();
      expect(() => decodeRequest(sealed, c)).toThrow("missing Nats-Server-Xkey");
    } finally { xkey.clear(); serverCurve.clear(); serverKey.clear(); c.issuer.clear(); c.xkey?.clear(); }
  });
  it.each([false, true])("official broker authorizes OIDC and denies bad tokens (XKey=%s)", async encrypted => {
    const curve = createCurve();
    const c = config(encrypted ? { CITO_AUTH_CALLOUT_XKEY_SEED: decoder.decode(curve.getSeed()) } : {});
    const file = resolve("deploy/nats/nats-server.oidc" + (encrypted ? "-xkey" : "") + ".conf");
    const broker = await startNats({ configFile: file }, {
      CITO_AUTH_CALLOUT_PASSWORD: c.servicePassword,
      CITO_AUTH_CALLOUT_ISSUER: c.issuer.getPublicKey(),
      CITO_AUTH_CALLOUT_XKEY: curve.getPublicKey(),
    });
    c.natsUrl = broker.url;
    const errors: Error[] = [];
    let service: Awaited<ReturnType<typeof startAuthCallout>> | undefined;
    let client: Awaited<ReturnType<typeof connect>> | undefined;
    try {
      service = await startAuthCallout(c, { onError: error => errors.push(error) });
      client = await connect({ servers: broker.url, token: await key.sign(oidc.claims()), timeout: 2000 });
      const { js, jsm, kv } = await provisionNats(client);
      expect((await js.publish("cito.signal.c_abcdefgh2345", StringCodec().encode("hello"))).seq).toBe(1);
      await kv.put("c_abcdefgh2345", StringCodec().encode("listener"));
      expect(StringCodec().decode((await kv.get("c_abcdefgh2345"))!.value)).toBe("listener");
      // A real pull consumer ack exercises the granted $JS.ACK.> publication permission.
      await jsm.consumers.add("CITO_SIGNALS", { durable_name: "ack-proof", ack_policy: AckPolicy.Explicit });
      const consumer = await js.consumers.get("CITO_SIGNALS", "ack-proof");
      const message = await consumer.next({ expires: 1000 });
      expect(message).toBeTruthy();
      expect(await message!.ackAck()).toBe(true);
      await consumer.delete();
      const permissionDenied = new Promise<void>(resolve => {
        const sub = client!.subscribe("outside.cito.scope", { callback: error => {
          if (error) { sub.unsubscribe(); resolve(); }
        } });
      });
      await permissionDenied;
      for (const payload of [
        { aud: "wrong" }, { iss: "https://wrong" }, { exp: Math.floor(Date.now() / 1000) - 60 },
      ]) {
        await expect(connect({ servers: broker.url, token: await key.sign(oidc.claims(payload)), timeout: 2000 }))
          .rejects.toThrow(/Authorization Violation/i);
      }
      await expect(connect({ servers: broker.url, token: "bad", timeout: 2000 })).rejects.toThrow(/Authorization Violation/i);
      expect(errors).toEqual([]);
    } finally {
      await client?.close();
      await service?.stop();
      await broker.stop();
      c.issuer.clear(); c.xkey?.clear(); curve.clear();
    }
  });
});
