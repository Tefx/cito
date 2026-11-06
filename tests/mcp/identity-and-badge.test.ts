import { afterEach, expect, test } from "vitest";
import { fixture, call, payload, waitFor, codexDesktop } from "./support.js";
import { CitoService } from "../../src/mcp/service.js";
import { LocalState, registrationSchema } from "../../src/mcp/state.js";
import { encode, type Broker } from "../../src/mcp/receiver.js";
import { listenerId, listenerSlug, normalizeSlug, publicListenerId, toCanonicalListenerId,
  listenerIdSchema, listenInputSchema } from "../../src/shared/index.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
async function setup() { const f = await fixture(); cleanups.push(() => f.close()); return f; }

test("slug normalization, derivation priority and canonical/public grammar", () => {
  expect(normalizeSlug("  Service __ Teams!!! ")).toBe("service-teams");
  expect(normalizeSlug("abcdefghijklmnopqrs--more")).toBe("abcdefghijklmnopqrs");
  expect(normalizeSlug("中文 🔔")).toBe("");
  const metadata = { tag: "Custom Tag", repo: "acme/Service", cwd: "/tmp/agent", title: "Conversation", harness: "pi" as const };
  expect(listenerSlug(metadata)).toBe("custom-tag");
  expect(listenerSlug({ ...metadata, tag: null })).toBe("service");
  expect(listenerSlug({ ...metadata, tag: null, repo: null })).toBe("agent");
  expect(listenerSlug({ ...metadata, tag: null, repo: null, cwd: "/中文" })).toBe("conversation");
  expect(listenerSlug({ ...metadata, tag: null, repo: null, cwd: "/中文", title: "中文" })).toBe("pi");
  const canonical = "c_pbtjxcxkgw3y", display = publicListenerId("service-teams", canonical);
  expect(display).toBe("service-teams-pbtjxcxkgw3y");
  for (const id of [canonical, display, "new-name-pbtjxcxkgw3y"]) {
    expect(toCanonicalListenerId(id)).toBe(canonical);
    expect(listenerIdSchema.safeParse(id).success).toBe(true);
  }
  for (const id of ["Service-pbtjxcxkgw3y", "-pbtjxcxkgw3y", "a--b-pbtjxcxkgw3y", "a-pbtjxcxkgw31", "c_short"]) {
    expect(() => toCanonicalListenerId(id)).toThrow();
    expect(listenerIdSchema.safeParse(id).success).toBe(false);
  }
  expect(listenInputSchema.safeParse({ about: "x", tag: "x".repeat(65) }).success).toBe(false);
});

