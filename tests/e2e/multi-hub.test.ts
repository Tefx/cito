import { afterEach, expect, test, vi } from "vitest";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture, call, payload, waitFor } from "../mcp/support.js";
import { startNats } from "../shared/nats-server.js";
import { unixServer } from "../adapters/support.js";
import { CitoService } from "../../src/mcp/service.js";
import { SocketBridgeAdapter, registerAdapter, ADAPTER_REGISTRY } from "../../src/adapters/index.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import { encode, refreshSessionBadge } from "../../src/mcp/receiver.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); ADAPTER_REGISTRY.delete("cursor"); });
async function multi(pi = true) {
  const a = await startNats(), b = await startNats(); cleanups.push(() => a.stop(), () => b.stop());
  const dir = await mkdtemp("/tmp/cito-multi-"); cleanups.push(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, "config.json");
  await writeFile(path, JSON.stringify({ default_hub: "personal", hubs: {
    personal: { natsUrl: a.url }, rda: { natsUrl: b.url },
  } }));
  const f = await fixture({ pi, broker: a, env: { CITO_CONFIG_PATH: path } }); cleanups.push(() => f.close());
  return f;
}
const canonical = (raw: string) => toCanonicalListenerId(raw.split(":").at(-1)!);

test("two real NATS hubs isolate wire IDs, batches and auto-acks while sharing one session badge", async () => {
  const f = await multi();
  const p = await call(f.service, "cito_listen", { about: "personal", tag: "agent" }, f.pi.meta);
  const r = await call(f.service, "cito_listen", { about: "corporate", hub: "rda", tag: "agent" }, f.pi.meta);
  expect(p.listener_id).toBe("personal:" + r.listener_id.split(":")[1]);
  const one = await call(f.service, "cito_send", { to: p.listener_id, message: "one", idempotency_key: "same" });
  const two = await call(f.service, "cito_send", { to: r.listener_id, message: "two", idempotency_key: "same" });
  expect(one.signal_id).toMatch(/^personal:sig_/); expect(two.signal_id).toMatch(/^rda:sig_/);
  expect(one.seq).toBe(1); expect(two.seq).toBe(1);
  await waitFor(() => f.pi.badges.at(-1) === "🔔 agent (2)");
  for (const [hub, sent] of [["personal", one], ["rda", two]] as const) {
    const broker = await f.service.slots.get(hub)!.network();
    const wire = (await broker.jsm.streams.getMessage("CITO_SIGNALS", { seq: 1 })).json<{ to: string; signal_id: string }>();
    expect(wire.to).toBe(canonical(p.listener_id)); expect(wire.signal_id).toBe(sent.signal_id.split(":")[1]);
    expect(wire).not.toHaveProperty("requested_wake");
  }
  await Promise.all([...f.service.receivers.values()].map(receiver => receiver.dispatch()));
  expect(f.pi.messages).toHaveLength(2);
  const rows = f.service.state.signals("SELECT * FROM local_signals ORDER BY hub");
  expect(rows.map(row => row.batch_id)).toEqual([`batch_personal_${canonical(p.listener_id)}_1_1_1`, `batch_rda_${canonical(p.listener_id)}_1_1_1`]);
  expect(rows.every(row => row.acknowledged_at_ms !== null && row.state === "DISPATCHED")).toBe(true);
  expect(f.pi.badges.at(-1)).toBe("🔔 agent");
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
  expect(await call(f.service, "cito_unlisten", { hub: "personal" }, f.pi.meta)).toMatchObject({ listener_id: p.listener_id });
  expect(f.pi.badges.at(-1)).toBe("🔔 agent");
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ listener_id: r.listener_id });
  expect(f.pi.badges.at(-1)).toBeUndefined();
}, 15_000);

