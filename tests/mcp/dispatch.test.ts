import { afterEach, expect, test } from "vitest";
import { readFile, writeFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { fixture, call, codexDesktop, waitFor } from "./support.js";
import { unixServer } from "../adapters/support.js";
import { CitoService } from "../../src/mcp/service.js";
import { connectNats, provisionNats, toCanonicalListenerId } from "../../src/shared/index.js";
import { encode } from "../../src/mcp/receiver.js";
import { registrationSchema } from "../../src/mcp/state.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
async function setup(pi = true) { const f = await fixture({ pi }); cleanups.push(() => f.close()); return f; }
async function queueScript(path: string, code: string) { await writeFile(path, "#!/usr/bin/env node\n" + code, { mode: 0o700 }); }
async function fileExists(path: string) { try { await stat(path); return true; } catch { return false; } }

test("whole frozen batch expires at earliest member; no partial wake escapes", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "expire" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "early" });
  await call(f.service, "cito_send", { to: id, message: "later" });
  f.service.state.run(`UPDATE local_signals SET state = 'DISPATCHING', batch_id = 'frozen', batch_expires_at_ms = ?, dispatcher_token = 'crashed'`, Date.now() - 1);
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect(f.service.state.signals("SELECT * FROM local_signals").map(row => row.state)).toEqual(["EXPIRED_UNSENT", "EXPIRED_UNSENT"]);
  expect(f.pi.messages).toEqual([]);
});

test("recovery retries immutable existing batch before new received rows", async () => {
  const f = await setup();
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "recovery" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId);
  const first = await call(f.service, "cito_send", { to: id, message: "original frozen" });
  const frozenCard = '🔔 Cito · Original title\n\noriginal frozen';
  f.service.state.run("UPDATE local_signals SET state = 'DISPATCHING', batch_id = 'immutable-batch', batch_expires_at_ms = expires_at_ms, batch_card = ? WHERE signal_id = ?", frozenCard, first.signal_id);
  f.service.state.run("UPDATE local_listeners SET title = 'Changed title' WHERE listener_id = ?", id);
  await call(f.service, "cito_send", { to: id, message: "new arrival" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect(f.pi.messages).toHaveLength(1);
  expect(f.pi.messages[0]!.content).toBe(frozenCard);
  expect(f.pi.messages[0]!.content).not.toContain("new arrival");
  expect(f.service.state.signals("SELECT * FROM local_signals ORDER BY seq").map(row => [row.state, row.batch_id])).toEqual([["DISPATCHED", "immutable-batch"], ["RECEIVED", null]]);
});

test("acknowledgement during queue submission survives completion and suppresses the separate open effect", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const marker = join(f.root, "queue-entered"), release = join(f.root, "release");
  await queueScript(f.config.codexBin, `import { writeFileSync, existsSync } from 'node:fs';
writeFileSync(${JSON.stringify(marker)}, 'queued');
const timer = setInterval(() => { if (existsSync(${JSON.stringify(release)})) { clearInterval(timer); } }, 10);
`);
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "ack-inflight" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  const sent = await call(f.service, "cito_send", { to: id, message: "read while queued" });
  const dispatch = f.service.receivers.get(`default:${id}`)!.dispatch();
  await waitFor(() => fileExists(marker));
  await call(f.service, "cito_inbox", { ack_all: true }, desktop.meta);
  await writeFile(release, "release"); await dispatch;
  const stored = f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]!;
  expect(stored.acknowledged_at_ms).toBeGreaterThan(0);
  expect(stored.queue_written).toBe(1);
  expect(stored.state).toBe("STORED_PASSIVE");
  expect(await fileExists(desktop.log)).toBe(false);
  expect((await call(f.service, "cito_inbox", {}, desktop.meta)).signals).toEqual([]);
});

