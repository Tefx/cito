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

test("restart restores persisted ACTIVE ownership and drains frozen then queued batches without re-enrolling", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "restart" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id);
  const sent = await call(f.service, "cito_send", { to: id, message: "frozen" });
  const owner = f.service.state.listener(id)!.owner_token;
  f.service.state.run("UPDATE local_signals SET state = 'DISPATCHING', batch_id = 'frozen-before-restart', batch_expires_at_ms = expires_at_ms, batch_card = 'Original frozen card' WHERE signal_id = ?", sent.signal_id);
  f.service.state.run("UPDATE local_listeners SET dispatcher_token = 'crashed-dispatcher', lease_until_ms = ? WHERE listener_id = ?", Date.now() + 60_000, id);
  await f.service.close();
  const nc = await connectNats({ servers: f.broker.url }); cleanups.push(() => nc.close());
  const { js } = await provisionNats(nc);
  await js.publish("cito.signal." + id, encode({ signal_id: "queued-during-restart", to: id, target_generation: 1,
    sender_endpoint_key: "none#alice|other|unbound|default", from: { principal_id: "none#alice", user: "alice", host_id: "other", host_name: "other", harness: null },
    message: "queued offline", in_reply_to: null }));
  const restarted = await CitoService.create(f.config, f.serviceOptions); cleanups.push(() => restarted.close());
  expect((await call(restarted, "cito_find")).listeners).toMatchObject([{ listener_id: enrolled.listener_id }]);
  await waitFor(() => restarted.state.listener(id)!.persisted_seq === 2 && restarted.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 2);
  expect(restarted.state.listener(id)).toMatchObject({ status: "ACTIVE", owner_token: owner, binding_generation: 1 });
  expect(f.pi.messages.map(message => message.content)).toEqual(["Original frozen card", "🔔 Cito · alice@other\n\nqueued offline"]);
  const delivered = restarted.state.signals("SELECT * FROM local_signals ORDER BY seq");
  expect(delivered[0]!.batch_id).toBe("frozen-before-restart");
  expect(delivered[1]!.batch_id).not.toBe(delivered[0]!.batch_id);
  expect(delivered.every(row => row.acknowledged_at_ms !== null)).toBe(true);
  expect((await call(restarted, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
}, 10_000); // Includes first-use Swift compilation and native startup under parallel test load.

test("unlisten cleanup cannot delete the concurrently installed replacement receiver", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "old" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const old = f.service.receivers.get(`default:${id}`)!, stop = old.stop.bind(old);
  let entered!: () => void, release!: () => void;
  const stopping = new Promise<void>(resolve => { entered = resolve; });
  const gate = new Promise<void>(resolve => { release = resolve; });
  vi.spyOn(old, "stop").mockImplementationOnce(async () => { await stop(); entered(); await gate; });
  const revocation = call(f.service, "cito_unlisten", {}, f.pi.meta);
  try {
    await stopping;
    await call(f.service, "cito_listen", { about: "replacement" }, f.pi.meta);
    const replacement = f.service.receivers.get(`default:${id}`)!;
    expect(replacement).not.toBe(old);
    release(); await revocation;
    expect(f.service.receivers.get(`default:${id}`)).toBe(replacement);
    expect(replacement.abort.signal.aborted).toBe(false);
    expect(await call(f.service, "cito_send", { to: id, message: "still receiving" })).toMatchObject({ status: "wake_accepted" });
  } finally { release(); await revocation; }
});

test("remote revocation failure preserves local unlisten and suppresses pending wakes", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "revoke offline" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId), receiver = f.service.receivers.get(`default:${id}`)!;
  await call(f.service, "cito_send", { to: id, message: "must stay passive" });
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  vi.spyOn(broker.kv, "update").mockRejectedValueOnce(new Error("temporary broker failure"));
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened" });
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect(registrationSchema.parse((await broker.kv.get(id))!.json()).state).toBe("ACTIVE");
  expect(receiver.abort.signal.aborted).toBe(true);
  expect(f.service.receivers.has(`default:${id}`)).toBe(false);
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "unlistened_before_dispatch" });
  expect(f.pi.messages).toEqual([]);
});