test("tag renaming preserves canonical KV, subjects, SQLite keys and sender idempotency", async () => {
  const f = await setup(), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const sender = await call(f.service, "cito_listen", { about: "sender" }, desktop.meta);
  expect(sender.listener_id).toMatch(/^repo-[a-z2-7]{12}$/);
  const first = await call(f.service, "cito_listen", { about: "receiver", tag: "  Service Teams! " }, f.pi.meta);
  const id = toCanonicalListenerId(first.listener_id);
  expect(first.listener_id).toBe(publicListenerId("service-teams", id));
  const sent = await call(f.service, "cito_send", { to: first.listener_id, message: "one", idempotency_key: "same" }, desktop.meta);
  expect(await call(f.service, "cito_send", { to: id, message: "one", idempotency_key: "same" }, desktop.meta)).toEqual(sent);
  const preserved = await call(f.service, "cito_listen", { about: "updated" }, f.pi.meta);
  expect(preserved.listener_id).toBe(first.listener_id);
  const renamed = await call(f.service, "cito_listen", { about: "renamed", tag: "New Name" }, f.pi.meta);
  expect(toCanonicalListenerId(renamed.listener_id)).toBe(id);
  expect(f.service.state.listener(id)).toMatchObject({ listener_id: id, tag: "new-name", public_id: renamed.listener_id });
  const replay = await call(f.service, "cito_send", { to: renamed.listener_id, message: "one", idempotency_key: "same" }, desktop.meta);
  expect(replay).toMatchObject({ signal_id: sent.signal_id, seq: sent.seq, to: renamed.listener_id });
  expect(payload(await f.service.call("cito_send", { to: first.listener_id, message: "self" }, f.pi.meta))).toMatchObject({ error: { code: "SELF_SEND_FORBIDDEN" } });
  const found = await call(f.service, "cito_find", {}, f.pi.meta);
  expect(found.caller.listener_id).toBe(renamed.listener_id);
  expect(found.listeners.map((row: { listener_id: string }) => row.listener_id)).toContain(renamed.listener_id);
  expect(await call(f.service, "cito_inbox", { ack_all: false }, f.pi.meta)).toMatchObject({ listener_id: renamed.listener_id,
    signals: [{ from: { listener_id: sender.listener_id } }] });
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  expect(registrationSchema.parse((await broker.kv.get(id))!.json())).toMatchObject({ listener_id: id, public_id: renamed.listener_id, tag: "new-name" });
  expect(await broker.kv.get(first.listener_id)).toBeNull();
  expect((await broker.jsm.streams.info("CITO_SIGNALS")).state.messages).toBe(1);
  expect((await broker.jsm.streams.getMessage("CITO_SIGNALS", { seq: sent.seq })).subject).toBe("cito.signal." + id);
  for (const [table, column] of [["local_signals", "listener_id"], ["local_sent_idempotency", "to_listener_id"]]) {
    expect(f.service.state.db.prepare(`SELECT ${column} AS id FROM ${table}`).all()).toEqual([{ id }]);
  }
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ listener_id: renamed.listener_id });
});

test("invalid explicit tags leave the existing receiver and persisted registration untouched", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "receiver", tag: "stable" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id), receiver = f.service.receivers.get(`default:${id}`)!;
  const before = f.service.state.listener(id);
  for (const tag of ["", "  --- ", "中文 🔔", "a".repeat(65)]) {
    expect(payload(await f.service.call("cito_listen", { about: "invalid", tag }, f.pi.meta))).toMatchObject({ error: { code: "INVALID_INPUT" } });
  }
  expect(f.service.state.listener(id)).toEqual(before);
  expect(receiver.abort.signal.aborted).toBe(false);
});

test.each(["principal_id", "host_id", "harness", "session_id"])("collision guard checks %s before stopping an owner or writing SQLite", async field => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "untouched", tag: "stable" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id), receiver = f.service.receivers.get(`default:${id}`)!;
  const before = f.service.state.listener(id), broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  const entry = (await broker.kv.get(id))!, value = registrationSchema.parse(entry.json());
  const collided = { ...value, [field]: field === "harness" ? "codex" : "another-identity" };
  const revision = await broker.kv.update(id, encode(collided), entry.revision);
  expect(payload(await f.service.call("cito_listen", { about: "must not replace" }, f.pi.meta)))
    .toMatchObject({ error: { code: "STALE_GENERATION", message: "Listener ID collision" } });
  expect(f.service.state.listener(id)).toEqual(before);
  expect(receiver.abort.signal.aborted).toBe(false);
  expect((await broker.kv.get(id))!.revision).toBe(revision);
});

test("V4 state persists public IDs and ownership on reopening", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "persisted", tag: "retained" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id), before = f.service.state.listener(id);
  await f.service.close();
  const reopened = await LocalState.open(f.stateDir);
  try { expect(reopened.listener(id)).toEqual(before); } finally { reopened.close(); }
});

