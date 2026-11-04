import { afterEach, describe, expect, test } from "vitest";
import { randomUUID } from "node:crypto";
import { uptime } from "node:os";
import { fixture, call } from "./support.js";
import { type Signal } from "../../src/mcp/state.js";

const fixtures: Awaited<ReturnType<typeof fixture>>[] = [];
afterEach(async () => { for (const f of fixtures.splice(0)) await f.close(); });
async function enrolled() {
  const f = await fixture(); fixtures.push(f);
  const listening = await call(f.service, "cito_listen", { about: "receiver", scope: "workspace" }, f.pi.meta);
  const id = String(listening.listener_id), state = f.service.state;
  const row = state.listener(id)!;
  let seq = 0;
  const admit = (overrides: Partial<Signal> = {}, sender = "none#alice|host|unbound|default") => {
    const signal: Signal = { signal_id: "sig_" + randomUUID(), to: id, target_generation: row.binding_generation,
      sender_endpoint_key: sender, from: { principal_id: "none#alice", user: "alice", host_id: "host", host_name: "sender-host",
        harness: null, session_id: null, listener_id: null, title: "sender", about: null },
      message: "notification", requested_wake: true, in_reply_to: null, ...overrides };
    return { signal, result: state.admit(id, row.owner_token, signal, ++seq, new Date().toISOString(), Date.now() + 1_800_000, Math.floor(uptime() * 1000)) };
  };
  return { f, id, state, row, admit };
}