test("Pi A to B to A retains metadata without wake quotas", async () => {
  const f = await setup();
  const first = await call(f.service, "cito_listen", { about: "A" }, f.pi.meta);
  f.pi.switchSession("B");
  const second = await call(f.service, "cito_listen", { about: "B" }, f.pi.meta);
  f.pi.switchSession("pi-session-1");
  const resumed = await call(f.service, "cito_listen", { about: "A resumed" }, f.pi.meta);
  expect(resumed.listener_id).toBe(first.listener_id);
  expect(resumed.binding_generation).toBe(2);
  expect(second.listener_id).not.toBe(first.listener_id);
  for (let n = 0; n < 5; n++) expect(await call(f.service, "cito_send", { to: first.listener_id, message: "unlimited " + n })).toMatchObject({ status: "wake_accepted" });
});

test.each(["flush", "ack"])("shutdown interrupts a stalled send %s boundary without waiting for its deadline", async boundary => {
  const target = await setup();
  const { listener_id: publicId } = await call(target.service, "cito_listen", { about: "offline recipient" }, target.pi.meta);
  const id = toCanonicalListenerId(publicId);
  await target.service.receivers.get(`default:${id}`)!.stop();
  const sender = await fixture({ broker: target.broker, pi: false }); cleanups.push(() => sender.close());
  await call(sender.service, "cito_find");
  const broker = Reflect.get(sender.service.slots.get("default")!, "broker") as Broker;
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
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  vi.spyOn(broker.nc, "flush").mockImplementation(() => new Promise(() => {}));
  expect(payload(await f.service.call("cito_send", { to: id, message: "never flushed" }))).toMatchObject({ error: { code: "NATS_UNAVAILABLE", retryable: true } });
  expect((await broker.jsm.streams.info("CITO_SIGNALS")).state.messages).toBe(0);
  expect(f.service.state.db.prepare("SELECT status FROM local_sent_idempotency").get()?.status).toBe("PENDING_PUBLISH");
}, 13_000);

test("20-second scheduled sweep renews the live registration", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "scheduled sweep" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id);
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  const revision = (await broker.kv.get(id))!.revision;
  await waitFor(async () => (await broker.kv.get(id))!.revision > revision, 23_000);
  expect(f.service.state.listener(id)).toMatchObject({ consecutive_probe_failures: 0, status: "ACTIVE" });
}, 26_000);

test("transient Pi probes revoke on third failure, whereas healthy probe resets failure streak", async () => {
  const f = await setup(false);
  let healthy = true;
  const server = await unixServer(join(f.stateDir, `sockets/pi-pid-${process.pid}.sock`), socket => {
    socket.once("data", bytes => {
      const request = JSON.parse(bytes.toString());
      socket.end(JSON.stringify(request.op !== "identify" ? { ok: true } : healthy ? {
        ok: true, pi_pid: process.pid, session_id: "transient-pi", session_name: "probe", cwd: f.root,
        } : { ok: false, error: "TEMPORARY" }) + "\n");
    });
  }); cleanups.push(() => server.stop());
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "three strikes" }, { pi_pid: process.pid });
  const id = toCanonicalListenerId(publicId);
  const receiver = f.service.receivers.get(`default:${id}`)!;
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
  await call(f.service, "cito_send", { to: id, message: "retained" });
  desktop.setLive(false); await f.service.receivers.get(`default:${id}`)!.sweep();
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect((await call(f.service, "cito_inbox", {}, desktop.meta)).signals).toMatchObject([{ message: "retained" }]);
});

test("CAS retry exhaustion from real competing KV writes fences failed contender without advancing local generation", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "CAS" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
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
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
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
  await call(f.service, "cito_send", { to: id, message: "first" });
  const original = f.service.receivers.get(`default:${id}`)!;
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  await broker.nc.reconnect();
  await waitFor(() => f.service.receivers.get(`default:${id}`) !== original && f.service.receivers.get(`default:${id}`)?.abort.signal.aborted === false);
  const next = await call(f.service, "cito_send", { to: id, message: "second" });
  expect(next).toMatchObject({ status: "wake_accepted", seq: 2 });
  expect(f.service.state.listener(id)).toMatchObject({ persisted_seq: 2 });
  expect(f.service.state.signals("SELECT * FROM local_signals ORDER BY seq").map(row => row.message)).toEqual(["first", "second"]);
}, 10_000);

