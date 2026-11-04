import { afterEach, expect, test } from "vitest";
import { readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fixture, call, payload, waitFor, codexDesktop } from "../mcp/support.js";
import { CitoService } from "../../src/mcp/service.js";

const fixtures: Awaited<ReturnType<typeof fixture>>[] = [];
afterEach(async () => { for (const f of fixtures.splice(0)) await f.close(); });
async function setup(pi = true) { const f = await fixture({ pi }); fixtures.push(f); return f; }

test("Pi lifecycle uses real broker, bridge socket, frozen wake card, inbox acknowledgement and native HUD", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "Pi notifications" }, f.pi.meta);
  const id = String(enrolled.listener_id);
  expect(id).toMatch(/^c_[a-z2-7]{12}$/);
  expect(enrolled).toMatchObject({ binding_generation: 1, scope: "self", allow_peer_wake: false, unattended_wakes_remaining: 3 });
  for (const [file, mode] of [[f.stateDir, 0o700], [join(f.stateDir, "host-id"), 0o600], [join(f.stateDir, "state.sqlite"), 0o600]] as const) expect((await stat(file)).mode & 0o777).toBe(mode);
  expect((await readFile(join(f.stateDir, "host-id"), "utf8")).trim()).toMatch(/^[a-f0-9-]{36}$/);
  expect(f.service.state.db.prepare("PRAGMA journal_mode").get()!.journal_mode).toBe("wal");
  expect(f.service.state.db.prepare("PRAGMA busy_timeout").get()!.timeout).toBe(5000);
  const found = await call(f.service, "cito_find", { query: "pi notifications", user: "ALICE", host: f.config.hostName, harness: "pi" });
  expect(found.caller).toMatchObject({ harness: null, session_id: null, listener_id: null });
  expect(found.listeners).toMatchObject([{ listener_id: id }]);
  const sent = await call(f.service, "cito_send", { to: id, message: " <ping>\u0000 ", idempotency_key: "one" });
  expect(sent).toMatchObject({ requested_wake: true, effective_wake: true, status: "wake_accepted", held_reason: null, seq: 1 });
  await waitFor(() => f.pi.messages.length === 1);
  expect(f.pi.messages[0]).toMatchObject({ options: { triggerTurn: true, deliverAs: "followUp" } });
  expect(f.pi.messages[0]!.content).toContain("&lt;ping&gt;");
  const inbox = await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta);
  expect(inbox).toMatchObject({ listener_id: id, acknowledged_count: 1, signals: [{ signal_id: sent.signal_id, message: "<ping>", effective_wake: true }] });
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
  expect(await call(f.service, "cito_send", { to: id, message: "<ping>", idempotency_key: "one" })).toEqual(sent);
  expect(payload(await f.service.call("cito_send", { to: id, message: "changed", idempotency_key: "one" }))).toMatchObject({ error: { code: "IDEMPOTENCY_KEY_CONFLICT" } });
  expect(payload(await f.service.call("cito_send", { to: id, message: "self" }, f.pi.meta))).toMatchObject({ error: { code: "SELF_SEND_FORBIDDEN" } });
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened", listener_id: id });
  expect(f.pi.badges.at(-1)).toBeUndefined();
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "not_listening", listener_id: null });
  expect((await call(f.service, "cito_find")).listeners).toEqual([]);
  expect(payload(await f.service.call("cito_send", { to: id, message: "closed" }))).toMatchObject({ error: { code: "LISTENER_NOT_FOUND" } });
  expect(f.diagnostics).toEqual([]);
}, 15_000);

test("passive Pi admission changes badge/inbox and never inserts a transcript card", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "passive" }, f.pi.meta);
  const sent = await call(f.service, "cito_send", { to: id, message: "FYI", wake: false });
  expect(sent).toMatchObject({ status: "stored_passive", effective_wake: false, held_reason: "passive_requested" });
  await waitFor(() => f.pi.badges.at(-1)?.includes("1 unread") === true);
  expect(f.pi.messages).toEqual([]);
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toMatchObject([{ message: "FYI", held_reason: "passive_requested" }]);
});

