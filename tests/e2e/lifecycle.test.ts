import { afterEach, expect, test } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { spawn } from "node:child_process";
import { mkdir, readFile, stat, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fixture, call, payload, waitFor, codexDesktop } from "../mcp/support.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import { peekClaudeWakes } from "../../src/adapters/index.js";
import { CitoService } from "../../src/mcp/service.js";
import { startHttpServer } from "../../src/mcp/server.js";

const fixtures: Awaited<ReturnType<typeof fixture>>[] = [];
afterEach(async () => { for (const f of fixtures.splice(0)) await f.close(); });
async function setup(pi = true, observeBanner = false) { const f = await fixture({ pi, observeBanner }); fixtures.push(f); return f; }

test.skipIf(process.platform !== "darwin")("Pi lifecycle uses real broker, bridge socket, frozen wake card, inbox acknowledgement and native HUD", async () => {
  const f = await setup(true, true);
  const enrolled = await call(f.service, "cito_listen", { about: "Pi notifications" }, f.pi.meta);
  const id = String(enrolled.listener_id);
  expect(id).toMatch(/^[a-z0-9]+(?:-[a-z0-9]+)*-[a-z2-7]{12}$/);
  expect(enrolled).toMatchObject({ binding_generation: 1, scope: "self" });
  for (const [file, mode] of [[f.stateDir, 0o700], [join(f.stateDir, "host-id"), 0o600], [join(f.stateDir, "state.sqlite"), 0o600]] as const) expect((await stat(file)).mode & 0o777).toBe(mode);
  expect((await readFile(join(f.stateDir, "host-id"), "utf8")).trim()).toMatch(/^[a-f0-9-]{36}$/);
  expect(f.service.state.db.prepare("PRAGMA journal_mode").get()!.journal_mode).toBe("wal");
  expect(f.service.state.db.prepare("PRAGMA busy_timeout").get()!.timeout).toBe(5000);
  const found = await call(f.service, "cito_find", { query: "pi notifications", user: "ALICE", host: f.config.hostName, harness: "pi" });
  expect(found.caller).toMatchObject({ harness: null, session_id: null, listener_id: null });
  expect(found.listeners).toMatchObject([{ listener_id: id }]);
  const sent = await call(f.service, "cito_send", { to: id, message: " <ping>\u0000 ", idempotency_key: "one" });
  expect(sent).toMatchObject({ status: "wake_accepted", held_reason: null, seq: 1 });
  await waitFor(() => f.pi.messages.length === 1);
  expect(f.pi.messages[0]).toMatchObject({ options: { triggerTurn: true, deliverAs: "followUp" } });
  expect(f.pi.messages[0]!.content).toContain("&lt;ping&gt;");
  await waitFor(() => f.bannerDisplays.length === 1);
  expect(f.bannerDisplays[0]).toMatchObject({ title: "Cito · Pi receiver", subtitle: `alice@${f.config.hostName} · `, message: "<ping>" });
  const inbox = await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta);
  expect(inbox).toMatchObject({ listener_id: id, acknowledged_count: 0, signals: [] });
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
  expect(await call(f.service, "cito_send", { to: id, message: "<ping>", idempotency_key: "one" })).toEqual(sent);
  expect(payload(await f.service.call("cito_send", { to: id, message: "changed", idempotency_key: "one" }))).toMatchObject({ error: { code: "IDEMPOTENCY_KEY_CONFLICT" } });
  expect(payload(await f.service.call("cito_send", { to: id, message: "self" }, f.pi.meta))).toMatchObject({ error: { code: "SELF_SEND_FORBIDDEN" } });
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened", listener_id: id });
  expect(f.pi.badges.at(-1)).toBeUndefined();
  expect(await call(f.service, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "not_listening", listener_id: null });
  expect((await call(f.service, "cito_find")).listeners).toEqual([]);
  expect(payload(await f.service.call("cito_send", { to: id, message: "closed" }))).toMatchObject({ error: { code: "LISTENER_NOT_FOUND" } });
  expect(f.diagnostics.filter(line => !line.startsWith("cito-ui: state ") && line !== "cito-ui: banner hidden")).toEqual([]);
}, 15_000);