test("admission failure never advances restart cursor past the failed earlier message", async () => {
  const f = await setup();
  const { listener_id: publicId, binding_generation: generation } = await call(f.service, "cito_listen", { about: "ordered admission" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  const original = f.service.state.admit.bind(f.service.state), order: string[] = [];
  let attempts = 0;
  // Inject a transaction failure, not a successful fake admission. All commits remain real SQLite.
  vi.spyOn(f.service.state, "admit").mockImplementation((...args) => {
    if (args[2].signal_id === "first" && attempts++ === 0) throw new Error("interrupted admission");
    const result = original(...args); order.push(args[2].signal_id); return result;
  });
  const wire = { to: id, target_generation: generation, sender_endpoint_key: "none#alice|sender|unbound|default",
    from: { principal_id: "none#alice", user: "alice", host_id: "sender", host_name: "sender", harness: null },
    message: "ordered", in_reply_to: null };
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
  await f.service.receivers.get(`default:${id}`)!.stop();
  const sender = await fixture({ broker: f.broker, pi: false }); cleanups.push(() => sender.close());
  expect(await call(sender.service, "cito_send", { to: id, message: "buffered" })).toMatchObject({ status: "queued_for_reconnect", held_reason: null });
  expect(await call(sender.service, "cito_send", { to: id, message: "buffered passive" })).toMatchObject({ status: "queued_for_reconnect", held_reason: null });
  expect(f.service.state.listener(id)!.persisted_seq).toBe(0);
}, 10_000);

test("Stage 3 symmetric Node: @self and @username resolution, multi-channel attachment without FENCED_INACTIVE, signal_claim_dispatch, signal_ack sync, and plugins/telegram pairing & replies", async () => {
  const macA = await setup();
  const macB = await fixture({ broker: macA.broker, user: "alice" });
  const bobMac = await fixture({ broker: macA.broker, user: "bob" });
  cleanups.push(() => macB.close(), () => bobMac.close());

  // 1. Multi-channel attachment on the same Human Node (@alice) across Mac A and Mac B without FENCED_INACTIVE
  const enrollA = await call(
    macA.service,
    "cito_listen",
    { about: "Alice Mac A", scope: "workspace", tag: "alice" },
    { expected_harness: "human", expected_session_id: "default", title: "@alice (Me)" },
  );
  const enrollB = await call(
    macB.service,
    "cito_listen",
    { about: "Alice Mac B", scope: "workspace", tag: "alice" },
    { expected_harness: "human", expected_session_id: "default", title: "@alice (Me)" },
  );
  expect(enrollA.listener_id).toBe(enrollB.listener_id);
  const aliceHumanId = toCanonicalListenerId(enrollA.listener_id);
  expect(macA.service.state.listener(aliceHumanId)?.status).toBe("ACTIVE");
  expect(macB.service.state.listener(aliceHumanId)?.status).toBe("ACTIVE");

  // 2. Agent session on Mac A sends to @self (resolves O(1) to Alice's Human Node without SELF_SEND_FORBIDDEN)
  const piListen = await call(macA.service, "cito_listen", { about: "Pi worker", scope: "workspace" }, macA.pi.meta);
  const sentToSelf = await call(macA.service, "cito_send", { to: "@self", message: "Build finished on Pi" }, macA.pi.meta);
  expect(sentToSelf.to).toBe(enrollA.listener_id);

  await waitFor(() =>
    macA.service.state.signals("SELECT * FROM local_signals WHERE listener_id = ?", aliceHumanId).length === 1 &&
    macB.service.state.signals("SELECT * FROM local_signals WHERE listener_id = ?", aliceHumanId).length === 1,
  );

  // 3. Cross-channel business signal_ack: acknowledging on Mac A marks the signal read on Mac B as well
  await macA.service.ackSignalsFromUi({ signal_ids: [sentToSelf.signal_id] });
  await waitFor(() =>
    macB.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sentToSelf.signal_id)[0]?.acknowledged_at_ms !== null,
  );

  // 4. Bob enrolls his Pi session and sends to @alice by username shorthand; unlistening Mac B leaves Mac A ACTIVE
  await call(bobMac.service, "cito_listen", { about: "Bob Pi session", scope: "workspace" }, bobMac.pi.meta);
  const sentFromBob = await call(bobMac.service, "cito_send", { to: "@alice", message: "Hey Alice from Bob" }, bobMac.pi.meta);
  expect(sentFromBob.to).toBe(enrollA.listener_id);

  await macB.service.startUi();
  const macBStates = vi.spyOn(macB.service.ui!, "state");
  await macB.service.unlistenById("default", aliceHumanId);
  expect(macB.service.state.listener(aliceHumanId)?.status).toBe("UNLISTENED");
  expect(macA.service.state.listener(aliceHumanId)?.status).toBe("ACTIVE");
  expect(macBStates.mock.lastCall?.[2]).toEqual(
    expect.arrayContaining([expect.objectContaining({ harness: "human", session_id: "default", title: "@alice (Me)" })]),
  );
  await macB.service.handleUiAction({
    op: "listen_session",
    harness: "human",
    session_id: "default",
    title: "@alice (Me)",
  });
  expect(macB.service.state.listener(aliceHumanId)?.status).toBe("ACTIVE");
  await macB.service.unlistenById("default", aliceHumanId);

  // Unknown @username fails with LISTENER_NOT_FOUND; ambiguous @username across distinct principals fails with INVALID_INPUT
  expect(payload(await bobMac.service.call("cito_send", { to: "@nonexistent_user", message: "hi" }))).toMatchObject({
    error: { code: "LISTENER_NOT_FOUND" },
  });
  const broker = Reflect.get(macA.service.slots.get("default")!, "broker") as Broker;
  const dupId = listenerId({ principal_id: "oidc#alice-dup", harness: "human", session_id: "default" });
  await broker.kv.create(
    dupId,
    encode({
      listener_id: dupId,
      public_id: publicListenerId("alice", dupId),
      tag: "alice",
      principal_id: "oidc#alice-dup",
      owner_user: "alice",
      host_id: "host-dup",
      host_name: "dup-mac",
      harness: "human",
      session_id: "default",
      cwd: "~",
      repo: null,
      branch: null,
      title: "@alice (Dup)",
      about: "dup",
      scope: "workspace",
      state: "ACTIVE",
      binding_generation: 1,
      owner_token: randomUUID(),
    }),
  );
  bobMac.service.slots.get("default")!.directoryIndex.clear();
  expect(payload(await bobMac.service.call("cito_send", { to: "@alice", message: "ambiguous" }))).toMatchObject({
    error: { code: "INVALID_INPUT", message: expect.stringContaining("Ambiguous @username") },
  });
  await broker.kv.delete(dupId);
  bobMac.service.slots.get("default")!.directoryIndex.clear();

  // 5. Atomic signal_claim_dispatch guarantees at most 1 wake_exclusive channel claims a signal batch
  const claim1 = await broker.nc.rpc<{ claimed: boolean }>("signal_claim_dispatch", {
    node_id: aliceHumanId,
    signal_ids: [sentFromBob.signal_id],
    channel_id: "host-a:pi",
    lease_ms: 30_000,
  });
  const claim2 = await broker.nc.rpc<{ claimed: boolean }>("signal_claim_dispatch", {
    node_id: aliceHumanId,
    signal_ids: [sentFromBob.signal_id],
    channel_id: "host-b:pi",
    lease_ms: 30_000,
  });
  expect(claim1.claimed).toBe(true);
  expect(claim2.claimed).toBe(false);

  // 6. Built-in plugins/telegram: pairing (/start <code>), outbound sendMessage, restart idempotency, swipe-to-reply, and implicit recent-sender reply
  const tgCalls: { method: string; body: Record<string, unknown> }[] = [];
  let nextMsgId = 500;
  let pendingUpdates: unknown[] = [];
  const hubEngine = macA.broker.engine;
  (hubEngine as { fetchFn?: typeof fetch }).fetchFn = (async (input: string | URL | Request, init?: RequestInit) => {
    const urlStr = String(input);
    const method = urlStr.split("/").at(-1) ?? "";
    const body = init?.body ? (JSON.parse(String(init.body)) as Record<string, unknown>) : {};
    tgCalls.push({ method, body });
    if (method === "getMe") {
      return new Response(JSON.stringify({ ok: true, result: { id: 1, is_bot: true, username: "cito_test_bot" } }), { status: 200 });
    }
    if (method === "sendMessage") {
      nextMsgId += 1;
      return new Response(JSON.stringify({ ok: true, result: { message_id: nextMsgId } }), { status: 200 });
    }
    if (method === "getUpdates") {
      const batch = pendingUpdates.splice(0);
      return new Response(JSON.stringify({ ok: true, result: batch }), { status: 200 });
    }
    return new Response(JSON.stringify({ ok: true, result: true }), { status: 200 });
  }) as typeof fetch;

  const pairStart = await broker.nc.rpc<{ code: string; instruction: string }>("human_channel_pair_start", {
    plugin_id: "telegram",
    hub_config: { botToken: "123456:TEST_TOKEN" },
  });
  expect(pairStart.code).toMatch(/^cito-[a-z2-7]{6}$/);
  expect(pairStart.instruction).toBe(`Send "/start ${pairStart.code}" to @cito_test_bot`);

  // User sends /start <code> in Telegram
  pendingUpdates.push({
    update_id: 10,
    message: {
      message_id: 1,
      from: { id: 42, username: "alice_tg" },
      chat: { id: 987654, type: "private" },
      text: `/start ${pairStart.code}`,
    },
  });
  const poll1 = await broker.nc.rpc<{ processed: number }>("human_channel_poll", { plugin_id: "telegram" });
  expect(poll1.processed).toBe(1);
  expect(tgCalls.some(c => c.method === "sendMessage" && String(c.body.text).includes("Paired with Cito as @alice"))).toBe(true);

  // Outbound notification to @alice delivers to Telegram and records external_msg_id in hub_channel_deliveries
  const tgSendBefore = tgCalls.filter(c => c.method === "sendMessage").length;
  const bobToAlice2 = await call(bobMac.service, "cito_send", { to: "@alice", message: "Please review PR #42" }, bobMac.pi.meta);
  await waitFor(() => tgCalls.filter(c => c.method === "sendMessage").length > tgSendBefore);
  const deliveredMsgCall = tgCalls.filter(c => c.method === "sendMessage").at(-1)!;
  expect(deliveredMsgCall.body).toMatchObject({
    chat_id: "987654",
    text: expect.stringContaining("Please review PR #42"),
  });
  const deliveredExternalMsgId = nextMsgId;

  // Replay / restart idempotency: replaying pending deliveries never re-sends an already-delivered signal
  const countAfterFirstDeliver = tgCalls.filter(c => c.method === "sendMessage").length;
  await hubEngine.replayPendingHumanDeliveries();
  expect(tgCalls.filter(c => c.method === "sendMessage").length).toBe(countAfterFirstDeliver);

  // Swipe-to-reply in Telegram routes back to Bob's Pi session and acknowledges bobToAlice2
  pendingUpdates.push({
    update_id: 11,
    message: {
      message_id: 2,
      from: { id: 42, username: "alice_tg" },
      chat: { id: 987654, type: "private" },
      text: "LGTM via swipe reply!",
      reply_to_message: { message_id: deliveredExternalMsgId },
    },
  });
  await broker.nc.rpc("human_channel_poll", { plugin_id: "telegram" });
  await waitFor(() =>
    bobMac.service.state.signals("SELECT * FROM local_signals").some(s => s.message === "LGTM via swipe reply!"),
  );

  // Implicit recent-sender reply within 10 minutes (without reply_to_message) routes to the latest sender and echoes confirmation
  await call(macA.service, "cito_send", { to: "@self", message: "Pi question for Alice" }, macA.pi.meta);
  await waitFor(() => tgCalls.some(c => c.method === "sendMessage" && String(c.body.text).includes("Pi question for Alice")));
  pendingUpdates.push({
    update_id: 12,
    message: {
      message_id: 3,
      from: { id: 42, username: "alice_tg" },
      chat: { id: 987654, type: "private" },
      text: "Implicit reply to Pi!",
    },
  });
  await broker.nc.rpc("human_channel_poll", { plugin_id: "telegram" });
  await waitFor(() =>
    macA.service.state.signals("SELECT * FROM local_signals").some(s => s.message === "Implicit reply to Pi!"),
  );
  expect(tgCalls.some(c => c.method === "sendMessage" && String(c.body.text).includes("↩ Replied to"))).toBe(true);
}, 20_000);
