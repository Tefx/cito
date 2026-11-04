import { afterEach, expect, test, vi } from "vitest";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { fixture, call, payload, waitFor, codexDesktop } from "./support.js";
import { unixServer } from "../adapters/support.js";
import { connectNats, provisionNats, listenerId, toCanonicalListenerId, publicListenerId, listenerSlug } from "../../src/shared/index.js";
import { encode, type Broker } from "../../src/mcp/receiver.js";
import { registrationSchema } from "../../src/mcp/state.js";
import { CitoService } from "../../src/mcp/service.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
async function setup(pi = true) { const f = await fixture({ pi }); cleanups.push(() => f.close()); return f; }

test("restart restores persisted ACTIVE ownership, frozen dispatch and queued pull without re-enrolling", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "restart" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id);
  const sent = await call(f.service, "cito_send", { to: id, message: "frozen" });
  const owner = f.service.state.listener(id)!.owner_token;
  f.service.state.run("UPDATE local_signals SET state = 'DISPATCHING', batch_id = 'frozen-before-restart', batch_expires_at_ms = expires_at_ms, batch_card = 'Original frozen card' WHERE signal_id = ?", sent.signal_id);
  f.service.state.run("UPDATE local_listeners SET dispatcher_token = 'crashed-dispatcher', lease_until_ms = ? WHERE listener_id = ?", Date.now() + 60_000, id);
  const observer = await CitoService.create(f.config, f.serviceOptions);
  try {
    await call(observer, "cito_find");
    expect(observer.receivers.has(id)).toBe(false);
    expect(observer.state.listener(id)!.dispatcher_token).toBe("crashed-dispatcher");
  } finally { await observer.close(); }
  await f.service.close();
  const nc = await connectNats({ servers: f.broker.url }); cleanups.push(() => nc.close());
  const { js } = await provisionNats(nc);
  await js.publish("cito.signal." + id, encode({ signal_id: "queued-during-restart", to: id, target_generation: 1,
    sender_endpoint_key: "none#alice|other|unbound|default", from: { principal_id: "none#alice", user: "alice", host_id: "other", host_name: "other", harness: null },
    message: "queued offline", requested_wake: false, in_reply_to: null }));
  const restarted = await CitoService.create(f.config, f.serviceOptions); cleanups.push(() => restarted.close());
  expect((await call(restarted, "cito_find")).listeners).toMatchObject([{ listener_id: enrolled.listener_id }]);
  await waitFor(() => restarted.state.listener(id)!.persisted_seq === 2 && restarted.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 1);
  expect(restarted.state.listener(id)).toMatchObject({ status: "ACTIVE", owner_token: owner, binding_generation: 1, unattended_wake_count: 1 });
  expect(f.pi.messages.map(message => message.content)).toEqual(["Original frozen card"]);
  expect((await call(restarted, "cito_inbox", {}, f.pi.meta)).signals).toMatchObject([{ signal_id: sent.signal_id }, { signal_id: "queued-during-restart", message: "queued offline" }]);
}, 10_000); // Includes first-use Swift compilation and native startup under parallel test load.

test("unlisten cleanup cannot delete the concurrently installed replacement receiver", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "old" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const old = f.service.receivers.get(id)!, stop = old.stop.bind(old);
  let entered!: () => void, release!: () => void;
  const stopping = new Promise<void>(resolve => { entered = resolve; });
  const gate = new Promise<void>(resolve => { release = resolve; });
  vi.spyOn(old, "stop").mockImplementationOnce(async () => { await stop(); entered(); await gate; });
  const revocation = call(f.service, "cito_unlisten", {}, f.pi.meta);
  try {
    await stopping;
    await call(f.service, "cito_listen", { about: "replacement" }, f.pi.meta);
    const replacement = f.service.receivers.get(id)!;
    expect(replacement).not.toBe(old);
    release(); await revocation;
    expect(f.service.receivers.get(id)).toBe(replacement);
    expect(replacement.abort.signal.aborted).toBe(false);
    expect(await call(f.service, "cito_send", { to: id, message: "still receiving" })).toMatchObject({ status: "wake_accepted" });
  } finally { release(); await revocation; }
});

