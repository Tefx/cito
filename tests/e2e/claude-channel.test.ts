import { expect, test } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { z } from "zod";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { stat } from "node:fs/promises";
import { startHttpServer } from "../../src/mcp/server.js";
import { encode } from "../../src/mcp/receiver.js";
import { bridgeSocketPath, requestBridge } from "../../src/adapters/pi.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import { fixture, call, payload, waitFor } from "../mcp/support.js";

const channelNotification = z.object({ method: z.literal("notifications/claude/channel"),
  params: z.object({ content: z.string(), meta: z.object({ origin: z.literal("cito") }) }) });

test("opt-in stdio channel advertises capability, pushes real broker delivery and preserves inbox drain fallback", async () => {
  const f = await fixture({ pi: false });
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const path = bridgeSocketPath(f.stateDir, "claude", process.pid);
  const client = new Client({ name: "channel-e2e", version: "1" });
  const notifications: z.infer<typeof channelNotification>[] = [];
  client.setNotificationHandler(channelNotification, notification => { notifications.push(notification); });
  const transport = new StdioClientTransport({ command: process.execPath, args: [resolve("bin/cito-mcp")],
    env: { PATH: process.env.PATH!, ...f.env, CITO_DAEMON_PORT: String(http.port), CITO_CLAUDE_CHANNEL: "1", CLAUDE_SESSION_ID: "channel-session" }, stderr: "pipe" });
  try {
    await client.connect(transport);
    expect(client.getServerCapabilities()).toMatchObject({ tools: {}, experimental: { "claude/channel": {} } });
    expect((await stat(path)).mode & 0o777).toBe(0o600);
    expect(await requestBridge(path, { op: "identify" }, 1500)).toMatchObject({ endpoint_pid: process.pid, session_id: "channel-session", cwd: process.cwd() });
    // The forwarder is discovered through the real HTTP client's OS PID ancestry.
    const enrollment = await client.callTool({ name: "cito_listen", arguments: { about: "channel receiver" } });
    const listener = payload(enrollment as { content: { text: string }[] });
    expect(listener).toMatchObject({ status: "listening", harness: "claude", session_id: "channel-session" });
    const sent = await call(f.service, "cito_send", { to: listener.listener_id, message: "Review available" });
    const receiver = f.service.receivers.get(`default:${toCanonicalListenerId(listener.listener_id as string)}`)!;
    await waitFor(() => f.service.state.signals("SELECT * FROM local_signals").length === 1);
    await receiver.dispatch();
    await waitFor(() => notifications.length === 1);
    expect(notifications[0]!.params).toMatchObject({ content: expect.stringContaining("Review available"), meta: { origin: "cito" } });
    const stored = f.service.state.signals("SELECT * FROM local_signals")[0]!;
    expect(stored).toMatchObject({ state: "DISPATCHED", acknowledged_at_ms: null });
    expect(await requestBridge(path, { op: "deliver", session_id: "channel-session", batch_id: stored.batch_id, content: "duplicate" }, 3000))
      .toMatchObject({ delivered: true, deduplicated: true });
    const inbox = await client.callTool({ name: "cito_inbox", arguments: {} });
    expect(payload(inbox as { content: { text: string }[] })).toMatchObject({ acknowledged_count: 1, signals: [{ signal_id: sent.signal_id, message: "Review available" }] });
    expect(notifications).toHaveLength(1);

    // Hold the persisted dispatcher lease while the real broker admits the entire burst.
    const id = toCanonicalListenerId(listener.listener_id as string), slot = f.service.slots.get("default")!;
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
      f.service.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE listener_id = ?", id);
      return messages;
    };
    const burstMessages = await publishBurst(600, "", "burst");
    const firstBurstNotification = notifications.length;
    await receiver.dispatch(); // A single dispatch must continue draining without another admission.
    await waitFor(() => f.service.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 601, 5000);
    await waitFor(() => notifications.length === firstBurstNotification + 12);
    const burstCards = notifications.slice(firstBurstNotification).map(notification => notification.params.content);
    expect(burstCards.every(card => card.startsWith("🔔 Cito · 50 notifications"))).toBe(true);
    expect(burstCards.every(card => Buffer.byteLength(JSON.stringify(card)) <= 256 * 1024)).toBe(true);
    expect(burstCards.reduce((n, card) => n + Buffer.byteLength(card), 0)).toBeGreaterThan(1_048_576);
    expect(f.service.state.signals("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")).toHaveLength(600);
    const drained: string[] = [];
    for (let i = 0; i < 12; i++) {
      const page = payload(await client.callTool({ name: "cito_inbox", arguments: {} }) as { content: { text: string }[] });
      expect(page.acknowledged_count).toBe(50);
      drained.push(...(page.signals as { message: string }[]).map(signal => signal.message));
    }
    expect(drained).toEqual(burstMessages);

    // Long metadata exercises the serialized-byte ceiling before the 50-row ceiling.
    await publishBurst(12, "<".repeat(15000), "large");
    const firstLargeNotification = notifications.length;
    await receiver.dispatch();
    await waitFor(() => f.service.state.signals("SELECT * FROM local_signals WHERE state = 'DISPATCHED'").length === 613, 5000);
    await waitFor(() => notifications.length >= firstLargeNotification + 3);
    const largeCards = notifications.slice(firstLargeNotification).map(notification => notification.params.content);
    expect(largeCards).toHaveLength(3);
    expect(largeCards.every(card => Buffer.byteLength(JSON.stringify(card)) <= 256 * 1024)).toBe(true);
    const batchSizes = f.service.state.db.prepare("SELECT COUNT(*) AS n FROM local_signals WHERE signal_id LIKE 'large%' GROUP BY batch_id").all();
    expect(batchSizes.map(row => row.n)).toEqual([4, 4, 4]);

    // A single oversized sender title stays in the inbox instead of wedging subsequent batches.
    await publishBurst(1, "<".repeat(70000), "oversized");
    await publishBurst(1, "", "after");
    await receiver.dispatch();
    await waitFor(() => notifications.length === firstLargeNotification + 4);
    expect(notifications.at(-1)!.params.content).toContain("after0000");
    expect(f.service.state.signals("SELECT * FROM local_signals WHERE signal_id = 'oversized0'")[0])
      .toMatchObject({ state: "STORED_PASSIVE", held_reason: "driver_unavailable", acknowledged_at_ms: null });
    const finalInbox = payload(await client.callTool({ name: "cito_inbox", arguments: {} }) as { content: { text: string }[] });
    expect(finalInbox).toMatchObject({ acknowledged_count: 14 });
    expect(notifications).toHaveLength(firstLargeNotification + 4);
    await client.close();
    await waitFor(async () => !(await stat(path).catch(() => undefined)));
    expect((await fetch(`http://127.0.0.1:${http.port}/health`)).status).toBe(200);
  } finally { await client.close(); await http.close(); await f.close(); }
}, 30000);