test("find queries hubs concurrently, deterministically merges revisions and skips malformed entries", async () => {
  const f = await multi();
  const entries = [];
  for (const hub of ["rda", "personal"]) entries.push(await call(f.service, "cito_listen", { about: "search", hub, tag: "agent" }, f.pi.meta));
  const brokers = await Promise.all([...f.service.slots.values()].map(slot => slot.network()));
  for (const broker of brokers) await broker.kv.create("malformed", encode({ broken: true }));
  let entered = 0, release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const spies = brokers.map(broker => {
    const keys = broker.kv.keys.bind(broker.kv);
    return vi.spyOn(broker.kv, "keys").mockImplementation(async (...args) => { entered++; await gate; return keys(...args); });
  });
  const pending = call(f.service, "cito_find", { query: "search" });
  try { await waitFor(() => entered === 2); } finally { release(); }
  const found = await pending; spies.forEach(spy => spy.mockRestore());
  expect(found.listeners.map((row: { hub: string }) => row.hub)).toEqual(["personal", "rda"]);
  const bad = vi.spyOn(f.service.slots.get("rda")!, "network").mockRejectedValue(new Error("hub offline"));
  const partial = await call(f.service, "cito_find");
  expect(partial.listeners).toHaveLength(1); expect(partial.listeners[0].hub).toBe("personal");
  expect(partial.warnings).toMatchObject([{ hub: "rda", code: "NATS_UNAVAILABLE" }]);
  vi.spyOn(f.service.slots.get("personal")!, "network").mockRejectedValue(new Error("both offline"));
  expect(payload(await f.service.call("cito_find"))).toMatchObject({ error: { code: "NATS_UNAVAILABLE" } });
  bad.mockRestore();
}, 15_000);

test("parallel unlisten revokes every local hub despite a failed remote update", async () => {
  const f = await multi();
  const ids = new Map<string, string>();
  for (const hub of ["personal", "rda"]) ids.set(hub, canonical((await call(f.service, "cito_listen", { about: "revoke", hub }, f.pi.meta)).listener_id));
  const broker = await f.service.slots.get("rda")!.network();
  const update = vi.spyOn(broker.kv, "update").mockRejectedValueOnce(new Error("remote unavailable"));
  const result = await call(f.service, "cito_unlisten", {}, f.pi.meta);
  update.mockRestore();
  expect(result).toMatchObject({ status: "unlistened", warnings: [{ hub: "rda", code: "NATS_UNAVAILABLE", message: "remote unavailable" }] });
  expect(f.service.state.listener(ids.get("personal")!, "personal")!.status).toBe("UNLISTENED");
  expect(f.service.state.listener(ids.get("rda")!, "rda")!.status).toBe("UNLISTENED");
  expect(f.service.receivers.has(`rda:${ids.get("rda")}`)).toBe(false);
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "not_listening" });
  expect(f.pi.badges.at(-1)).toBeUndefined();
}, 15_000);

test("send and inbox round-trip non-null reply IDs with named hub qualification", async () => {
  const f = await multi();
  for (const hub of ["personal", "rda"]) {
    const enrolled = await call(f.service, "cito_listen", { about: "reply routing", hub }, f.pi.meta);
    const sent = await call(f.service, "cito_send", { to: enrolled.listener_id, message: "reply", in_reply_to: hub + ":sig_original" });
    const inbox = await call(f.service, "cito_inbox", { hub, ack_all: false }, f.pi.meta);
    expect(inbox.signals).toMatchObject([{ signal_id: sent.signal_id, in_reply_to: hub + ":sig_original" }]);
    expect(f.service.state.signals("SELECT * FROM local_signals WHERE hub = ?", hub)[0]!.in_reply_to).toBe("sig_original");
  }
});

