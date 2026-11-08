import { afterEach, expect, test } from "vitest";
import { setTimeout as delay } from "node:timers/promises";
import { nativeFixture, object, type NativeHarness } from "../support/native-harness.js";

const fixtures: NativeHarness[] = [];
afterEach(async () => { for (const f of fixtures.splice(0).reverse()) await f.close(); });
test("native default draining acknowledges only the ordered page; ack_ids disables default draining and ack_all can union both", async () => {
  const f = await nativeFixture(); fixtures.push(f);
  const pi = await f.piBridge(); await f.daemon();
  const enrolled = await f.tool("cito_listen", { about: "pages" }, pi.meta);
  const rows = f.query("SELECT * FROM local_listeners WHERE session_id = ?", ["pi-session-1"]);
  expect(rows).toHaveLength(1);
  const listener = rows[0]!;
  expect(enrolled.listener_id).toBe(listener.public_id);
  // Keep background wake delivery paused while testing inbox page selection.
  f.mutate("INSERT OR REPLACE INTO wake_retries(hub,listener_id,retry_at_ms) VALUES(?,?,?)", [String(listener.hub), String(listener.listener_id), Date.now() + 60_000]);
  // Seed production SQLite, then exercise the native inbox and receiver. No test-side admission engine.
  for (let seq = 55; seq > 0; seq--) {
    f.mutate(`INSERT INTO local_signals
      (hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_harness,from_listener_id,from_title,from_about,message,held_reason,in_reply_to,created_at,expires_at_ms,state)
      VALUES (?, ?, ?, ?, ?, 'alice', 'other', NULL, NULL, '', NULL, ?, NULL, NULL, '2026-01-01T00:00:00Z', ?, 'RECEIVED')`,
    [String(listener.hub), "sig_" + seq, String(listener.listener_id), seq, Number(listener.binding_generation), "message " + seq, Date.now() + 60_000]);
  }
  const selected = await f.tool("cito_inbox", { ack_ids: ["sig_55"] }, pi.meta);
  const sequences = (value: unknown) => {
    if (!Array.isArray(value)) throw new Error("Inbox signals must be an array");
    return value.map(row => object(row).seq);
  };
  expect(sequences(selected.signals)).toEqual(Array.from({ length: 50 }, (_, n) => n + 1));
  expect(selected.acknowledged_count).toBe(1);
  const first = await f.tool("cito_inbox", {}, pi.meta);
  expect(first.acknowledged_count).toBe(50); expect(first.signals).toHaveLength(50);
  const union = await f.tool("cito_inbox", { ack_ids: ["sig_55"], ack_all: true }, pi.meta);
  expect(sequences(union.signals)).toEqual([51, 52, 53, 54]);
  expect(union.acknowledged_count).toBe(4);
  expect((await f.tool("cito_inbox", {}, pi.meta)).signals).toEqual([]);
  const stored = f.query("SELECT * FROM local_signals");
  expect(stored).toHaveLength(55);
  expect(stored.every(row => row.state === "STORED_PASSIVE" && typeof row.acknowledged_at_ms === "number")).toBe(true);
  await delay(2_200); expect(pi.messages).toEqual([]);
});