test("Codex lifecycle and offline post-close inbox/unlisten use explicit metadata and local registration", async () => {
  const f = await setup(false), desktop = await codexDesktop(f);
  try {
    const enrolled = await call(f.service, "cito_listen", { about: "Codex receiver" }, desktop.meta);
    const id = String(enrolled.listener_id);
    expect(enrolled).toMatchObject({ harness: "codex", session_id: desktop.thread, repo: "Owner/Repo", branch: "main" });
    const sent = await call(f.service, "cito_send", { to: id, message: "review available" });
    await waitFor(() => f.service.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 1);
    const commands = (await readFile(desktop.log, "utf8")).trim().split("\n").map(line => JSON.parse(line));
    expect(commands[0].slice(0, 4)).toEqual(["queue", "queue", "--thread", desktop.thread]);
    expect(commands[0][5]).toContain("review available");
    expect(commands[1]).toEqual(["open", "-g", "codex://threads/wake/" + desktop.thread]);
    desktop.setLive(false); desktop.db.prepare("UPDATE threads SET archived = 1").run();
    expect(payload(await f.service.call("cito_listen", { about: "closed" }, desktop.meta))).toMatchObject({ error: { code: "SESSION_NOT_OPEN" } });
    expect(await call(f.service, "cito_unlisten", {}, desktop.meta)).toMatchObject({ status: "unlistened" });
    await f.broker.stop();
    const offline = await call(f.service, "cito_inbox", { ack_ids: [sent.signal_id] }, desktop.meta);
    expect(offline).toMatchObject({ acknowledged_count: 1, signals: [{ message: "review available" }] });
  } finally { await desktop.close(); }
}, 15_000);

test("offline Pi inbox reads and acknowledges SQLite without attempting NATS", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "offline" }, f.pi.meta);
  await call(f.service, "cito_send", { to: id, message: "offline-readable", wake: false });
  await f.broker.stop();
  expect(await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta)).toMatchObject({ acknowledged_count: 1, signals: [{ message: "offline-readable" }] });
});

test("concurrent takeover fences old owner without changing winner or its new-generation signals", async () => {
  const f = await setup();
  const initial = await call(f.service, "cito_listen", { about: "initial", scope: "workspace", allow_peer_wake: true }, f.pi.meta);
  const id = String(initial.listener_id), old = f.service.receivers.get(id)!;
  const other = await CitoService.create(f.config, f.serviceOptions);
  try {
    const winner = await call(other, "cito_listen", { about: "winner" }, f.pi.meta);
    expect(winner).toMatchObject({ listener_id: id, binding_generation: 2, scope: "workspace", allow_peer_wake: true });
    await waitFor(() => old.abort.signal.aborted);
    const sent = await call(other, "cito_send", { to: id, message: "winner work" });
    old.fence();
    expect(f.service.state.listener(id)).toMatchObject({ status: "ACTIVE", binding_generation: 2 });
    expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]).toMatchObject({ state: "RECEIVED", binding_generation: 2 });
    expect(await call(other, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened" });
  } finally { await other.close(); }
});

test("receiver acknowledgement prevents a debounced wake; unlisten cancels admitted batches", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "cancel" }, f.pi.meta);
  await call(f.service, "cito_send", { to: id, message: "read before wake" });
  await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta);
  await f.service.receivers.get(id)!.dispatch();
  expect(f.pi.messages).toEqual([]);
  const next = await call(f.service, "cito_send", { to: id, message: "cancel me" });
  await call(f.service, "cito_unlisten", {}, f.pi.meta);
  expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", next.signal_id)[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "unlistened_before_dispatch" });
  expect(f.pi.messages).toEqual([]);
});

test("liveness sweep is single-flight, consumes interactive deltas and revokes a switched Pi session", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "live" }, f.pi.meta);
  const receiver = f.service.receivers.get(id)!;
  await call(f.service, "cito_send", { to: id, message: "charge" });
  f.pi.input();
  await Promise.all([receiver.sweep(), receiver.sweep(), receiver.sweep()]);
  expect(f.service.state.listener(id)).toMatchObject({ human_event_seq: 1, unattended_wake_count: 0, consecutive_probe_failures: 0 });
  f.pi.switchSession("pi-session-2"); await receiver.sweep();
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect((await call(f.service, "cito_find")).listeners).toEqual([]);
});

test("private targets are invisible and forbidden cross-user; workspace peer wakes default to passive", async () => {
  const f = await setup();
  const peer = await fixture({ broker: f.broker, user: "bob", pi: false }); fixtures.push(peer);
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "private" }, f.pi.meta);
  expect((await call(peer.service, "cito_find")).listeners).toEqual([]);
  expect(payload(await peer.service.call("cito_send", { to: id, message: "forbidden" }))).toMatchObject({ error: { code: "FORBIDDEN_TARGET_SCOPE" } });
  await call(f.service, "cito_listen", { about: "shared", scope: "workspace" }, f.pi.meta);
  expect((await call(peer.service, "cito_find")).listeners).toMatchObject([{ listener_id: id }]);
  expect(await call(peer.service, "cito_send", { to: id, message: "peer" })).toMatchObject({ effective_wake: false, held_reason: "peer_wake_disabled" });
  expect(payload(await peer.service.call("cito_find", { harness: "claude" }))).toMatchObject({ error: { code: "UNSUPPORTED_HARNESS" } });
});

