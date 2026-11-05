import { expect, test } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { z } from "zod";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { stat } from "node:fs/promises";
import { startHttpServer } from "../../src/mcp/server.js";
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
    await client.close();
    await waitFor(async () => !(await stat(path).catch(() => undefined)));
    expect((await fetch(`http://127.0.0.1:${http.port}/health`)).status).toBe(200);
  } finally { await client.close(); await http.close(); await f.close(); }
}, 20000);

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