test("Pi confirmed delivery automatically drains its inbox and refreshes the badge", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "confirmed", tag: "confirmed" }, f.pi.meta);
  await call(f.service, "cito_send", { to: id, message: "confirmed delivery" });
  await waitFor(() => f.pi.messages.length === 1);
  await waitFor(() => f.service.state.signals("SELECT * FROM local_signals")[0]?.acknowledged_at_ms != null);
  expect((await call(f.service, "cito_inbox", {}, f.pi.meta)).signals).toEqual([]);
  expect(f.pi.badges.at(-1)).toBe("🔔 confirmed");
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
    expect(commands[0].slice(0, 5)).toEqual(["queue", "queue", "--resume", "--thread", desktop.thread]);
    expect(commands[0][6]).toContain("review available");
    expect(commands[1]).toEqual(["open", "-g", "codex://threads/wake/" + desktop.thread]);
    desktop.setLive(false); desktop.db.prepare("UPDATE threads SET archived = 1").run();
    expect(payload(await f.service.call("cito_listen", { about: "closed" }, desktop.meta))).toMatchObject({ error: { code: "SESSION_NOT_OPEN" } });
    expect(await call(f.service, "cito_unlisten", {}, desktop.meta)).toMatchObject({ status: "unlistened" });
    await f.broker.stop();
    await rm(join(f.codexHome, "state_5.sqlite"));
    const offline = await call(f.service, "cito_inbox", { ack_ids: [sent.signal_id] }, desktop.meta);
    expect(offline).toMatchObject({ acknowledged_count: 0, signals: [] });
  } finally { await desktop.close(); }
}, 15_000);

test("offline Pi inbox reads and acknowledges SQLite without attempting NATS", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "offline" }, f.pi.meta);
  await call(f.service, "cito_send", { to: id, message: "offline-readable" });
  await f.broker.stop();
  expect(await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta)).toMatchObject({ acknowledged_count: 1, signals: [{ message: "offline-readable" }] });
});

test("concurrent takeover fences old owner without changing winner or its new-generation signals", async () => {
  const f = await setup();
  const initial = await call(f.service, "cito_listen", { about: "initial", scope: "workspace" }, f.pi.meta);
  const id = String(initial.listener_id), old = f.service.receivers.get(`default:${toCanonicalListenerId(id)}`)!;
  const other = await CitoService.create(f.config, f.serviceOptions);
  try {
    const winner = await call(other, "cito_listen", { about: "winner" }, f.pi.meta);
    expect(winner).toMatchObject({ listener_id: id, binding_generation: 2, scope: "workspace" });
    await waitFor(() => old.abort.signal.aborted);
    const sent = await call(other, "cito_send", { to: id, message: "winner work" });
    old.fence();
    expect(f.service.state.listener(toCanonicalListenerId(id))).toMatchObject({ status: "ACTIVE", binding_generation: 2 });
    expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", sent.signal_id)[0]).toMatchObject({ state: "RECEIVED", binding_generation: 2 });
    expect(await call(other, "cito_unlisten", {}, f.pi.meta)).toMatchObject({ status: "unlistened" });
  } finally { await other.close(); }
});

test("receiver acknowledgement prevents a debounced wake; unlisten cancels admitted batches", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "cancel" }, f.pi.meta);
  await call(f.service, "cito_send", { to: id, message: "read before wake" });
  await call(f.service, "cito_inbox", { ack_all: true }, f.pi.meta);
  await f.service.receivers.get(`default:${toCanonicalListenerId(id)}`)!.dispatch();
  expect(f.pi.messages).toEqual([]);
  const next = await call(f.service, "cito_send", { to: id, message: "cancel me" });
  await call(f.service, "cito_unlisten", {}, f.pi.meta);
  expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = ?", next.signal_id)[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "unlistened_before_dispatch" });
  expect(f.pi.messages).toEqual([]);
});

test("liveness sweep is single-flight and revokes a switched Pi session", async () => {
  const f = await setup();
  const { listener_id: id } = await call(f.service, "cito_listen", { about: "live" }, f.pi.meta);
  const receiver = f.service.receivers.get(`default:${toCanonicalListenerId(id)}`)!;
  await call(f.service, "cito_send", { to: id, message: "charge" });
  await Promise.all([receiver.sweep(), receiver.sweep(), receiver.sweep()]);
  expect(f.service.state.listener(toCanonicalListenerId(id))).toMatchObject({ consecutive_probe_failures: 0 });
  f.pi.switchSession("pi-session-2"); await receiver.sweep();
  expect(f.service.state.listener(toCanonicalListenerId(id))!.status).toBe("UNLISTENED");
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
  expect(await call(peer.service, "cito_send", { to: id, message: "peer" })).toMatchObject({ status: "wake_accepted", held_reason: null });
  expect((await call(peer.service, "cito_find", { harness: "cursor" })).listeners).toEqual([]);
});

