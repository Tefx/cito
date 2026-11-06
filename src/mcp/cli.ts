import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { spawn } from "node:child_process";
import { basename } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { CITO_VERSION, loadConfig, type CitoConfig } from "../shared/index.js";
import { startOptionalStdioChannel } from "../adapters/index.js";
import { readCodexThreadOptional, resolveCodexThreadId } from "../adapters/codex.js";
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
  const existing = await daemonHealth(config);
  if (existing?.version === CITO_VERSION) return;
  if (existing) {
    try { process.kill(existing.pid, "SIGTERM"); } catch {}
    const stopDeadline = performance.now() + 5000;
    while (alive(existing.pid) && performance.now() < stopDeadline) await delay(50);
  }
  const entry = fileURLToPath(new URL("../../../bin/cito-mcp", import.meta.url));
  const sourceEntry = fileURLToPath(new URL("../../bin/cito-mcp", import.meta.url));
  const { existsSync } = await import("node:fs");
  const child = spawn(process.execPath, [existsSync(entry) ? entry : sourceEntry, "daemon"], { detached: true, stdio: "ignore", env: process.env });
  await new Promise<void>((resolve, reject) => { child.once("spawn", resolve); child.once("error", reject); });
  child.unref();
  const deadline = performance.now() + 5000;
  while (performance.now() < deadline) { if ((await daemonHealth(config))?.version === CITO_VERSION) return; await delay(50); }
  throw new Error(`Daemon did not become healthy on 127.0.0.1:${config.daemonPort} for ${config.stateDir} within 5000ms`);
}
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
async function callDaemonTool(config: CitoConfig, name: "cito_listen" | "cito_unlisten",
  args: Record<string, unknown>, meta: Record<string, unknown>): Promise<Record<string, unknown>> {
  await ensureDaemon(config);
  const res = await fetch(`http://127.0.0.1:${config.daemonPort}/mcp`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name, arguments: args, _meta: meta },
    }),
  });
  const raw = await res.text();
  const dataLines = raw.split(/\r?\n/).filter(line => line.startsWith("data:")).map(line => line.slice(5).trim()).filter(Boolean);
  const jsonText = dataLines.at(-1) ?? (raw.trim().startsWith("{") ? raw.trim() : "");
  if (!jsonText) throw new Error(raw.trim() || `HTTP ${res.status}`);
  const rpc = JSON.parse(jsonText) as { error?: { message?: unknown }; result?: { isError?: boolean; content?: { type?: string; text?: string }[] } };
  if (rpc.error) throw new Error(typeof rpc.error.message === "string" ? rpc.error.message : JSON.stringify(rpc.error));
  const text = rpc.result?.content?.find(part => part?.type === "text" && typeof part.text === "string")?.text;
  let payload: Record<string, unknown> = {};
  if (text) {
    try { payload = JSON.parse(text) as Record<string, unknown>; }
    catch { if (rpc.result?.isError || !res.ok) throw new Error(text); }
  }
  if (rpc.result?.isError || payload.ok === false || !res.ok) {
    const errObj = payload.error as { message?: unknown } | string | undefined;
    const msg = typeof errObj === "object" && errObj && typeof errObj.message === "string"
      ? errObj.message : typeof errObj === "string" ? errObj : text || `HTTP ${res.status}`;
    throw new Error(msg);
  }
  return payload;
}
async function runDirectCommand(cmd: "listen" | "unlisten", argv: string[], config: CitoConfig): Promise<void> {
  try {
    let explicitScope: "self" | "workspace" | undefined;
    let team = false, hub: string | undefined, tag: string | undefined, useCodex = false, explicitCodexId: string | undefined;
    const positionals: string[] = [], cwd = process.cwd();
    const readValue = (arg: string, index: number, flag: string): [string, number] => {
      if (arg.startsWith(flag + "=")) {
        const val = arg.slice(flag.length + 1).trim();
        if (!val) throw new Error(`Missing value for ${flag}`);
        return [val, index];
      }
      const next = argv[index + 1];
      if (next === undefined || next.startsWith("-")) throw new Error(`Missing value for ${flag}`);
      return [next, index + 1];
    };
    for (let i = 0; i < argv.length; i++) {
      const arg = argv[i]!;
      if (arg === "--hub" || arg.startsWith("--hub=")) {
        [hub, i] = readValue(arg, i, "--hub");
      } else if (arg === "--codex" || arg.startsWith("--codex=")) {
        useCodex = true;
        if (arg.startsWith("--codex=")) {
          const val = arg.slice("--codex=".length).trim();
          if (!val) throw new Error("Missing value for --codex");
          explicitCodexId = val;
        } else {
          const next = argv[i + 1];
          if (next !== undefined && !next.startsWith("-")) {
            if (cmd === "unlisten" || UUID_RE.test(next) || readCodexThreadOptional(next, config).status !== "unavailable") {
              explicitCodexId = next;
              i++;
            }
          }
        }
      } else if (cmd === "listen" && arg === "--team") {
        team = true;
      } else if (cmd === "listen" && (arg === "--scope" || arg.startsWith("--scope="))) {
        let val: string;
        [val, i] = readValue(arg, i, "--scope");
        if (val !== "self" && val !== "workspace") throw new Error(`Invalid --scope: ${val}`);
        if (explicitScope && explicitScope !== val) throw new Error("Conflicting --scope values");
        explicitScope = val;
      } else if (cmd === "listen" && (arg === "--tag" || arg.startsWith("--tag="))) {
        [tag, i] = readValue(arg, i, "--tag");
      } else if (arg.startsWith("-")) {
        throw new Error(`Unknown flag: ${arg}`);
      } else {
        positionals.push(arg);
      }
    }
    if (cmd === "unlisten" && positionals.length > 0) throw new Error(`Unexpected argument: ${positionals[0]}`);
    if (team && explicitScope === "self") throw new Error("Conflicting --scope self and --team");
    const meta: Record<string, unknown> = useCodex
      ? { codexThreadId: explicitCodexId ?? resolveCodexThreadId(cwd, config, { allowArchived: cmd === "unlisten" }), cwd }
      : { caller_pid: process.ppid };
    if (cmd === "listen") {
      const about = positionals.join(" ").trim() || basename(cwd) || "CLI session";
      const scope = team ? "workspace" : (explicitScope ?? "self");
      const payload = await callDaemonTool(config, "cito_listen", {
        about, scope, ...(hub !== undefined ? { hub } : {}), ...(tag !== undefined ? { tag } : {}),
      }, meta);
      process.stdout.write(`Listening as ${String(payload.listener_id)} (${String(payload.scope)}): ${String(payload.about)}\n`);
    } else {
      const payload = await callDaemonTool(config, "cito_unlisten", { ...(hub !== undefined ? { hub } : {}) }, meta);
      const removed = payload.removed ?? (payload.status === "unlistened" ? 1 : 0);
      process.stdout.write(`Unlistened (${String(removed)} removed)\n`);
    }
  } catch (error) {
    process.stderr.write((error instanceof Error ? error.message : String(error)) + "\n");
    process.exitCode = 1;
  }
}
export async function main(): Promise<void> {
  const cmd = process.argv[2];
  if (cmd === "--version" || cmd === "-v") { process.stdout.write(`cito v${CITO_VERSION}\n`); return; }
  const config = loadConfig();
  if (cmd === "daemon") { await runDaemon(config); return; }
  if (cmd === "listen" || cmd === "unlisten") { await runDirectCommand(cmd, process.argv.slice(3), config); return; }
  await ensureDaemon(config);
  const stdio = new StdioServerTransport();
  const http = new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${config.daemonPort}/mcp`));
  const report = (error: unknown) => { console.error(error); process.exitCode = 1; };
  // An unbindable wake channel must never keep the MCP server itself from forwarding.
  const channel = await startOptionalStdioChannel(config, stdio);
  let stopped = false, stopping: Promise<void> | undefined;
  const stop = () => stopping ??= (async () => {
    stopped = true;
    process.removeListener("SIGINT", onSignal); process.removeListener("SIGTERM", onSignal);
    process.stdin.removeListener("end", onSignal);
    await channel?.close();
    await http.close(); await stdio.close();
    process.stdout.destroy();
  })();
  const onSignal = () => { void stop().catch(report); };
  http.onmessage = message => { if (!stopped) void stdio.send(channel ? channel.forward(message) : message).catch(report); };
  stdio.onmessage = message => { if (stopped) return; void http.send(message).catch(error => {
    if ("id" in message) void stdio.send({ jsonrpc: "2.0", id: message.id!, error: { code: -32603, message: String(error) } }).catch(report);
    else report(error);
  }); };
  http.onerror = report; stdio.onerror = report; stdio.onclose = onSignal;
  process.once("SIGINT", onSignal); process.once("SIGTERM", onSignal); process.stdin.once("end", onSignal);
  try {
    await http.start();
    if (stopped) { await stop(); return; }
    await stdio.start();
  } catch (error) { await stop(); throw error; }
}