test("inbox orders and drains across hubs, enforces scoped ack scope and rejects bare-ID ambiguity atomically", async () => {
  const f = await multi();
  for (const hub of ["personal", "rda"]) await call(f.service, "cito_listen", { about: "inbox", hub }, f.pi.meta);
  const p = [...f.service.receivers.values()].find(row => row.hub === "personal")!.id;
  const r = [...f.service.receivers.values()].find(row => row.hub === "rda")!.id;
  for (const [hub, id] of [["personal", p], ["rda", r]]) {
    const row = f.service.state.listener(id!, hub)!;
    f.service.state.admit(id!, row.owner_token, { signal_id: "sig_same", to: id!, target_generation: 1, sender_endpoint_key: "sender",
      from: { principal_id: row.principal_id, user: "alice", host_id: "other", host_name: "other", harness: "cursor", listener_id: "agent-abcdefgh2345", title: "agent", about: null },
      message: hub!, in_reply_to: null }, 1, "2026-01-01T00:00:00Z", Date.now() + 60_000, hub!);
  }
  const ambiguous = await f.service.call("cito_inbox", { ack_ids: ["sig_same"] }, f.pi.meta);
  expect(payload(ambiguous)).toMatchObject({ error: { code: "INVALID_INPUT" } });
  expect(f.service.state.signals("SELECT * FROM local_signals").every(row => row.acknowledged_at_ms === null)).toBe(true);
  expect(payload(await f.service.call("cito_inbox", { hub: "personal", ack_ids: ["rda:sig_same"] }, f.pi.meta))).toMatchObject({ error: { code: "INVALID_INPUT" } });
  const peek = await call(f.service, "cito_inbox", { ack_all: false }, f.pi.meta);
  expect(peek.signals.map((row: { signal_id: string }) => row.signal_id)).toEqual(["personal:sig_same", "rda:sig_same"]);
  expect(peek.signals[0].from.listener_id).toBe("personal:agent-abcdefgh2345");
  const ack = await call(f.service, "cito_inbox", { ack_ids: ["personal:sig_same"] }, f.pi.meta);
  expect(ack.acknowledged_count).toBe(1); expect(ack.signals).toHaveLength(2);
  const drained = await call(f.service, "cito_inbox", {}, f.pi.meta);
  expect(drained.acknowledged_count).toBe(1); expect(drained.signals.map((row: { signal_id: string }) => row.signal_id)).toEqual(["rda:sig_same"]);
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
});

test("unknown/conflicting addressing and conflicting caller claims fail before network or persistent effects", async () => {
  const f = await multi();
  const spies = [...f.service.slots.values()].map(slot => vi.spyOn(slot, "network"));
  for (const args of [{ to: "other:c_abcdefgh2345" }, { to: "rda:c_abcdefgh2345", hub: "personal" },
    { to: "personal:c_abcdefgh2345", in_reply_to: "rda:sig_one" }]) {
    expect(payload(await f.service.call("cito_send", { ...args, message: "invalid" }))).toMatchObject({ error: { code: "INVALID_INPUT" } });
  }
  for (const name of ["cito_listen", "cito_find", "cito_send", "cito_inbox", "cito_unlisten"]) {
    const args = name === "cito_send" ? { to: "c_abcdefgh2345", message: "invalid" } : name === "cito_listen" ? { about: "invalid" } : {};
    expect(payload(await f.service.call(name, args, { codexThreadId: "explicit", endpoint_pid: process.pid }))).toMatchObject({ error: { code: "INVALID_INPUT" } });
  }
  expect(spies.every(spy => spy.mock.calls.length === 0)).toBe(true);
  expect(f.service.state.db.prepare("SELECT COUNT(*) AS n FROM local_sent_idempotency").get()!.n).toBe(0);
});

test("find and send reject invalid, unreachable, and ambiguous explicit PID claims before connecting", async () => {
  const f = await multi();
  const target = await call(f.service, "cito_listen", { about: "first" }, f.pi.meta);
  f.pi.switchSession("second-session");
  await call(f.service, "cito_listen", { about: "second" }, f.pi.meta);
  f.service.state.run("UPDATE local_listeners SET endpoint_pid = 2147483647");
  const spies = [...f.service.slots.values()].map(slot => vi.spyOn(slot, "network"));
  for (const meta of [{ endpoint_pid: 0 }, { endpoint_pid: 2147483647 }, { endpoint_pid: 2147483646 }]) {
    for (const name of ["cito_find", "cito_send"]) {
      const args = name === "cito_send" ? { to: target.listener_id, message: "invalid claim" } : {};
      expect(payload(await f.service.call(name, args, meta))).toMatchObject({ error: { code: "PI_BRIDGE_UNAVAILABLE" } });
    }
  }
  expect(spies.every(spy => spy.mock.calls.length === 0)).toBe(true);
});

