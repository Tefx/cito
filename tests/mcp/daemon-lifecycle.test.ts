import { expect, test } from "vitest";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { resolve } from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { daemonHealth } from "../../src/mcp/server.js";
import { LocalState } from "../../src/mcp/state.js";
import { loadConfig } from "../../src/shared/index.js";
import { startNats } from "../shared/nats-server.js";
import { freePort, waitFor } from "./support.js";

function daemon(env: NodeJS.ProcessEnv, instrumented = true) {
  const args = instrumented ? ["--import", "tsx", resolve("tests/mcp/fixtures/daemon-lifecycle.ts"), "daemon"] : [resolve("bin/cito-mcp"), "daemon"];
  const child = spawn(process.execPath, args, { env, stdio: ["ignore", "ignore", "pipe", "ipc"] });
  let stderr = ""; const events: Record<string, unknown>[] = [];
  child.stderr!.on("data", chunk => { stderr += chunk; });
  child.on("message", value => { if (value && typeof value === "object") events.push(value as Record<string, unknown>); });
  const exited = new Promise<number | null>(resolve => child.once("exit", resolve));
  return {
    child, exited, events, stderr: () => stderr,
    async event(name: string) {
      await waitFor(() => events.some(event => event.event === name), 10000).catch(error => { throw new Error(String(error) + ": " + stderr); });
      return events.find(event => event.event === name)!;
    },
    async stop() {
      if (child.exitCode !== null || child.signalCode !== null) return;
      if (child.connected) { child.send("release_operation"); child.send("release_ui"); }
      child.kill("SIGTERM");
      const timeout = setTimeout(() => child.kill("SIGKILL"), 5000);
      try { await exited; } finally { clearTimeout(timeout); }
    },
  };
}

test("daemon ownership survives HTTP shutdown until an in-flight service operation and service.close finish", async () => {
  const broker = await startNats(), env = { ...process.env, ...broker.env, CITO_DAEMON_PORT: String(await freePort()), CITO_UI_HEADLESS: "1", CITO_NATS_URL: broker.url };
  const original = daemon(env), client = new Client({ name: "shutdown-operation", version: "1" });
  let contender: ReturnType<typeof daemon> | undefined, replacement: ReturnType<typeof daemon> | undefined, state: LocalState | undefined;
  let operation: Promise<unknown> | undefined;
  try {
    await original.event("ui_ready");
    await client.connect(new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${env.CITO_DAEMON_PORT}/mcp`)));
    original.child.send("hold_operation"); await original.event("operation_armed");
    operation = client.callTool({ name: "cito_find", arguments: {} }).catch(error => error);
    await original.event("operation_held");
    original.child.kill("SIGTERM"); await original.event("service_closing");
    await waitFor(async () => !await daemonHealth(loadConfig(env)));
    state = await LocalState.open(loadConfig(env).stateDir);
    expect(state.daemonOwner()?.pid).toBe(original.child.pid);
    contender = daemon(env, false);
    await waitFor(() => contender!.child.exitCode !== null, 7500);
    expect(await contender.exited).toBe(1); expect(contender.stderr()).toContain("refusing duplicate ownership");
    expect(state.daemonOwner()?.pid).toBe(original.child.pid);
    expect(original.events.some(event => event.event === "service_closed")).toBe(false);
    original.child.send("release_operation"); expect(await original.exited).toBe(0);
    expect(state.daemonOwner()).toBeUndefined();
    replacement = daemon(env, false);
    await waitFor(async () => (await daemonHealth(loadConfig(env)))?.pid === replacement!.child.pid);
    expect(state.daemonOwner()?.pid).toBe(replacement.child.pid);
  } finally {
    await client.close(); await original.stop(); await operation;
    await contender?.stop(); await replacement?.stop(); state?.close(); await broker.stop();
  }
}, 20000);

test("HTTP and the stdio forwarder are ready while UI startup exceeds five seconds; shutdown awaits and stops that startup", async () => {
  const broker = await startNats(), env = { ...process.env, ...broker.env, CITO_DAEMON_PORT: String(await freePort()), CITO_UI_HEADLESS: "1", CITO_NATS_URL: broker.url, CITO_TEST_HOLD_UI: "1" };
  const child = daemon(env), began = performance.now(), client = new Client({ name: "cold-ui-forwarder", version: "1" });
  let state: LocalState | undefined;
  try {
    await child.event("ui_starting");
    expect(await daemonHealth(loadConfig(env))).toMatchObject({ pid: child.child.pid });
    const transport = new StdioClientTransport({ command: process.execPath, args: [resolve("bin/cito-mcp")], env: env as Record<string, string>, stderr: "pipe" });
    await client.connect(transport); expect((await client.listTools()).tools).toHaveLength(5);
    expect(performance.now() - began).toBeLessThan(5000);
    await delay(5200);
    expect(child.events.some(event => event.event === "ui_ready")).toBe(false);
    await client.close(); child.child.kill("SIGTERM"); await child.event("service_closing");
    state = await LocalState.open(loadConfig(env).stateDir);
    expect(state.daemonOwner()?.pid).toBe(child.child.pid); expect(child.child.exitCode).toBeNull();
    child.child.send("release_ui");
    expect(await child.exited).toBe(0);
    expect(child.events.find(event => event.event === "ui_ready")).toMatchObject({ child_spawned: false });
    expect(state.daemonOwner()).toBeUndefined();
  } finally { await client.close(); await child.stop(); state?.close(); await broker.stop(); }
}, 15000);