test("a transient revocation failure restores the ACTIVE receiver and permits a successful retry", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "retry revoke" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service, "broker") as Broker;
  vi.spyOn(broker.kv, "update").mockRejectedValueOnce(new Error("temporary broker failure"));
  expect(payload(await f.service.call("cito_unlisten", {}, f.pi.meta))).toMatchObject({ error: { code: "NATS_UNAVAILABLE", retryable: true } });
  expect(f.service.state.listener(id)!.status).toBe("ACTIVE");
  expect(registrationSchema.parse((await broker.kv.get(id))!.json()).state).toBe("ACTIVE");
  expect(f.service.receivers.get(id)!.abort.signal.aborted).toBe(false);
  expect(await call(f.service, "cito_send", { to: id, message: "recovered receiver" })).toMatchObject({ status: "wake_accepted" });
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened" });
  expect(f.service.receivers.has(id)).toBe(false);
});

test("Pi A to B to A preserves A's human watermark through re-enrollment and sweep", async () => {
  const f = await setup();
  const { listener_id: aPublic } = await call(f.service, "cito_listen", { about: "A" }, f.pi.meta);
  const a = toCanonicalListenerId(aPublic);
  f.pi.input(); f.pi.input(); f.pi.input();
  await f.service.receivers.get(a)!.sweep();
  await call(f.service, "cito_send", { to: a, message: "charge A" });
  f.pi.switchSession("B"); f.pi.input(); f.pi.input();
  const { listener_id: bPublic } = await call(f.service, "cito_listen", { about: "B" }, f.pi.meta);
  const b = toCanonicalListenerId(bPublic);
  f.pi.switchSession("pi-session-1"); f.pi.input();
  await call(f.service, "cito_listen", { about: "A resumed" }, f.pi.meta);
  expect(f.service.state.listener(a)).toMatchObject({ human_event_seq: 4, last_bridge_count: 4, last_consumed_human_seq: 4, unattended_wake_count: 0 });
  await call(f.service, "cito_send", { to: a, message: "charge resumed A" });
  f.pi.input(); await f.service.receivers.get(a)!.sweep();
  expect(f.service.state.listener(a)).toMatchObject({ human_event_seq: 5, last_bridge_count: 5, unattended_wake_count: 0 });
  expect(f.service.state.listener(b)).toMatchObject({ human_event_seq: 2, last_bridge_count: 2 });
});

test.each(["flush", "ack"])("shutdown interrupts a stalled send %s boundary without waiting for its deadline", async boundary => {
  const target = await setup();
  const { listener_id: publicId } = await call(target.service, "cito_listen", { about: "offline recipient" }, target.pi.meta);
  const id = toCanonicalListenerId(publicId);
  await target.service.receivers.get(id)!.stop();
  const sender = await fixture({ broker: target.broker, pi: false }); cleanups.push(() => sender.close());
  await call(sender.service, "cito_find");
  const broker = Reflect.get(sender.service, "broker") as Broker;
  const flush = boundary === "flush" ? vi.spyOn(broker.nc, "flush").mockImplementation(() => new Promise(() => {})) : undefined;
  const pending = sender.service.call("cito_send", { to: id, message: "blocked send" });
  if (flush) await waitFor(() => flush.mock.calls.length === 1);
  else await waitFor(async () => (await broker.jsm.streams.info("CITO_SIGNALS")).state.messages === 1);
  const began = performance.now();
  await sender.service.close();
  expect(performance.now() - began).toBeLessThan(1000);
  expect(payload(await pending)).toMatchObject({ error: { code: "NATS_UNAVAILABLE", retryable: true } });
}, 5000);

test("a stalled flush is bounded by the ten-second NATS operation deadline", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "flush deadline" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service, "broker") as Broker;
  vi.spyOn(broker.nc, "flush").mockImplementation(() => new Promise(() => {}));
  expect(payload(await f.service.call("cito_send", { to: id, message: "never flushed" }))).toMatchObject({ error: { code: "NATS_UNAVAILABLE", retryable: true } });
  expect((await broker.jsm.streams.info("CITO_SIGNALS")).state.messages).toBe(0);
  expect(f.service.state.db.prepare("SELECT status FROM local_sent_idempotency").get()?.status).toBe("PENDING_PUBLISH");
}, 13_000);

