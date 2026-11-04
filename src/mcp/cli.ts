import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { loadConfig, type CitoConfig } from "../shared/index.js";
import { CitoService } from "./service.js";
import { LocalState } from "./state.js";
import { startHttpServer, daemonHealth } from "./server.js";

function alive(pid: number): boolean { try { process.kill(pid, 0); return true; } catch { return false; } }
async function runDaemon(config: CitoConfig): Promise<void> {
  process.title = "cito-daemon";
  const state = await LocalState.open(config.stateDir);
  let claimed = false, ownershipClosed = false;
  let service: CitoService | undefined;
  try {
    for (;;) {
      const existing = state.daemonOwner();
      if (existing && alive(existing.pid)) {
        let health: Awaited<ReturnType<typeof daemonHealth>>;
        const deadline = performance.now() + 5000;
        do {
          health = await daemonHealth({ ...config, daemonPort: existing.port });
          if (health?.pid === existing.pid || !alive(existing.pid)) break;
          await delay(50);
        } while (performance.now() < deadline);
        if (health?.pid === existing.pid) {
          if (existing.port !== config.daemonPort) throw new Error(`Daemon already owns ${config.stateDir} on port ${existing.port}; requested port ${config.daemonPort}`);
          state.close(); return;
        }
        if (alive(existing.pid)) throw new Error(`Daemon owner PID ${existing.pid} is alive but health is unavailable; refusing duplicate ownership of ${config.stateDir}`);
      }
      if (state.claimDaemon(process.pid, config.daemonPort, existing)) { claimed = true; break; }
    }
    let binding: ReturnType<typeof startHttpServer> | undefined;
    let stopping: Promise<void> | undefined;
    // Ownership has its own handle and outlives every service operation and badge update.
    service = await CitoService.create(config, { onQuit: () => stop() });
    const daemonService = service;
    const stop = () => stopping ??= (async () => {
      process.removeListener("SIGINT", onSignal); process.removeListener("SIGTERM", onSignal);
      const http = await binding?.catch(() => undefined);
      await http?.close();
      try { await daemonService.close(); state.releaseDaemon(process.pid); }
      finally { state.close(); ownershipClosed = true; }
    })();
    const onSignal = () => { void stop().catch(error => { console.error(error); process.exitCode = 1; }); };
    process.once("SIGINT", onSignal); process.once("SIGTERM", onSignal);
    try {
      binding = startHttpServer(config, daemonService);
      await binding;
      if (stopping) { await stopping; return; }
      // HTTP readiness must not depend on a cold Swift compile.
      await daemonService.startUi();
      if (!stopping) void daemonService.connectBackground();
    } catch (error) { await stop(); throw error; }
  } catch (error) {
    if (!ownershipClosed) {
      try { await service?.close(); if (claimed) state.releaseDaemon(process.pid); }
      finally { state.close(); }
    }
    throw error;
  }
}
async function ensureDaemon(config: CitoConfig): Promise<void> {
  if (await daemonHealth(config)) return;
  const entry = fileURLToPath(new URL("../../../bin/cito-mcp", import.meta.url));
  const sourceEntry = fileURLToPath(new URL("../../bin/cito-mcp", import.meta.url));
  const { existsSync } = await import("node:fs");
  const child = spawn(process.execPath, [existsSync(entry) ? entry : sourceEntry, "daemon"], { detached: true, stdio: "ignore", env: process.env });
  await new Promise<void>((resolve, reject) => { child.once("spawn", resolve); child.once("error", reject); });
  child.unref();
  const deadline = performance.now() + 5000;
  while (performance.now() < deadline) { if (await daemonHealth(config)) return; await delay(50); }
  throw new Error(`Daemon did not become healthy on 127.0.0.1:${config.daemonPort} for ${config.stateDir} within 5000ms`);
}
export async function main(): Promise<void> {
  const config = loadConfig();
  if (process.argv[2] === "daemon") { await runDaemon(config); return; }
  await ensureDaemon(config);
  const stdio = new StdioServerTransport();
  const http = new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${config.daemonPort}/mcp`));
  const report = (error: unknown) => { console.error(error); process.exitCode = 1; };
  let stopped = false;
  const stop = async () => {
    if (stopped) return; stopped = true;
    process.removeListener("SIGINT", onSignal); process.removeListener("SIGTERM", onSignal);
    process.stdin.removeListener("end", onSignal);
    await http.close(); await stdio.close();
  };
  const onSignal = () => { void stop().catch(report); };
  http.onmessage = message => { void stdio.send(message).catch(report); };
  stdio.onmessage = message => { void http.send(message).catch(error => {
    if ("id" in message) void stdio.send({ jsonrpc: "2.0", id: message.id!, error: { code: -32603, message: String(error) } }).catch(report);
    else report(error);
  }); };
  http.onerror = report; stdio.onerror = report; stdio.onclose = onSignal;
  process.once("SIGINT", onSignal); process.once("SIGTERM", onSignal); process.stdin.once("end", onSignal);
  try { await http.start(); await stdio.start(); } catch (error) { await stop(); throw error; }
}