test("unopen implicit ancestor sockets leave outbound callers unbound", async () => {
  const f = await multi();
  const enrolled = await call(f.service, "cito_listen", { about: "implicit" }, f.pi.meta);
  const server = await unixServer(join(f.stateDir, `sockets/aider-pid-${process.ppid}.sock`), socket => socket.once("data", () => socket.end('{"ok":false,"error":"SESSION_NOT_OPEN"}\n')));
  cleanups.push(() => server.stop());
  expect((await call(f.service, "cito_find")).caller).toMatchObject({ harness: null, session_id: null });
  expect(await call(f.service, "cito_send", { to: enrolled.listener_id, message: "unbound allowed" })).toMatchObject({ status: "wake_accepted" });
});

test("a dynamically discovered cursor bridge can deliver unconfirmed, drain offline, and revoke all hubs on a session switch", async () => {
  const f = await multi(false), requests: Record<string, unknown>[] = [];
  let switched = false;
  const server = await unixServer(join(f.stateDir, `sockets/cursor-pid-${process.pid}.sock`), socket => socket.once("data", bytes => {
    const req = JSON.parse(bytes.toString()); requests.push(req);
    socket.end(JSON.stringify(req.op === "identify" ? { ok: true, endpoint_pid: process.pid, session_id: switched ? "other" : "cursor-session", session_name: "Cursor", cwd: f.root }
      : req.op === "deliver" ? { ok: true, delivered: true, deduplicated: false } : { ok: true }) + "\n");
  })); cleanups.push(() => server.stop());
  const meta = { endpoint_pid: process.pid };
  const personal = await call(f.service, "cito_listen", { about: "custom", tag: "cursor" }, meta);
  expect(personal.harness).toBe("cursor"); expect(ADAPTER_REGISTRY.get("cursor")).toBeInstanceOf(SocketBridgeAdapter);
  await call(f.service, "cito_listen", { about: "custom", hub: "rda", tag: "cursor" }, meta);
  registerAdapter(new SocketBridgeAdapter("cursor", { confirmDelivery: false }));
  requests.length = 0;
  expect((await call(f.service, "cito_find", {}, meta)).caller.harness).toBe("cursor");
  expect(requests.filter(req => req.op === "identify")).toHaveLength(1); // Discovery runs once, independent of hub count.
  const senderPid = 2147483645;
  const senderSocket = await unixServer(join(f.stateDir, `sockets/aider-pid-${senderPid}.sock`), socket => socket.once("data", bytes => {
    requests.push(JSON.parse(bytes.toString()));
    socket.end(JSON.stringify({ ok: true, endpoint_pid: senderPid, session_id: "sender-session", session_name: "Sender", cwd: f.root }) + "\n");
  })); cleanups.push(() => senderSocket.stop());
  requests.length = 0;
  const sent = await call(f.service, "cito_send", { to: personal.listener_id, message: "unconfirmed" }, { endpoint_pid: senderPid });
  expect(requests.filter(req => req.op === "identify")).toHaveLength(1);
  const receiver = f.service.receivers.get(`personal:${canonical(personal.listener_id)}`)!;
  await receiver.dispatch();
  expect(requests.find(req => req.op === "deliver")).toMatchObject({ wake: true, session_id: "cursor-session", batch_id: expect.stringMatching(/^batch_personal_/) });
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "DISPATCHED", acknowledged_at_ms: null });
  const inbox = await call(f.service, "cito_inbox", {}, meta);
  expect(inbox).toMatchObject({ acknowledged_count: 1, signals: [{ signal_id: sent.signal_id, message: "unconfirmed" }] });
  switched = true; await receiver.sweep();
  expect(f.service.state.db.prepare("SELECT status FROM local_listeners").all().map(row => row.status)).toEqual(["UNLISTENED", "UNLISTENED"]);
  for (const slot of f.service.slots.values()) expect((await call(f.service, "cito_find", { hub: slot.name })).listeners).toEqual([]);
}, 15_000);