test("20-second scheduled sweep consumes human input without explicit test invocation", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "scheduled sweep" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "charge" });
  f.pi.input();
  await waitFor(() => f.service.state.listener(id)!.human_event_seq === 1, 23_000);
  expect(f.service.state.listener(id)).toMatchObject({ unattended_wake_count: 0, last_consumed_human_seq: 1, consecutive_probe_failures: 0, status: "ACTIVE" });
}, 26_000);

test("transient Pi probes revoke on third failure, whereas healthy probe resets failure streak", async () => {
  const f = await setup(false);
  let healthy = true;
  const server = await unixServer(join(f.stateDir, `sockets/pi-pid-${process.pid}.sock`), socket => {
    socket.once("data", bytes => {
      const request = JSON.parse(bytes.toString());
      socket.end(JSON.stringify(request.op !== "identify" ? { ok: true } : healthy ? {
        ok: true, pi_pid: process.pid, session_id: "transient-pi", session_name: "probe", cwd: f.root,
        is_streaming: false, bridge_epoch: "epoch", human_event_count: 0,
      } : { ok: false, error: "TEMPORARY" }) + "\n");
    });
  }); cleanups.push(() => server.stop());
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "three strikes" }, { pi_pid: process.pid });
  const id = toCanonicalListenerId(publicId);
  const receiver = f.service.receivers.get(id)!;
  healthy = false; await receiver.sweep(); await receiver.sweep();
  expect(f.service.state.listener(id)).toMatchObject({ status: "ACTIVE", consecutive_probe_failures: 2 });
  healthy = true; await receiver.sweep();
  expect(f.service.state.listener(id)!.consecutive_probe_failures).toBe(0);
  healthy = false; await receiver.sweep(); await receiver.sweep(); await receiver.sweep();
  expect(f.service.state.listener(id)).toMatchObject({ status: "UNLISTENED", consecutive_probe_failures: 3 });
  expect((await call(f.service, "cito_find")).listeners).toEqual([]);
});

test("Codex definitive no-client-found revokes immediately and preserves unread passive signals", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "Codex closure" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "retained", wake: false });
  desktop.setLive(false); await f.service.receivers.get(id)!.sweep();
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect((await call(f.service, "cito_inbox", {}, desktop.meta)).signals).toMatchObject([{ message: "retained" }]);
});

test("CAS retry exhaustion from real competing KV writes fences failed contender without advancing local generation", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "CAS" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service, "broker") as Broker;
  const peer = await connectNats({ servers: f.broker.url }); cleanups.push(() => peer.close());
  const { kv } = await provisionNats(peer);
  const original = broker.kv.update.bind(broker.kv);
  let conflicts = 0;
  // Scheduling barrier only: each competitor CAS and each failing contender CAS executes on NATS.
  const spy = vi.spyOn(broker.kv, "update").mockImplementation(async (key, data, revision) => {
    const entry = (await kv.get(key))!, value = registrationSchema.parse(entry.json());
    await kv.update(key, encode({ ...value, binding_generation: value.binding_generation + 1, owner_token: randomUUID() }), entry.revision);
    conflicts++;
    return original(key, data, revision);
  });
  const result = await f.service.call("cito_listen", { about: "loser" }, f.pi.meta);
  spy.mockRestore();
  expect(payload(result)).toMatchObject({ error: { code: "STALE_GENERATION", retryable: true } });
  expect(conflicts).toBe(3);
  expect(f.service.state.listener(id)).toMatchObject({ status: "FENCED_INACTIVE", binding_generation: 1 });
  expect(registrationSchema.parse((await kv.get(id))!.json()).binding_generation).toBe(4);
});

