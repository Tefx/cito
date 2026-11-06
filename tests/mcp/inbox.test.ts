import { afterEach, expect, test } from "vitest";
import { fixture, call } from "./support.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import type { Signal } from "../../src/mcp/state.js";

const fixtures: Awaited<ReturnType<typeof fixture>>[] = [];
afterEach(async () => { for (const f of fixtures.splice(0)) await f.close(); });
test("default draining acknowledges only the ordered page; ack_ids disables default draining and ack_all can union both", async () => {
  const f = await fixture(); fixtures.push(f);
  const enrolled = await call(f.service, "cito_listen", { about: "pages" }, f.pi.meta), id = toCanonicalListenerId(enrolled.listener_id);
  const listener = f.service.state.listener(id)!;
  for (let seq = 55; seq > 0; seq--) {
    const wire: Signal = { signal_id: "sig_" + seq, to: id, target_generation: 1, sender_endpoint_key: "sender",
      from: { principal_id: listener.principal_id, user: "alice", host_id: "other", host_name: "other", harness: null, listener_id: null, title: "", about: null },
      message: "message " + seq, in_reply_to: null };
    f.service.state.admit(id, listener.owner_token, wire, seq, "2026-01-01T00:00:00Z", Date.now() + 60_000);
  }
  const selected = await call(f.service, "cito_inbox", { ack_ids: ["sig_55"] }, f.pi.meta);
  expect(selected.signals.map((row: { seq: number }) => row.seq)).toEqual(Array.from({ length: 50 }, (_, n) => n + 1));
  expect(selected.acknowledged_count).toBe(1);
  const first = await call(f.service, "cito_inbox", {}, f.pi.meta);
  expect(first.acknowledged_count).toBe(50); expect(first.signals).toHaveLength(50);
  const union = await call(f.service, "cito_inbox", { ack_ids: ["sig_55"], ack_all: true }, f.pi.meta);
  expect(union.signals.map((row: { seq: number }) => row.seq)).toEqual([51, 52, 53, 54]);
  expect(union.acknowledged_count).toBe(4);
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
  expect(f.service.state.signals("SELECT * FROM local_signals").every(row => row.state === "STORED_PASSIVE" && row.acknowledged_at_ms !== null)).toBe(true);
  await f.service.receivers.get(`default:${id}`)!.dispatch(); expect(f.pi.messages).toEqual([]);
});
