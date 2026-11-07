import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { AckPolicy, StringCodec, connectNats, provisionNats } from "../../src/shared/nats.js";
import { startHubServer, type HubAuditEvent } from "../../src/hub/index.js";
import { signingKey, startOidc } from "./oidc-fixture.js";

const sc = StringCodec();

describe("HubServer OIDC, Token, and Row-Level Security (RLS) enforcement", () => {
  let key: Awaited<ReturnType<typeof signingKey>>;
  let oidc: Awaited<ReturnType<typeof startOidc>>;
  beforeAll(async () => {
    key = await signingKey("RS256", "rsa");
    oidc = await startOidc([key.jwk]);
  });
  afterAll(async () => {
    await oidc.stop();
  });

  it("verifies pre-shared bearer tokens in token mode and rejects missing or wrong tokens", async () => {
    const hub = await startHubServer({ authMode: "token", token: "secret-shared-key" });
    let client: Awaited<ReturnType<typeof connectNats>> | undefined;
    try {
      await expect(connectNats({ servers: hub.url, timeout: 2000 })).rejects.toThrow(/Authorization Violation/i);
      await expect(connectNats({ servers: hub.url, token: "wrong-token", timeout: 2000 })).rejects.toThrow(/Authorization Violation/i);
      client = await connectNats({ servers: hub.url, token: "secret-shared-key", user: "alice", timeout: 2000 });
      const { kv, js } = await provisionNats(client);
      const rev = await kv.create("c_abcdefgh2345", sc.encode(JSON.stringify({ principal_id: "none#alice", scope: "workspace", state: "ACTIVE" })));
      expect(rev).toBe(1);
      expect((await js.publish("cito.signal.c_abcdefgh2345", sc.encode("hello"))).seq).toBe(1);
    } finally {
      await client?.close();
      await hub.stop();
    }
  });

  it("returns transient AUTH_UNAVAILABLE on OIDC discovery failure instead of crashing", async () => {
    const failingOidc = await startOidc();
    failingOidc.state.fail = true;
    const hub = await startHubServer({ authMode: "oidc", oidcIssuer: failingOidc.issuer, oidcClientId: "cito" });
    try {
      const token = await key.sign(failingOidc.claims());
      await expect(connectNats({ servers: hub.url, token, timeout: 2000 })).rejects.toMatchObject({
        code: "AUTH_UNAVAILABLE",
      });
    } finally {
      await hub.stop();
      await failingOidc.stop();
    }
  });

  it("authorizes OIDC tokens, enforces Row-Level Security (RLS) on KV and Signals, blocks unauthorized subjects, and records audit logs", async () => {
    const dir = await mkdtemp(join(tmpdir(), "cito-rls-"));
    const auditLogPath = join(dir, "audit.jsonl");
    const audits: HubAuditEvent[] = [];
    const errors: Error[] = [];
    const hub = await startHubServer({
      authMode: "oidc",
      oidcIssuer: oidc.issuer,
      oidcClientId: "cito",
      dataDir: join(dir, "hub-data"),
      auditLogPath,
      onAudit: ev => audits.push(ev),
      onError: err => errors.push(err),
    });

    let aliceNc: Awaited<ReturnType<typeof connectNats>> | undefined;
    let bobNc: Awaited<ReturnType<typeof connectNats>> | undefined;
    try {
      const aliceToken = await key.sign(oidc.claims({ sub: "user-1", preferred_username: "Alice" }));
      const bobToken = await key.sign(oidc.claims({ sub: "user-2", preferred_username: "Bob" }));

      aliceNc = await connectNats({ servers: hub.url, token: aliceToken, timeout: 2000 });
      bobNc = await connectNats({ servers: hub.url, token: bobToken, timeout: 2000 });

      const alice = await provisionNats(aliceNc);
      const bob = await provisionNats(bobNc);

      const alicePrincipal = oidc.issuer + "#user-1";
      const bobPrincipal = oidc.issuer + "#user-2";
      const aliceListenerId = "c_abcdefgh2345";
      const alicePrivateReg = {
        listener_id: aliceListenerId,
        principal_id: alicePrincipal,
        owner_user: "alice",
        host_id: "h-alice",
        host_name: "alice-mac",
        harness: "pi",
        session_id: "s-alice",
        cwd: "/Users/alice/secret-repo",
        repo: "acme/secret",
        branch: "main",
        title: "Secret Session",
        about: "Confidential task",
        scope: "self",
        state: "ACTIVE",
        binding_generation: 1,
        owner_token: randomUUID(),
      };

      // 1. Alice creates a self-scoped registration
      const rev1 = await alice.kv.create(aliceListenerId, sc.encode(JSON.stringify(alicePrivateReg)));
      expect(rev1).toBe(1);
      expect((await alice.kv.get(aliceListenerId))?.json()).toMatchObject({ title: "Secret Session", scope: "self" });

      // 2. Bob cannot read Alice's self-scoped registration via kv.get, kv.keys, or kv.watch
      expect(await bob.kv.get(aliceListenerId)).toBeNull();
      await expect(bob.kv.get(aliceListenerId, { checkForbiddenScope: true })).rejects.toMatchObject({
        code: "FORBIDDEN_TARGET_SCOPE",
      });
      const bobKeysIter = await bob.kv.keys();
      const bobKeys: string[] = [];
      for await (const k of bobKeysIter) bobKeys.push(k);
      expect(bobKeys).toEqual([]);

      let watchInitCalled = false;
      let bobWatch: Awaited<ReturnType<typeof bob.kv.watch>> | undefined;
      bobWatch = await bob.kv.watch({
        initializedFn: () => {
          watchInitCalled = true;
          bobWatch?.stop();
        },
      });
      if (watchInitCalled) bobWatch.stop();
      expect(watchInitCalled).toBe(true);
      const bobWatched: string[] = [];
      for await (const entry of bobWatch) bobWatched.push(entry.key);
      expect(bobWatched).toEqual([]);

      // 3. Bob cannot overwrite Alice's registration or forge Alice's principal_id
      await expect(
        bob.kv.update(aliceListenerId, sc.encode(JSON.stringify({ ...alicePrivateReg, principal_id: bobPrincipal })), rev1),
      ).rejects.toThrow(/PERMISSIONS_VIOLATION/i);
      await expect(
        bob.kv.put(aliceListenerId, sc.encode(JSON.stringify({ ...alicePrivateReg, principal_id: bobPrincipal }))),
      ).rejects.toThrow(/PERMISSIONS_VIOLATION/i);
      await expect(
        bob.kv.create("c_forged234567", sc.encode(JSON.stringify({ ...alicePrivateReg, listener_id: "c_forged234567", principal_id: alicePrincipal }))),
      ).rejects.toThrow(/PERMISSIONS_VIOLATION/i);

      // 4. Bob cannot publish a signal targeting Alice's self-scoped listener or forging Alice's sender principal_id
      await expect(
        bob.js.publish(
          "cito.signal." + aliceListenerId,
          sc.encode(JSON.stringify({ signal_id: "sig_1", to: aliceListenerId, from: { principal_id: bobPrincipal } })),
        ),
      ).rejects.toMatchObject({ code: "FORBIDDEN_TARGET_SCOPE" });
      await expect(
        bob.js.publish(
          "cito.signal." + aliceListenerId,
          sc.encode(JSON.stringify({ signal_id: "sig_2", to: aliceListenerId, from: { principal_id: alicePrincipal } })),
        ),
      ).rejects.toThrow(/PERMISSIONS_VIOLATION/i);

      // 5. Alice updates her registration to workspace scope; Bob can now read and signal it
      const rev2 = await alice.kv.update(
        aliceListenerId,
        sc.encode(JSON.stringify({ ...alicePrivateReg, scope: "workspace" })),
        rev1,
      );
      expect(rev2).toBe(2);
      expect((await bob.kv.get(aliceListenerId))?.json()).toMatchObject({ listener_id: aliceListenerId, scope: "workspace" });

      const pubRes = await bob.js.publish(
        "cito.signal." + aliceListenerId,
        sc.encode(JSON.stringify({ signal_id: "sig_ok", to: aliceListenerId, from: { principal_id: bobPrincipal } })),
      );
      expect(pubRes.seq).toBe(1);

      await alice.jsm.consumers.add("CITO_SIGNALS", {
        durable_name: "ack-proof",
        filter_subject: "cito.signal." + aliceListenerId,
        ack_policy: AckPolicy.Explicit,
      });
      const consumer = await alice.js.consumers.get("CITO_SIGNALS", "ack-proof");
      const message = await consumer.next({ expires: 1000 });
      expect(message).toBeTruthy();
      expect(await message!.ackAck()).toBe(true);
      await consumer.delete();

      // 6. Subject guard rejects subscriptions outside cito.signal.* and cito.ack.*
      const permissionDenied = new Promise<void>(resolveDenied => {
        const sub = aliceNc!.subscribe("outside.cito.scope", {
          callback: error => {
            if (error) {
              sub.unsubscribe();
              resolveDenied();
            }
          },
        });
      });
      await permissionDenied;

      // 7. Invalid / expired OIDC tokens are rejected at handshake
      for (const payload of [
        { aud: "wrong" },
        { iss: "https://wrong" },
        { exp: Math.floor(Date.now() / 1000) - 60 },
      ]) {
        await expect(connectNats({ servers: hub.url, token: await key.sign(oidc.claims(payload)), timeout: 2000 })).rejects.toThrow(
          /Authorization Violation/i,
        );
      }
      await expect(connectNats({ servers: hub.url, token: "bad", timeout: 2000 })).rejects.toThrow(/Authorization Violation/i);
      expect(errors).toEqual([]);

      // 8. Audit log recorded register and send events
      expect(audits.map(a => a.action)).toContain("register");
      expect(audits.map(a => a.action)).toContain("send");
      const auditLines = (await readFile(auditLogPath, "utf8")).trim().split("\n").map(l => JSON.parse(l));
      expect(auditLines.length).toBeGreaterThanOrEqual(2);
    } finally {
      await aliceNc?.close();
      await bobNc?.close();
      await hub.stop();
      await rm(dir, { recursive: true, force: true });
    }
  });
});