test("badge refresh covers waking/passive admission, gaps, acknowledgements and restore", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "badge", tag: "abcdefghijkl-more" }, f.pi.meta);
  const id = toCanonicalListenerId(enrolled.listener_id);
  expect(f.pi.badges.at(-1)).toBe("🔔 abcdefghijkl");
  f.service.state.run("UPDATE local_listeners SET replay_gap_detected = 1, replay_gap_range = 'offline_exceeded_30m' WHERE listener_id = ?", id);
  expect(await call(f.service, "cito_inbox", { ack_all: false }, f.pi.meta)).toMatchObject({ replay_gap_detected: true, replay_gap_range: "offline_exceeded_30m" });
  expect(f.pi.badges.at(-1)).toBe("🔔 abcdefghijkl");
  const waking = await call(f.service, "cito_send", { to: enrolled.listener_id, message: "awake" });
  await waitFor(() => f.pi.badges.at(-1) === "🔔 abcdefghijkl (1)");
  expect(f.pi.messages).toEqual([]); // Admission updates before Stage-2 debounce.
  expect(await call(f.service, "cito_inbox", { ack_ids: [waking.signal_id] }, f.pi.meta)).toMatchObject({ replay_gap_detected: true });
  expect(f.pi.badges.at(-1)).toBe("🔔 abcdefghijkl");
  expect(await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta)).toMatchObject({ replay_gap_detected: true });
  expect(await call(f.service, "cito_inbox", { ack_all: false }, f.pi.meta)).toMatchObject({ replay_gap_detected: false, replay_gap_range: null });
  expect(f.pi.badges.at(-1)).toBe("🔔 abcdefghijkl");
  await call(f.service, "cito_send", { to: id, message: "passive" });
  await waitFor(() => f.pi.badges.at(-1) === "🔔 abcdefghijkl (1)");
  await f.service.close();
  expect(f.pi.badges.at(-1)).toBeUndefined();
  const restarted = await CitoService.create(f.config, f.serviceOptions); cleanups.push(() => restarted.close());
  await call(restarted, "cito_find");
  expect(f.pi.badges.at(-1)).toBe("🔔 abcdefghijkl (1)");
  await restarted.receivers.get(`default:${id}`)!.revoke("session_closed");
  expect(f.pi.badges.at(-1)).toBeUndefined();
});

test("persisted ACTIVE rows with no live receiver never contribute a badge", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "offline", tag: "offline" }, f.pi.meta);
  await call(f.service, "cito_send", { to: enrolled.listener_id, message: "unread" });
  await waitFor(() => f.pi.badges.at(-1) === "🔔 offline (1)");
  await f.service.receivers.get(`default:${toCanonicalListenerId(enrolled.listener_id)}`)!.stop();
  await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta);
  expect(f.pi.badges.at(-1)).toBeUndefined();
  expect(f.service.state.listener(toCanonicalListenerId(enrolled.listener_id))!.status).toBe("ACTIVE");
});

test("a fenced old receiver cannot mutate a live replacement's persisted owner", async () => {
  const f = await setup();
  const first = await call(f.service, "cito_listen", { about: "old", tag: "old" }, f.pi.meta);
  const id = toCanonicalListenerId(first.listener_id), old = f.service.receivers.get(`default:${id}`)!;
  await call(f.service, "cito_listen", { about: "winner", tag: "winner" }, f.pi.meta);
  old.fence(); await old.revoke("session_closed");
  await call(f.service, "cito_inbox", {}, f.pi.meta);
  expect(f.pi.badges.at(-1)).toBe("🔔 winner");
  expect(f.service.receivers.get(`default:${id}`)!.abort.signal.aborted).toBe(false);
});

test("receiver admits a public wire recipient only on its canonical subject", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "wire", tag: "wire" }, f.pi.meta);
  const id = listenerId({ principal_id: "none#alice", host_id: f.service.hostId, harness: "pi", session_id: "pi-session-1" });
  const broker = Reflect.get(f.service.slots.get("default")!, "broker") as Broker;
  await broker.js.publish("cito.signal." + id, encode({ signal_id: "public-wire", to: enrolled.listener_id,
    target_generation: enrolled.binding_generation, sender_endpoint_key: "sender", from: {
      principal_id: "none#alice", user: "alice", host_id: "other", host_name: "other", harness: null },
    message: "public wire", in_reply_to: null }));
  await waitFor(() => f.service.state.listener(id)!.persisted_seq === 1);
  expect((await call(f.service, "cito_inbox", { ack_all: false }, f.pi.meta)).signals).toMatchObject([{ signal_id: "public-wire" }]);
});
