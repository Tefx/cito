import { afterEach, expect, test, vi } from "vitest";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { fixture, call, payload, waitFor, codexDesktop } from "./support.js";
import { unixServer } from "../adapters/support.js";
import { connectNats, provisionNats, listenerId } from "../../src/shared/index.js";
import { encode, type Broker } from "../../src/mcp/receiver.js";
import { registrationSchema } from "../../src/mcp/state.js";
import { CitoService } from "../../src/mcp/service.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
async function setup(pi = true) { const f = await fixture({ pi }); cleanups.push(() => f.close()); return f; }

test("20-second scheduled sweep consumes human input without explicit test invocation", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "scheduled sweep" }, f.pi.meta);
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
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "three strikes" }, { pi_pid: process.pid });
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
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "Codex closure" }, desktop.meta);
  await call(f.service, "cito_send", { to: id, message: "retained", wake: false });
  desktop.setLive(false); await f.service.receivers.get(id)!.sweep();
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect((await call(f.service, "cito_inbox", {}, desktop.meta)).signals).toMatchObject([{ message: "retained" }]);
});

test("CAS retry exhaustion from real competing KV writes fences failed contender without advancing local generation", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "CAS" }, f.pi.meta);
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
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "many leases" }, f.pi.meta);
  const broker = Reflect.get(f.service, "broker") as Broker;
  const base = registrationSchema.parse((await broker.kv.get(id))!.json());
  const ids: string[] = [];
  for (let n = 0; n < 60; n++) {
    const session = "other-session-" + n;
    const key = listenerId({ ...base, session_id: session }); ids.push(key);
    await broker.kv.create(key, encode({ ...base, listener_id: key, session_id: session, owner_token: randomUUID() }));
  }
  const result = await call(f.service, "cito_find", { limit: 50 });
  expect(result.listeners.map((entry: { listener_id: string }) => entry.listener_id)).toEqual(ids.reverse().slice(0, 50));
});

test("a second MCP process can revoke the active owner without losing unlisten suppression reason", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "remote revoke" }, f.pi.meta);
  const sent = await call(f.service, "cito_send", { to: id, message: "pending" });
  const other = await CitoService.create(f.config, f.serviceOptions); cleanups.push(() => other.close());
  expect(await call(other, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened" });
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "unlistened_before_dispatch" });
  expect(f.pi.messages).toEqual([]);
});

test("reconnect creates a fresh ephemeral consumer from persisted cursor without double charging", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "reconnect" }, f.pi.meta);
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
  const { listener_id: id, binding_generation: generation } = await call(f.service, "cito_listen", { about: "ordered admission" }, f.pi.meta);
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
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "disconnected", scope: "workspace" }, f.pi.meta);
  await f.service.receivers.get(id)!.stop();
  expect(await call(f.service, "cito_send", { to: id, message: "buffered" })).toMatchObject({ status: "queued_for_reconnect", effective_wake: true, held_reason: null });
  expect(await call(f.service, "cito_send", { to: id, message: "buffered passive", wake: false })).toMatchObject({ status: "stored_passive", effective_wake: false, held_reason: "passive_requested" });
  expect(f.service.state.listener(id)!.persisted_seq).toBe(0);
}, 10_000);