test.each(["unlisten", "fence"])("%s aborts an in-flight Codex queue and prevents open -g", async action => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const marker = join(f.root, "queue-entered");
  await queueScript(f.config.codexBin, `import { writeFileSync } from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'entered'); setInterval(() => {}, 1000);`);
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: action }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  const sent = await call(f.service, "cito_send", { to: id, message: "abort" });
  const receiver = f.service.receivers.get(`default:${id}`)!;
  const dispatch = receiver.dispatch();
  await waitFor(() => fileExists(marker));
  if (action === "unlisten") await call(f.service, "cito_unlisten", {}, desktop.meta);
  else {
    const nc = await connectNats({ servers: f.broker.url });
    try {
      const { kv } = await provisionNats(nc), entry = (await kv.get(id))!, value = registrationSchema.parse(entry.json());
      await kv.update(id, encode({ ...value, binding_generation: value.binding_generation + 1, owner_token: "12345678-1234-4234-8234-123456789012" }), entry.revision);
      await waitFor(() => receiver.abort.signal.aborted);
    } finally { await nc.close(); }
  }
  await dispatch;
  expect(await fileExists(desktop.log)).toBe(false);
  expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: action === "unlisten" ? "unlistened_before_dispatch" : "fenced_before_dispatch" });
});

test("three definite Codex pre-insertion failures store passive and never invoke open", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const attempts = join(f.root, "attempts");
  await queueScript(f.config.codexBin, `import { appendFileSync } from 'node:fs'; appendFileSync(${JSON.stringify(attempts)}, 'attempt\\n'); process.exit(2);`);
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "failure" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "driver fails" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect((await readFile(attempts, "utf8")).trim().split("\n")).toHaveLength(3);
  expect(await fileExists(desktop.log)).toBe(false);
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "driver_unavailable", queue_written: 0 });
}, 10_000);

test("Pi socket failures retry the exact batch three times; session mismatch revokes immediately", async () => {
  const f = await setup(false);
  let mode: "error" | "mismatch" = "error";
  const batches: string[] = [];
  const socket = await unixServer(join(f.stateDir, `sockets/pi-pid-${process.pid}.sock`), connection => {
    connection.once("data", bytes => {
      const request = JSON.parse(bytes.toString());
      if (request.op === "identify") connection.end(JSON.stringify({ ok: true, pi_pid: process.pid, session_id: "pi-retry", session_name: "retry", cwd: f.root,
        }) + "\n");
      else if (request.op === "status") connection.end('{"ok":true}\n');
      else { batches.push(request.batch_id); connection.end(mode === "mismatch" ? '{"ok":false,"error":"SESSION_MISMATCH"}\n' : '{"ok":false,"error":"TEMPORARY_ERROR"}\n'); }
    });
  }); cleanups.push(() => socket.stop());
  const meta = { pi_pid: process.pid };
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "Pi retry" }, meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "retry" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect(batches).toHaveLength(3); expect(new Set(batches).size).toBe(1);
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "driver_unavailable" });
  mode = "mismatch";
  await call(f.service, "cito_send", { to: id, message: "switch" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect(f.service.state.signals("SELECT * FROM local_signals ORDER BY seq")[1]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "session_switched" });
}, 10_000);

test("Codex 10-second queue timeout records unknown insertion and still submits exactly one open", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  await queueScript(f.config.codexBin, "setInterval(() => {}, 1000);");
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "timeout" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "unknown insertion" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ queue_written: 1, state: "DISPATCHED", acknowledged_at_ms: null });
  expect((await readFile(desktop.log, "utf8")).trim().split("\n").map(line => JSON.parse(line))).toEqual([["open", "-g", "codex://threads/wake/" + desktop.thread]]);
  expect(await call(f.service, "cito_inbox", {}, desktop.meta)).toMatchObject({ acknowledged_count: 1, signals: [{ message: "unknown insertion" }] });
}, 15_000);

test("Desktop open retry does not repeat queue insertion", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const attempts = join(f.root, "open-attempts");
  await queueScript(f.config.openBin, `import { appendFileSync, readFileSync } from 'node:fs';
appendFileSync(${JSON.stringify(attempts)}, 'open\\n'); if (readFileSync(${JSON.stringify(attempts)}, 'utf8').trim().split('\\n').length === 1) process.exit(1);`);
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "open retry" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "queue once" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect((await readFile(attempts, "utf8")).trim().split("\n")).toHaveLength(2);
  expect((await readFile(desktop.log, "utf8")).trim().split("\n").map(line => JSON.parse(line)[0])).toEqual(["queue"]);
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "DISPATCHED", queue_written: 1 });
});

