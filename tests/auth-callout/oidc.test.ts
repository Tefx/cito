import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { SignJWT } from "jose";
import { OidcVerifier } from "../../src/auth-callout/oidc.js";
import { signingKey, startOidc } from "./oidc-fixture.js";

describe("OIDC signature verification and in-memory JWKS", () => {
  let rsa: Awaited<ReturnType<typeof signingKey>>;
  let ec: Awaited<ReturnType<typeof signingKey>>;
  let oidc: Awaited<ReturnType<typeof startOidc>>;
  beforeAll(async () => {
    rsa = await signingKey("RS256", "rsa");
    ec = await signingKey("ES256", "ec");
    oidc = await startOidc([rsa.jwk, ec.jwk]);
  });
  afterAll(async () => { await oidc.stop(); });
  it("verifies both RS256 and ES256, scalar and array aud, and requires identity claims", async () => {
    const verifier = new OidcVerifier({ issuer: oidc.issuer, clientId: "cito" });
    const before = oidc.state.jwksRequests;
    for (const key of [rsa, ec]) {
      const token = await key.sign(oidc.claims({ aud: ["other", "cito"] }));
      expect((await verifier.verify(token)).sub).toBe("user-1");
    }
    expect(oidc.state.jwksRequests - before).toBe(1);
    const badPayloads = [
      { exp: Math.floor(Date.now() / 1000) - 31 }, { aud: "wrong" },
      { iss: "https://wrong" }, { iss: oidc.issuer + "/" },
      { sub: "" }, { sub: undefined }, { exp: undefined }, { aud: undefined }, { iss: undefined },
      { nbf: Math.floor(Date.now() / 1000) + 60 },
    ];
    for (const payload of badPayloads) {
      await expect(verifier.verify(await rsa.sign(oidc.claims(payload)))).rejects.toMatchObject({ code: "UNAUTHORIZED" });
    }
    await expect(verifier.verify("broken.jwt")).rejects.toMatchObject({ code: "UNAUTHORIZED" });
    const hmac = await new SignJWT(oidc.claims()).setProtectedHeader({ alg: "HS256", kid: "rsa" }).sign(new Uint8Array(32));
    await expect(verifier.verify(hmac)).rejects.toMatchObject({ code: "UNAUTHORIZED" });
    const signed = await rsa.sign(oidc.claims());
    const [header, , signature] = signed.split(".");
    const forgedBody = Buffer.from(JSON.stringify(oidc.claims({ sub: "forged" }))).toString("base64url");
    await expect(verifier.verify(header + "." + forgedBody + "." + signature)).rejects.toMatchObject({ code: "UNAUTHORIZED" });
    await expect(verifier.verify(await rsa.sign(oidc.claims(), { kid: "" }))).rejects.toMatchObject({ code: "UNAUTHORIZED" });
  });
  it("honors strict exp > now - 30 clock tolerance", async () => {
    const now = Math.floor(Date.now() / 1000) * 1000;
    const verifier = new OidcVerifier({ issuer: oidc.issuer, clientId: "cito", now: () => now });
    await expect(verifier.verify(await ec.sign(oidc.claims({ exp: now / 1000 - 29 })))).resolves.toMatchObject({ sub: "user-1" });
    await expect(verifier.verify(await ec.sign(oidc.claims({ exp: now / 1000 - 30 })))).rejects.toMatchObject({ code: "UNAUTHORIZED" });
  });
  it("refreshes on unknown kid after a 60-second cooldown and shares concurrent refreshes", async () => {
    const rotated = await signingKey("ES256", "rotated");
    const fixture = await startOidc([rsa.jwk]);
    let now = Date.now();
    const verifier = new OidcVerifier({ issuer: fixture.issuer, clientId: "cito", now: () => now });
    try {
      await verifier.verify(await rsa.sign(fixture.claims()));
      fixture.state.keys = [rsa.jwk, rotated.jwk];
      const token = await rotated.sign(fixture.claims());
      await expect(verifier.verify(token)).rejects.toMatchObject({ code: "UNAUTHORIZED" });
      now += 59_999;
      await expect(verifier.verify(token)).rejects.toMatchObject({ code: "UNAUTHORIZED" });
      expect(fixture.state.jwksRequests).toBe(1);
      now += 1;
      await Promise.all(Array.from({ length: 10 }, () => verifier.verify(token)));
      expect(fixture.state.jwksRequests).toBe(2);
      expect(fixture.state.discoveryRequests).toBe(2);
    } finally { await fixture.stop(); }
  });
  it("returns transient AUTH_UNAVAILABLE on cache-miss fetch failure, retaining usable cached keys", async () => {
    const unknown = await signingKey("ES256", "unknown");
    const fixture = await startOidc([rsa.jwk]);
    let now = Date.now();
    const verifier = new OidcVerifier({ issuer: fixture.issuer, clientId: "cito", now: () => now });
    try {
      const known = await rsa.sign(fixture.claims());
      const token = await unknown.sign(fixture.claims());
      await verifier.verify(known);
      fixture.state.fail = true;
      now += 60_000;
      await expect(verifier.verify(token)).rejects.toMatchObject({ code: "AUTH_UNAVAILABLE" });
      await expect(verifier.verify(token)).rejects.toMatchObject({ code: "AUTH_UNAVAILABLE" });
      await expect(verifier.verify(known)).resolves.toMatchObject({ sub: "user-1" });
      expect(fixture.state.discoveryRequests).toBe(2);
      fixture.state.fail = false;
      fixture.state.keys = [rsa.jwk, unknown.jwk];
      now += 60_000;
      await expect(verifier.verify(token)).resolves.toMatchObject({ sub: "user-1" });
    } finally { await fixture.stop(); }
  });
  it("aborts discovery after the 5000ms timeout rather than hanging", async () => {
    const fixture = await startOidc();
    fixture.state.stall = true;
    const verifier = new OidcVerifier({ issuer: fixture.issuer, clientId: "cito" });
    try {
      await expect(verifier.verify(await rsa.sign(fixture.claims()))).rejects.toMatchObject({ code: "AUTH_UNAVAILABLE" });
    } finally { await fixture.stop(); }
  }, 8000);
});
