import { describe, expect, it } from "vitest";
import { connect, StringCodec, provisionNats, brokerNow, connectNats } from "../../src/shared/nats.js";
import { startNats } from "./nats-server.js";

describe("official local NATS JetStream + KV", () => {
  it("provisions concurrently and idempotently, retaining KV generations and deduplicating signals", async () => {
    const broker = await startNats();
    const connections = await Promise.all(Array.from({ length: 3 }, () => connect({ servers: broker.url })));
    try {
      const [first] = await Promise.all(connections.map(provisionNats));
      const { js, jsm, kv } = first!;
      const sc = StringCodec();
      const rev = await kv.create("c_abcdefgh2345", sc.encode(JSON.stringify({ binding_generation: 1 })));
      await kv.update("c_abcdefgh2345", sc.encode(JSON.stringify({ binding_generation: 2 })), rev);
      await expect(kv.update("c_abcdefgh2345", sc.encode("{}"), rev)).rejects.toThrow();
      const again = await provisionNats(connections[0]!);
      expect(sc.decode((await again.kv.get("c_abcdefgh2345"))!.value)).toContain('"binding_generation":2');
      const config = (await jsm.streams.info("CITO_SIGNALS")).config;
      expect(config).toMatchObject({
        subjects: ["cito.signal.*"], retention: "limits", storage: "file", discard: "old",
        max_age: 1_800_000_000_000, duplicate_window: 1_800_000_000_000,
      });
      const kvConfig = (await jsm.streams.info("KV_cito_listeners")).config;
      expect(kvConfig).toMatchObject({ max_msgs_per_subject: 1, max_age: 0, storage: "file" });
      const a = await js.publish("cito.signal.c_abcdefgh2345", sc.encode("message"), { msgID: "same" });
      const b = await js.publish("cito.signal.c_abcdefgh2345", sc.encode("message"), { msgID: "same" });
      expect(b.seq).toBe(a.seq);
      expect(b.duplicate).toBe(true);
      const time = await brokerNow(jsm);
      expect(Number.isFinite(Date.parse(time))).toBe(true);
      const entry = await kv.get("c_abcdefgh2345");
      expect(Date.parse(time)).toBeGreaterThanOrEqual(entry!.created.getTime());
    } finally {
      await Promise.all(connections.map(nc => nc.close()));
      await broker.stop();
    }
  });
  it("rejects incompatible existing resources without destroying data or rewriting policy", async () => {
    const broker = await startNats();
    const nc = await connect({ servers: broker.url });
    try {
      const { jsm } = await provisionNats(nc);
      await jsm.streams.update("KV_cito_listeners", { max_age: 300_000_000_000 });
      await expect(provisionNats(nc)).rejects.toThrow("Incompatible Hub stream KV_cito_listeners");
      expect((await jsm.streams.info("KV_cito_listeners")).config.max_age).toBe(300_000_000_000);
      await jsm.streams.update("KV_cito_listeners", { max_age: 0 });
      await jsm.streams.update("CITO_SIGNALS", { max_age: 3_600_000_000_000 });
      await expect(provisionNats(nc)).rejects.toThrow("Incompatible Hub stream CITO_SIGNALS");
    } finally { await nc.close(); await broker.stop(); }
  });
  it("connects over the Node native WebSocket transport using the same JetStream APIs", async () => {
    const broker = await startNats("websocket {\n host: \"127.0.0.1\"\n port: -1\n no_tls: true\n}");
    let nc;
    try {
      expect(broker.wsUrl).toBeTruthy();
      nc = await connectNats({ servers: broker.wsUrl! });
      const { js } = await provisionNats(nc);
      expect((await js.publish("cito.signal.c_abcdefgh2345", new Uint8Array())).seq).toBe(1);
    } finally { await nc?.close(); await broker.stop(); }
  });
});