test("a clean queue with failed Desktop wake remains unread and can drain through inbox", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  await queueScript(f.config.openBin, "process.exit(1);");
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "unconfirmed" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  const sent = await call(f.service, "cito_send", { to: id, message: "wake failed" });
  await f.service.receivers.get(`default:${id}`)!.dispatch();
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "DISPATCHED", queue_written: 1, acknowledged_at_ms: null });
  expect(await call(f.service, "cito_inbox", {}, desktop.meta)).toMatchObject({ acknowledged_count: 1, signals: [{ signal_id: sent.signal_id, message: "wake failed" }] });
  expect((await call(f.service, "cito_inbox", {}, desktop.meta)).signals).toEqual([]);
});

test("Pi completion sets DISPATCHED without clearing a concurrent inbox acknowledgement", async () => {
  const f = await setup(false);
  let entered!: () => void, complete!: () => void;
  const deliveryEntered = new Promise<void>(resolve => { entered = resolve; });
  const server = await unixServer(join(f.stateDir, `sockets/pi-pid-${process.pid}.sock`), socket => {
    socket.once("data", bytes => {
      const request = JSON.parse(bytes.toString());
      if (request.op === "identify") socket.end(JSON.stringify({ ok: true, pi_pid: process.pid, session_id: "ack-inflight",
        session_name: "Pi acknowledgement", cwd: f.root, }) + "\n");
      else if (request.op === "status") socket.end('{"ok":true}\n');
      else { complete = () => socket.end('{"ok":true,"delivered":true,"deduplicated":false}\n'); entered(); }
    });
  }); cleanups.push(() => server.stop());
  const meta = { pi_pid: process.pid };
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "ack during Pi effect" }, meta);
  const id = toCanonicalListenerId(publicId);
  const sent = await call(f.service, "cito_send", { to: id, message: "ack while delivering" });
  const dispatch = f.service.receivers.get(`default:${id}`)!.dispatch();
  await deliveryEntered;
  expect(await call(f.service, "cito_inbox", { ack_all: true }, meta)).toMatchObject({ acknowledged_count: 1 });
  complete(); await dispatch;
  const row = f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]!;
  expect(row.state).toBe("DISPATCHED"); expect(row.acknowledged_at_ms).toBeGreaterThan(0);
  expect((await call(f.service, "cito_inbox", {}, meta)).signals).toEqual([]);
});

test("frozen DISPATCHING recovery after restart preserves batch identity and queue_written", async () => {
  const f = await setup(false), desktop = await codexDesktop(f); cleanups.push(() => desktop.close());
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "restart" }, desktop.meta);
  const id = toCanonicalListenerId(publicId);
  await call(f.service, "cito_send", { to: id, message: "already queued" });
  f.service.state.run("UPDATE local_signals SET state = 'DISPATCHING', batch_id = 'crashed-batch', batch_expires_at_ms = expires_at_ms, queue_written = 1");
  await f.service.close();
  const restarted = await CitoService.create(f.config, f.serviceOptions); cleanups.push(() => restarted.close());
  await call(restarted, "cito_listen", { about: "resumed" }, desktop.meta);
  await restarted.receivers.get(`default:${id}`)!.dispatch();
  expect((await readFile(desktop.log, "utf8")).trim().split("\n").map(line => JSON.parse(line))).toEqual([["open", "-g", "codex://threads/wake/" + desktop.thread]]);
  expect(restarted.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "DISPATCHED", batch_id: "crashed-batch", queue_written: 1 });
});