test("offline multi-hub inbox skips an unauthenticated secondary slot, but explicit or all-unauthenticated requests fail", async () => {
  const f = await multi();
  await call(f.service, "cito_listen", { about: "offline" }, f.pi.meta);
  const config = { ...f.config, hubs: { ...f.config.hubs, rda: { ...f.config.hubs.rda!, authMode: "oidc" as const,
    oidcIssuer: "https://not-contacted.example", oidcClientId: "cito" } } };
  const offline = await CitoService.create(config, f.serviceOptions); cleanups.push(() => offline.close());
  expect(await call(offline, "cito_inbox", {}, f.pi.meta)).toMatchObject({ signals: [] });
  expect(payload(await offline.call("cito_inbox", { hub: "rda" }, f.pi.meta))).toMatchObject({ error: { code: "UNAUTHORIZED" } });
  const noAuth = await CitoService.create({ ...config, hubs: { rda: config.hubs.rda }, defaultHub: "rda" }, f.serviceOptions);
  cleanups.push(() => noAuth.close());
  expect(payload(await noAuth.call("cito_inbox", {}, f.pi.meta))).toMatchObject({ error: { code: "UNAUTHORIZED" } });
});

test("newly enrolled endpoints replace every retained hub PID and active rows select the badge target", async () => {
  const f = await multi();
  let personal;
  for (let generation = 1; generation <= 5; generation++) personal = await call(f.service, "cito_listen", { about: "old host", tag: "old-host" }, f.pi.meta);
  const id = canonical(personal!.listener_id);
  await call(f.service, "cito_unlisten", { hub: "personal" }, f.pi.meta);
  f.service.state.run("UPDATE local_listeners SET endpoint_pid = 2147483647 WHERE hub = 'personal'");
  const rda = await call(f.service, "cito_listen", { hub: "rda", about: "new host", tag: "new-host" }, f.pi.meta);
  expect(rda.binding_generation).toBe(1);
  expect(f.service.state.listener(id, "personal")).toMatchObject({ status: "UNLISTENED", binding_generation: 5, endpoint_pid: process.pid });
  expect(f.service.state.listener(id, "rda")).toMatchObject({ status: "ACTIVE", binding_generation: 1, endpoint_pid: process.pid });
  expect(f.pi.badges.at(-1)).toBe("🔔 new-host");
  // A stale inactive row must not outrank the current active endpoint, independently of enrollment propagation.
  f.service.state.run("UPDATE local_listeners SET endpoint_pid = 2147483647 WHERE hub = 'personal'");
  await refreshSessionBadge(f.service.state, f.config, "pi", "pi-session-1");
  expect(f.pi.badges.at(-1)).toBe("🔔 new-host");
  f.service.state.run("UPDATE local_listeners SET endpoint_pid = ? WHERE hub = 'personal'", process.pid);
  await call(f.service, "cito_unlisten", { hub: "rda" }, f.pi.meta);
  expect(f.pi.badges.at(-1)).toBeUndefined();
});

test("shutdown excludes closing receiver keys from badge aggregation", async () => {
  const f = await multi();
  const enrolled = await call(f.service, "cito_listen", { about: "badge", tag: "shared" }, f.pi.meta), id = canonical(enrolled.listener_id);
  const key = `personal:${id}`;
  await refreshSessionBadge(f.service.state, f.config, "pi", "pi-session-1", { closingKeys: new Set([key]) });
  expect(f.pi.badges.at(-1)).toBeUndefined();
  await f.service.receivers.get(key)!.stop();
  await refreshSessionBadge(f.service.state, f.config, "pi", "pi-session-1", { closingKeys: new Set([key]) });
  expect(f.pi.badges.at(-1)).toBeUndefined();
});