test.skipIf(process.platform !== "darwin")("real Codex HUD click opens its thread without accessing local state", async () => {
  const f = await setup(false), desktop = await codexDesktop(f);
  try {
    f.serviceOptions.ui!.env = { CITO_UI_HEADLESS: "1", CITO_BANNER_DISMISS_MS: "5000", CITO_BANNER_URL_OPENER: f.config.openBin };
    await f.service.startUi();
    const { listener_id: id } = await call(f.service, "cito_listen", { about: "click" }, desktop.meta);
    await call(f.service, "cito_send", { to: id, message: "click to read" });
    await waitFor(() => f.bannerDisplays.length > 0);
    process.kill(f.bannerDisplays[0]!.pid, "SIGUSR1");
    await waitFor(async () => { try { return (await readFile(desktop.log, "utf8")).includes("codex://threads/" + desktop.thread); } catch { return false; } });
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
    const piSignal = await call(f.service, "cito_send", { to: pi.listener_id, message: "Pi only" });
    const codexSignal = await call(f.service, "cito_send", { to: codex.listener_id, message: "Codex only" });
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
  expect(f.service.state.listener(toCanonicalListenerId(id))!.persisted_seq).toBe(1);
  expect(f.service.state.signals("SELECT * FROM local_signals")).toHaveLength(1);
}, 10_000);

test("cito-mcp stdio advertises the Claude channel, preserves first-turn session_id across transcript creation, supports UI listen_session, and acknowledges exact drained batch_ids via cito-claude-hook", async () => {
  const f = await setup(false);
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  (f.service as unknown as { config: { daemonPort: number } }).config.daemonPort = http.port;
  const client = new Client({ name: "stdio-claude-e2e", version: "1" });
  const session = `claude-${process.pid}`;
  const transcriptUuid = "11111111-2222-3333-4444-555555555555";
  try {
    await client.connect(new StdioClientTransport({
      command: process.execPath,
      args: [resolve("bin/cito-mcp")],
      env: { PATH: process.env.PATH!, ...f.env, CITO_DAEMON_PORT: String(http.port), CITO_CLAUDE_CHANNEL: "1" },
      stderr: "pipe",
    }));
    expect(client.getServerCapabilities()?.experimental).toMatchObject({ "claude/channel": {} });
    // UI listen_session passes both endpoint_pid and expected_session_id and must resolve via socket bridge without conflict.
    await (f.service as unknown as { handleUiAction(action: unknown): Promise<void> }).handleUiAction({
      op: "listen_session",
      harness: "claude",
      session_id: session,
      endpoint_pid: process.pid,
      cwd: f.root,
      title: "claude code session",
    });
    const enrolled = payload(await client.callTool({ name: "cito_listen", arguments: { about: "claude code session" } }) as { content: { text: string }[] });
    expect(enrolled).toMatchObject({ status: "listening", harness: "claude", session_id: session });
    // First-turn transcript creation on disk aliases transcriptUuid -> claude-<pid> without revoking the listener.
    const projDir = join(f.claudeHome, "projects", process.cwd().replace(/[^a-zA-Z0-9]/g, "-"));
    await mkdir(projDir, { recursive: true });
    await writeFile(join(projDir, transcriptUuid + ".jsonl"), "{}\n");
    const listenerCanon = toCanonicalListenerId(String(enrolled.listener_id));
    await call(f.service, "cito_send", { to: String(enrolled.listener_id), message: "ship the adapter" });
    await waitFor(
      async () =>
        (await peekClaudeWakes(f.config, transcriptUuid)).length > 0
        && f.service.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 1
        && f.service.state.listener(listenerCanon)?.dispatcher_token === null,
      10_000,
    );
    const queuedWakes = await peekClaudeWakes(f.config, transcriptUuid);
    expect(queuedWakes[0]!.content).toContain("ship the adapter");
    // Insert a newly arrived RECEIVED signal that is NOT part of the drained batch; hook ack must not touch it.
    f.service.state.run(
      "UPDATE local_listeners SET dispatcher_token = 'hold', lease_until_ms = ? WHERE listener_id = ?",
      Date.now() + 30_000,
      listenerCanon,
    );
    f.service.state.run(
      `INSERT INTO local_signals (hub, signal_id, listener_id, seq, binding_generation, from_user, from_host_name,
        from_harness, from_listener_id, from_title, from_about, message, held_reason, in_reply_to, created_at,
        expires_at_ms, state) VALUES ('default', 'sig_unbatched_new', ?, 99, 2, 'bob', 'mac', 'pi', NULL, 'bob', NULL,
        'later message', NULL, NULL, ?, ?, 'RECEIVED')`,
      listenerCanon, new Date().toISOString(), Date.now() + 60_000,
    );
    const hookProc = spawn(process.execPath, [resolve("bin/cito-claude-hook")], {
      stdio: ["pipe", "pipe", "pipe"],
      env: { PATH: process.env.PATH!, ...f.env, CITO_DAEMON_PORT: String(http.port) },
    });
    let hookOut = "";
    hookProc.stdout.on("data", chunk => { hookOut += String(chunk); });
    const hookClosed = new Promise<number | null>(r => hookProc.once("close", r));
    hookProc.stdin.end(JSON.stringify({ hook_event_name: "Stop", session_id: transcriptUuid, cwd: process.cwd() }));
    expect(await hookClosed).toBe(0);
    expect(JSON.parse(hookOut.trim())).toMatchObject({ decision: "block", reason: expect.stringContaining("ship the adapter") });
    const rows = f.service.state.signals("SELECT * FROM local_signals ORDER BY seq ASC");
    expect(rows.find(r => r.batch_id === queuedWakes[0]!.batch_id)?.acknowledged_at_ms).toEqual(expect.any(Number));
    expect(rows.find(r => r.signal_id === "sig_unbatched_new")?.acknowledged_at_ms).toBeNull();
    expect(rows.find(r => r.signal_id === "sig_unbatched_new")?.state).toBe("RECEIVED");
  } finally {
    await client.close().catch(() => {});
    await http.close();
  }
}, 30_000);

test("plain cito-mcp stdio forwarding discovers parent Pi bridge and exits cleanly on SIGINT and SIGTERM", async () => {
  const f = await setup(true);
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const client = new Client({ name: "stdio-pi-e2e", version: "1" });
  try {
    await client.connect(new StdioClientTransport({
      command: process.execPath,
      args: [resolve("bin/cito-mcp")],
      env: { PATH: process.env.PATH!, ...f.env, CITO_DAEMON_PORT: String(http.port) },
      stderr: "pipe",
    }));
    expect(client.getServerCapabilities()).toMatchObject({ tools: {} });
    expect(client.getServerCapabilities()?.experimental).toBeUndefined();
    const enrollment = await client.callTool({ name: "cito_listen", arguments: { about: "stdio pi receiver" } });
    expect(payload(enrollment as { content: { text: string }[] })).toMatchObject({ status: "listening", harness: "pi", session_id: "pi-session-1" });
    await client.close();
    for (const signal of ["SIGINT", "SIGTERM"] as const) {
      const child = spawn(process.execPath, [resolve("bin/cito-mcp")], {
        stdio: ["pipe", "pipe", "pipe"],
        env: { ...process.env, ...f.env, CITO_DAEMON_PORT: String(http.port) },
      });
      let stdout = "", stderr = "";
      child.stdout.on("data", chunk => { stdout += chunk.toString(); });
      child.stderr.on("data", chunk => { stderr += chunk.toString(); });
      const exited = new Promise<number | null>(resolveExit => child.once("exit", resolveExit));
      try {
        child.stdin.write(JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "initialize",
          params: { protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "sig-test", version: "1" } },
        }) + "\n");
        await waitFor(() => stdout.includes('"result"'), 5000);
        child.kill(signal);
        expect(await exited, stderr).toBe(0);
      } finally {
        if (child.exitCode === null) child.kill("SIGKILL");
        await exited;
      }
    }
  } finally {
    await client.close();
    await http.close();
  }
}, 15_000);