test.skipIf(process.platform !== "darwin")("real Codex HUD click advances human sequence and resets persisted wake quota on sweep", async () => {
  const f = await setup(false), desktop = await codexDesktop(f);
  try {
    f.serviceOptions.banner!.env = { CITO_BANNER_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "5000", CITO_BANNER_URL_OPENER: f.serviceOptions.codex!.openBin };
    const { listener_id: id } = await call(f.service, "cito_listen", { about: "human click" }, desktop.meta);
    f.service.state.run("UPDATE local_listeners SET unattended_wake_count = 3 WHERE listener_id = ?", id);
    await call(f.service, "cito_send", { to: id, message: "click to read", wake: false });
    const binary = join(f.stateDir, "bin/cito-banner");
    let pid = 0;
    await waitFor(async () => {
      const { stdout } = await promisify(execFile)("/bin/ps", ["-axo", "pid=,command="]);
      const line = stdout.split("\n").find(line => line.trim().replace(/^\d+\s+/, "") === binary);
      if (line) pid = Number(line.trim().split(/\s+/)[0]);
      try { return pid > 0 && (await stat(join(f.stateDir, "sockets/banner.sock"))).isSocket(); } catch { return false; }
    });
    process.kill(pid, "SIGUSR1");
    await waitFor(() => f.service.state.listener(id)!.human_event_seq === 1);
    await f.service.receivers.get(id)!.sweep();
    expect(f.service.state.listener(id)).toMatchObject({ human_event_seq: 1, last_consumed_human_seq: 1, unattended_wake_count: 0 });
    await waitFor(async () => { try { return (await readFile(desktop.log, "utf8")).trim() === JSON.stringify(["open", "codex://threads/" + desktop.thread]); } catch { return false; } });
    expect((await readFile(desktop.log, "utf8")).trim()).toBe(JSON.stringify(["open", "codex://threads/" + desktop.thread]));
  } finally { await desktop.close(); }
});

test("find applies every filter, orders by revision, and inbox cannot acknowledge a different session", async () => {
  const f = await setup(), desktop = await codexDesktop(f);
  try {
    const pi = await call(f.service, "cito_listen", { about: "Build review" }, f.pi.meta);
    const codex = await call(f.service, "cito_listen", { about: "Build review" }, desktop.meta);
    expect((await call(f.service, "cito_find", { limit: 1 })).listeners.map((entry: { listener_id: string }) => entry.listener_id)).toEqual([codex.listener_id]);
    expect((await call(f.service, "cito_find", { query: "OWNER MAIN", repo: "owner/re", user: "ALICE", host: f.service.hostId })).listeners.map((entry: { listener_id: string }) => entry.listener_id)).toEqual([codex.listener_id]);
    expect((await call(f.service, "cito_find", { query: "Build missing" })).listeners).toEqual([]);
    expect((await call(f.service, "cito_find", { user: "bob" })).listeners).toEqual([]);
    expect((await call(f.service, "cito_find", { host: "no-such-host" })).listeners).toEqual([]);
    await call(f.service, "cito_listen", { about: "Build review updated" }, f.pi.meta);
    expect((await call(f.service, "cito_find", { limit: 1 })).listeners.map((entry: { listener_id: string }) => entry.listener_id)).toEqual([pi.listener_id]);
    const piSignal = await call(f.service, "cito_send", { to: pi.listener_id, message: "Pi only", wake: false });
    const codexSignal = await call(f.service, "cito_send", { to: codex.listener_id, message: "Codex only", wake: false });
    expect(await call(f.service, "cito_inbox", { ack_ids: [codexSignal.signal_id, "unknown"] }, f.pi.meta)).toMatchObject({ acknowledged_count: 0, signals: [{ signal_id: piSignal.signal_id }] });
    expect(await call(f.service, "cito_inbox", { ack_ids: [piSignal.signal_id] }, f.pi.meta)).toMatchObject({ acknowledged_count: 1 });
    expect((await call(f.service, "cito_inbox", {}, desktop.meta)).signals).toMatchObject([{ signal_id: codexSignal.signal_id }]);
  } finally { await desktop.close(); }
});

test("PENDING_PUBLISH recovery reuses exact signal ID and JetStream dedup sequence", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "idempotency" }, f.pi.meta);
  const original = await call(f.service, "cito_send", { to: id, message: "reserved", idempotency_key: "recover" });
  f.service.state.run("UPDATE local_sent_idempotency SET status = 'PENDING_PUBLISH'");
  const recovered = await call(f.service, "cito_send", { to: id, message: "reserved", idempotency_key: "recover" });
  expect(recovered).toMatchObject({ signal_id: original.signal_id, seq: original.seq });
  expect(f.service.state.listener(id)!.unattended_wake_count).toBe(1);
  expect(f.service.state.signals("SELECT * FROM local_signals")).toHaveLength(1);
}, 10_000);