test("stdio channel startup and idle shutdown remove owned sockets on SIGINT and SIGTERM", async () => {
  const f = await fixture({ pi: false });
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const path = bridgeSocketPath(f.stateDir, "claude", process.pid);
  try {
    for (const signal of ["SIGINT", "SIGTERM"] as const) {
      const child = spawn(process.execPath, [resolve("bin/cito-mcp")], { stdio: ["pipe", "ignore", "pipe"],
        env: { ...process.env, ...f.env, CITO_DAEMON_PORT: String(http.port), CITO_CLAUDE_CHANNEL: "1" } });
      let stderr = ""; child.stderr.on("data", chunk => { stderr += chunk; });
      const exited = new Promise<number | null>(resolve => child.once("exit", resolve));
      try {
        await waitFor(async () => !!await stat(path).catch(() => undefined));
        child.kill(signal);
        expect(await exited, stderr).toBe(0);
        await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
      } finally { if (child.exitCode === null) child.kill("SIGKILL"); await exited; }
    }
  } finally { await http.close(); await f.close(); }
}, 15000);

test("ordinary stdio forwarding leaves experimental channel disabled and creates no host socket", async () => {
  const f = await fixture({ pi: false }), client = new Client({ name: "no-channel", version: "1" });
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  try {
    await client.connect(new StdioClientTransport({ command: process.execPath, args: [resolve("bin/cito-mcp")],
      env: { PATH: process.env.PATH!, ...f.env, CITO_DAEMON_PORT: String(http.port) }, stderr: "pipe" }));
    expect(client.getServerCapabilities()?.experimental?.["claude/channel"]).toBeUndefined();
    await expect(stat(bridgeSocketPath(f.stateDir, "claude", process.pid))).rejects.toMatchObject({ code: "ENOENT" });
  } finally { await client.close(); await http.close(); await f.close(); }
}, 15000);