test("600-message burst paginates into 12 batches of 50, splits 256 KiB byte-ceiling batches into [4, 4, 4], and holds 70 KB oversized sender title as STORED_PASSIVE driver_unavailable without blocking subsequent messages", async () => {
  const f = await setup();
  const listener = await call(f.service, "cito_listen", { about: "burst receiver" }, f.pi.meta);
  const id = toCanonicalListenerId(listener.listener_id as string);
  const receiver = f.service.receivers.get(`default:${id}`)!;
  const slot = f.service.slots.get("default")!;
  const broker = await slot.network();
  const publishBurst = async (count: number, title: string, prefix: string) => {
    const before = f.service.state.signals("SELECT * FROM local_signals").length;
    f.service.state.run("UPDATE local_listeners SET dispatcher_token = 'burst', lease_until_ms = ? WHERE listener_id = ?", Date.now() + 30000, id);
    const messages = Array.from({ length: count }, (_, i) => prefix + String(i).padStart(4, "0") + "🙂".repeat(500 - prefix.length - 4));
    for (const [i, message] of messages.entries()) broker.nc.publish("cito.signal." + id, encode({
      signal_id: prefix + i, to: id, target_generation: listener.binding_generation, sender_endpoint_key: "burst-sender",
      from: { principal_id: slot.principal!.principal_id, user: "alice", host_id: "source-host", host_name: "source",
        harness: null, listener_id: null, title }, message, in_reply_to: null,
    }));
    await broker.nc.flush();
    await waitFor(() => f.service.state.signals("SELECT * FROM local_signals").length === before + count, 10000);
    return messages;
  };
  const burstMessages = await publishBurst(600, "", "burst");
  f.service.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE listener_id = ?", id);
  await receiver.dispatch();
  await waitFor(() => f.service.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 600, 5000);
  await waitFor(() => f.pi.messages.length === 12);
  const burstCards = f.pi.messages.map(m => m.content);
  expect(burstCards.every(card => card.startsWith("🔔 Cito · 50 notifications"))).toBe(true);
  expect(burstCards.every(card => Buffer.byteLength(JSON.stringify(card)) <= 256 * 1024)).toBe(true);
  expect(burstCards.reduce((n, card) => n + Buffer.byteLength(card), 0)).toBeGreaterThan(1_048_576);
  // Reset acknowledgement to exercise 12-page cito_inbox pagination
  f.service.state.run("UPDATE local_signals SET acknowledged_at_ms = NULL WHERE signal_id LIKE 'burst%'");
  const drained: string[] = [];
  for (let i = 0; i < 12; i++) {
    const page = await call(f.service, "cito_inbox", {}, f.pi.meta);
    expect(page.acknowledged_count).toBe(50);
    drained.push(...(page.signals as { message: string }[]).map(signal => signal.message));
  }
  expect(drained).toEqual(burstMessages);

  // Long metadata exercises the serialized-byte ceiling before the 50-row ceiling.
  await publishBurst(12, "<".repeat(15000), "large");
  const firstLargeIdx = f.pi.messages.length;
  f.service.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE listener_id = ?", id);
  await receiver.dispatch();
  await waitFor(() => f.service.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 612, 5000);
  await waitFor(() => f.pi.messages.length >= firstLargeIdx + 3);
  const largeCards = f.pi.messages.slice(firstLargeIdx).map(m => m.content);
  expect(largeCards).toHaveLength(3);
  expect(largeCards.every(card => Buffer.byteLength(JSON.stringify(card)) <= 256 * 1024)).toBe(true);
  const batchSizes = f.service.state.db.prepare("SELECT COUNT(*) AS n FROM local_signals WHERE signal_id LIKE 'large%' GROUP BY batch_id").all();
  expect(batchSizes.map(row => row.n)).toEqual([4, 4, 4]);

  // A single oversized sender title stays in the inbox instead of wedging subsequent batches.
  await publishBurst(1, "<".repeat(70000), "oversized");
  await publishBurst(1, "", "after");
  f.service.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE listener_id = ?", id);
  await receiver.dispatch();
  await waitFor(() => f.pi.messages.length === firstLargeIdx + 4);
  expect(f.pi.messages.at(-1)!.content).toContain("after0000");
  expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = 'oversized0'")[0])
    .toMatchObject({ state: "STORED_PASSIVE", held_reason: "driver_unavailable", acknowledged_at_ms: null });
  const finalInbox = await call(f.service, "cito_inbox", {}, f.pi.meta);
  expect(finalInbox).toMatchObject({ acknowledged_count: 1, signals: [{ signal_id: "oversized0" }] });
}, 30_000);