test("find drains a multi-key KV snapshot before per-entry reads and applies the 50-result limit after sorting", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "many leases" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service, "broker") as Broker;
  const base = registrationSchema.parse((await broker.kv.get(id))!.json());
  const ids: string[] = [];
  for (let n = 0; n < 60; n++) {
    const session = "other-session-" + n;
    const key = listenerId({ ...base, session_id: session }); ids.push(key);
    await broker.kv.create(key, encode({ ...base, public_id: undefined, listener_id: key, session_id: session, owner_token: randomUUID() }));
  }
  const result = await call(f.service, "cito_find", { limit: 50 });
  expect(result.listeners.map((entry: { listener_id: string }) => entry.listener_id)).toEqual(ids.reverse().slice(0, 50).map(key => publicListenerId(listenerSlug(base), key)));
});

test("a second MCP process can revoke the active owner without losing unlisten suppression reason", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "remote revoke" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const sent = await call(f.service, "cito_send", { to: id, message: "pending" });
  const other = await CitoService.create(f.config, f.serviceOptions); cleanups.push(() => other.close());
  expect(await call(other, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened" });
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "unlistened_before_dispatch" });
  expect(f.pi.messages).toEqual([]);
});

test("reconnect creates a fresh ephemeral consumer from persisted cursor without double charging", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "reconnect" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "first", wake: false });
  const original = f.service.receivers.get(id)!;
  const broker = Reflect.get(f.service, "broker") as Broker;
  await broker.nc.reconnect();
  await waitFor(() => f.service.receivers.get(id) !== original && f.service.receivers.get(id)?.abort.signal.aborted === false);
  const next = await call(f.service, "cito_send", { to: id, message: "second" });
  expect(next).toMatchObject({ status: "wake_accepted", seq: 2 });
  expect(f.service.state.listener(id)).toMatchObject({ persisted_seq: 2, unattended_wake_count: 1 });
  expect(f.service.state.signals("SELECT * FROM local_signals ORDER BY seq").map(row => row.message)).toEqual(["first", "second"]);
}, 10_000);

test("admission failure never advances restart cursor past the failed earlier message", async () => {
  const f = await setup();
  const { listener_id: publicId, binding_generation: generation } = await call(f.service, "cito_listen", { about: "ordered admission" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service, "broker") as Broker;
  const original = f.service.state.admit.bind(f.service.state), order: string[] = [];
  let attempts = 0;
  // Inject a transaction failure, not a successful fake admission. All commits remain real SQLite.
  vi.spyOn(f.service.state, "admit").mockImplementation((...args) => {
    if (args[2].signal_id === "first" && attempts++ === 0) throw new Error("interrupted admission");
    const result = original(...args); order.push(args[2].signal_id); return result;
  });
  const wire = { to: id, target_generation: generation, sender_endpoint_key: "none#alice|sender|unbound|default",
    from: { principal_id: "none#alice", user: "alice", host_id: "sender", host_name: "sender", harness: null },
    message: "ordered", requested_wake: false, in_reply_to: null };
  await broker.js.publish("cito.signal." + id, encode({ ...wire, signal_id: "first" }));
  await broker.js.publish("cito.signal." + id, encode({ ...wire, signal_id: "second" }));
  await waitFor(() => f.service.state.signals("SELECT * FROM local_signals").length === 2);
  expect(order).toEqual(["first", "second"]);
  expect(f.service.state.listener(id)!.persisted_seq).toBe(2);
  expect(f.service.state.signals("SELECT * FROM local_signals ORDER BY seq").map(row => row.signal_id)).toEqual(["first", "second"]);
});

test("disconnected recipient returns the exact sender timeout milestone and passive reason", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "disconnected", scope: "workspace" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  await f.service.receivers.get(id)!.stop();
  const sender = await fixture({ broker: f.broker, pi: false }); cleanups.push(() => sender.close());
  expect(await call(sender.service, "cito_send", { to: id, message: "buffered" })).toMatchObject({ status: "queued_for_reconnect", effective_wake: true, held_reason: null });
  expect(await call(sender.service, "cito_send", { to: id, message: "buffered passive", wake: false })).toMatchObject({ status: "stored_passive", effective_wake: false, held_reason: "passive_requested" });
  expect(f.service.state.listener(id)!.persisted_seq).toBe(0);
}, 10_000);