describe("atomic Stage-1 admission", () => {
  test("inbox selects the first 50 sequences and ignores unknown acknowledgement IDs", async () => {
    const { f, admit } = await enrolled();
    for (let n = 1; n <= 60; n++) admit({ requested_wake: false, message: "message-" + n });
    const first = await call(f.service, "cito_inbox", { ack_all: true, ack_ids: ["unknown"] }, f.pi.meta);
    expect(first.acknowledged_count).toBe(50);
    expect(first.signals.map((signal: { seq: number }) => signal.seq)).toEqual(Array.from({ length: 50 }, (_, index) => index + 1));
    const remaining = await call(f.service, "cito_inbox", {}, f.pi.meta);
    expect(remaining.signals.map((signal: { seq: number }) => signal.seq)).toEqual([51, 52, 53, 54, 55, 56, 57, 58, 59, 60]);
  });
  test("listener-specific retention gaps preserve quota at >30m and clear only with ack_all", async () => {
    const { f, id, state, admit } = await enrolled();
    admit(); admit(); admit({}, "other");
    const now = new Date().toISOString();
    state.recordOfflineGap(id, now, new Date(Date.parse(now) - 1_800_000));
    expect(state.listener(id)!.replay_gap_detected).toBe(0);
    state.recordOfflineGap(id, now, new Date(Date.parse(now) - 1_800_001));
    expect(state.listener(id)).toMatchObject({ replay_gap_detected: 1, replay_gap_range: "offline_exceeded_30m", unattended_wake_count: 3 });
    const relisten = await call(f.service, "cito_listen", { about: "retained" }, f.pi.meta);
    expect(relisten.unattended_wakes_remaining).toBe(0);
    expect(await call(f.service, "cito_inbox", {}, f.pi.meta)).toMatchObject({ replay_gap_detected: true, replay_gap_range: "offline_exceeded_30m" });
    expect(await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta)).toMatchObject({ replay_gap_detected: true, acknowledged_count: 3 });
    expect(await call(f.service, "cito_inbox", {}, f.pi.meta)).toMatchObject({ replay_gap_detected: false, replay_gap_range: null });
  });
  test("rolling pairwise window expires independently of persistent unattended quota", async () => {
    const { id, state, admit } = await enrolled();
    admit(); admit();
    state.run("UPDATE local_pairwise_wakes SET admitted_at_ms = admitted_at_ms - 120001");
    expect(admit().result.row).toMatchObject({ effective_wake: 1, held_reason: null });
    expect(state.listener(id)!.unattended_wake_count).toBe(3);
    expect(state.db.prepare("SELECT COUNT(*) AS n FROM local_pairwise_wakes").get()!.n).toBe(1);
  });
  test("redelivery reuses result, advances cursor, and never double-charges", async () => {
    const { f, id, state, row, admit } = await enrolled();
    const { signal, result } = admit();
    expect(result.row).toMatchObject({ state: "RECEIVED", effective_wake: 1, held_reason: null });
    const redelivered = state.admit(id, row.owner_token, signal, 99, new Date().toISOString(), 0, 0);
    expect(redelivered).toMatchObject({ duplicate: true, row: { signal_id: signal.signal_id, effective_wake: 1, held_reason: null } });
    expect(state.listener(id)).toMatchObject({ unattended_wake_count: 1, persisted_seq: 99 });
    expect(state.db.prepare("SELECT COUNT(*) AS n FROM local_pairwise_wakes").get()!.n).toBe(1);
    expect(f.pi.messages).toEqual([]);
  });
  test("pairwise threshold is recipient-local; quota wins after three admissions", async () => {
    const { id, state, admit } = await enrolled();
    expect(admit().result.row!.effective_wake).toBe(1);
    expect(admit().result.row!.effective_wake).toBe(1);
    expect(admit().result.row).toMatchObject({ effective_wake: 0, held_reason: "pairwise_rate_limited", state: "STORED_PASSIVE" });
    expect(admit({}, "other-sender").result.row!.effective_wake).toBe(1);
    expect(admit().result.row).toMatchObject({ effective_wake: 0, held_reason: "wake_quota_exhausted" });
    expect(state.listener(id)!.unattended_wake_count).toBe(3);
    expect(state.db.prepare("SELECT COUNT(*) AS n FROM local_pairwise_wakes").get()!.n).toBe(3);
  });
  test("all five suppression levels have deterministic precedence", async () => {
    const { id, row, state, admit } = await enrolled();
    admit(); admit();
    state.run("UPDATE local_listeners SET binding_generation = binding_generation + 1, unattended_wake_count = 3 WHERE listener_id = ?", id);
    const peer = { principal_id: "none#bob", user: "bob", host_id: "h", host_name: "h", harness: null, session_id: null, listener_id: null, title: "", about: null };
    expect(admit({ requested_wake: false, from: peer }).result.row!.held_reason).toBe("stale_generation");
    expect(admit({ target_generation: row.binding_generation + 1, requested_wake: false, from: peer }).result.row!.held_reason).toBe("passive_requested");
    expect(admit({ target_generation: row.binding_generation + 1, from: peer }).result.row!.held_reason).toBe("peer_wake_disabled");
    expect(admit({ target_generation: row.binding_generation + 1 }).result.row!.held_reason).toBe("wake_quota_exhausted");
    state.run("UPDATE local_listeners SET unattended_wake_count = 2 WHERE listener_id = ?", id);
    expect(admit({ target_generation: row.binding_generation + 1 }).result.row!.held_reason).toBe("pairwise_rate_limited");
  });
  test("stale generations remain readable; unauthorized private signals only advance cursor", async () => {
    const { f, id, state, admit } = await enrolled();
    state.run("UPDATE local_listeners SET binding_generation = binding_generation + 1 WHERE listener_id = ?", id);
    const stale = admit().result.row!;
    expect(stale).toMatchObject({ state: "STORED_PASSIVE", held_reason: "stale_generation" });
    expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toMatchObject([{ signal_id: stale.signal_id, held_reason: "stale_generation" }]);
    state.run("UPDATE local_listeners SET scope = 'self' WHERE listener_id = ?", id);
    const denied = admit({ from: { principal_id: "none#bob", user: "bob", host_id: "", host_name: "", harness: null, session_id: null, listener_id: null, title: "", about: null } });
    expect(denied.result.row).toBeUndefined();
    expect(state.listener(id)).toMatchObject({ persisted_seq: 2, unattended_wake_count: 0 });
    expect(state.signals("SELECT * FROM local_signals")).toHaveLength(1);
  });
  test("Pi epoch deltas reject retired observations and bound history; human events alone reset counters", async () => {
    const { f, id, state, admit } = await enrolled();
    admit(); admit();
    const epoch = state.listener(id)!.last_bridge_epoch!;
    state.transaction(() => { state.observePi(id, "restart-1", 0); state.consumeHuman(id); });
    expect(state.listener(id)).toMatchObject({ human_event_seq: 0, unattended_wake_count: 2 });
    state.transaction(() => { state.observePi(id, "restart-1", 1); state.consumeHuman(id); });
    expect(state.listener(id)).toMatchObject({ human_event_seq: 1, last_consumed_human_seq: 1, unattended_wake_count: 0 });
    state.observePi(id, epoch, 100);
    state.observePi(id, "restart-1", 0);
    expect(state.listener(id)).toMatchObject({ human_event_seq: 1, last_bridge_count: 1 });
    expect(state.db.prepare("SELECT COUNT(*) AS n FROM local_pairwise_wakes").get()!.n).toBe(0);
    for (let n = 2; n < 40; n++) state.observePi(id, "restart-" + n, 0);
    expect(JSON.parse(state.listener(id)!.retired_bridge_epochs_json)).toHaveLength(32);
    f.pi.input("extension");
    await f.service.receivers.get(id)!.sweep();
    expect(state.listener(id)!.human_event_seq).toBe(1);
    f.pi.input();
    await f.service.receivers.get(id)!.sweep();
    expect(state.listener(id)!.human_event_seq).toBe(2);
  });
  test("quota and preferences survive unlisten/relisten; verified human sequence consumption resets quota", async () => {
    const { f, id, state, admit } = await enrolled();
    admit(); admit(); admit({}, "other");
    await call(f.service, "cito_unlisten", {}, f.pi.meta);
    const again = await call(f.service, "cito_listen", { about: "updated" }, f.pi.meta);
    expect(again).toMatchObject({ listener_id: id, binding_generation: 3, scope: "workspace", allow_peer_wake: false, unattended_wakes_remaining: 0 });
    state.run("UPDATE local_listeners SET human_event_seq = human_event_seq + 1 WHERE listener_id = ?", id);
    const human = await call(f.service, "cito_listen", { about: "human seen", allow_peer_wake: true }, f.pi.meta);
    expect(human).toMatchObject({ listener_id: id, scope: "workspace", allow_peer_wake: true, unattended_wakes_remaining: 3 });
  });
});
