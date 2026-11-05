import { expect, test, vi } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { startHttpServer, parseClientPid, httpMetadata, daemonHealth } from "../../src/mcp/server.js";
import { LocalState } from "../../src/mcp/state.js";
import { loadConfig } from "../../src/shared/index.js";
import type { CallerInput } from "../../src/adapters/index.js";
import { fixture, waitFor, freePort } from "./support.js";

const exec = promisify(execFile);
test("HTTP binds loopback, enforces Origin, attributes real client PID, rejects spoofing and merges Codex headers", async () => {
  const f = await fixture();
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const base = `http://127.0.0.1:${http.port}`;
  const client = new Client({ name: "http-test", version: "1" });
  const captured: CallerInput[] = [], original = f.service.call.bind(f.service);
  const spy = vi.spyOn(f.service, "call").mockImplementation((name, input, meta) => { captured.push(meta ?? {}); return original(name, input, meta); });
  try {
    expect(http.http.address()).toMatchObject({ address: "127.0.0.1" });
    expect(await (await fetch(base + "/health")).json()).toEqual({ ok: true, pid: process.pid, stateDir: f.stateDir });
    for (const origin of ["https://example.com", "null", "not a URL"]) expect((await fetch(base + "/mcp", { headers: { origin } })).status).toBe(403);
    for (const origin of ["http://localhost:9999", "http://127.0.0.1"]) expect((await fetch(base + "/health", { headers: { origin } })).status).toBe(200);
    await client.connect(new StreamableHTTPClientTransport(new URL(base + "/mcp")));
    const result = await client.callTool({ name: "cito_listen", arguments: { about: "OS attributed caller" }, _meta: { http_transport: false, http_client_pid: 999999 } });
    expect(JSON.parse((result.content as { text: string }[])[0]!.text)).toMatchObject({ status: "listening", harness: "pi" });
    expect(captured.at(-1)).toMatchObject({ httpTransport: true, httpClientPid: process.pid });
    const latestProtocol = await fetch(base + "/mcp", { method: "POST", headers: {
      "content-type": "application/json", accept: "application/json, text/event-stream", "MCP-Protocol-Version": "2026-07-28",
    }, body: JSON.stringify({ jsonrpc: "2.0", id: 101, method: "tools/list" }) });
    expect(latestProtocol.status).toBe(200);
    expect(await latestProtocol.text()).toContain("cito_listen");
    await client.close();
    const codex = new Client({ name: "header-test", version: "1" });
    try {
      await codex.connect(new StreamableHTTPClientTransport(new URL(base + "/mcp"), { requestInit: { headers: { "x-codex-turn-metadata": JSON.stringify({ thread_id: "http-thread" }) } } }));
      const success = await codex.callTool({ name: "cito_listen", arguments: { about: "header claim" }, _meta: { cwd: f.root } });
      expect(JSON.parse((success.content as { text: string }[])[0]!.text)).toMatchObject({ status: "listening", harness: "codex", session_id: "http-thread" });
      const conflict = await codex.callTool({ name: "cito_listen", arguments: { about: "conflict" }, _meta: { codexThreadId: "other" } });
      expect(JSON.parse((conflict.content as { text: string }[])[0]!.text)).toMatchObject({ error: { code: "INVALID_INPUT" } });
    } finally { await codex.close(); }
    const events = await fetch(base + "/mcp", { headers: { accept: "text/event-stream" } });
    expect(events.status).toBe(200); expect(events.headers.get("content-type")).toContain("text/event-stream"); await events.body?.cancel();
    expect((await fetch(base + "/mcp", { method: "DELETE" })).status).toBe(200);
    expect((await f.service.call("cito_find")).isError).toBe(false);
  } finally { spy.mockRestore(); await client.close(); await http.close(); await f.close(); }
}, 15000);

test("stdio forwarder walks the HTTP forwarder PID to its parent socket bridge and leaves the daemon alive on EOF", async () => {
  const f = await fixture();
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const client = new Client({ name: "stdio-ancestor", version: "1" });
  const transport = new StdioClientTransport({ command: process.execPath, args: [resolve("bin/cito-mcp")],
    env: { PATH: process.env.PATH!, ...f.env, CITO_DAEMON_PORT: String(http.port), CITO_UI_HEADLESS: "1" }, stderr: "pipe" });
  try {
    await client.connect(transport);
    const result = await client.callTool({ name: "cito_listen", arguments: { about: "forwarded ancestor" } });
    expect(JSON.parse((result.content as { text: string }[])[0]!.text)).toMatchObject({ status: "listening", harness: "pi", session_id: "pi-session-1" });
    await client.close();
    expect((await fetch(`http://127.0.0.1:${http.port}/health`)).status).toBe(200);
  } finally { await client.close(); await http.close(); await f.close(); }
}, 15000);

test("PID parser matches the client side endpoint and excludes daemon PID when another owner matches", () => {
  expect(parseClientPid(`p${process.pid}\nn127.0.0.1:5000->127.0.0.1:4290\np456\nn127.0.0.1:5000->127.0.0.1:4290\np789\nn127.0.0.1:4290->127.0.0.1:5000`, 5000, 4290)).toBe(456);
  expect(parseClientPid("p456\nn127.0.0.1:5001->127.0.0.1:4290", 5000, 4290)).toBeUndefined();
  expect(httpMetadata({ http_transport: false, http_client_pid: 99 }, undefined, { "trace-id": "header" })).toEqual({
    meta: { http_transport: false, http_client_pid: 99 }, httpTransport: true, httpClientPid: undefined, headers: { "trace-id": "header" },
  });
});

test("singleton daemon returns zero on same port, rejects a different port and releases ownership on shutdown", async () => {
  const root = await mkdtemp("/tmp/cito-daemon-");
  const env = { ...process.env, CITO_STATE_DIR: join(root, "state"), CITO_CODEX_HOME: join(root, "codex"),
    CITO_DAEMON_PORT: String(await freePort()), CITO_UI_HEADLESS: "1", CITO_NATS_URL: "nats://127.0.0.1:1" };
  const child = spawn(process.execPath, [resolve("bin/cito-mcp"), "daemon"], { env, stdio: ["ignore", "ignore", "pipe"] });
  let errors = ""; child.stderr.on("data", chunk => { errors += chunk; });
  const exited = new Promise<number | null>(resolve => child.once("exit", resolve));
  try {
    await waitFor(async () => !!await daemonHealth(loadConfig(env)), 10000);
    expect((await exec(process.execPath, [resolve("bin/cito-mcp"), "daemon"], { env, timeout: 10000 })).stdout).toBe("");
    await expect(exec(process.execPath, [resolve("bin/cito-mcp"), "daemon"], { env: { ...env, CITO_DAEMON_PORT: String(await freePort()) }, timeout: 10000 }))
      .rejects.toMatchObject({ code: 1, stderr: expect.stringContaining("already owns") });
    child.kill("SIGTERM"); expect(await exited).toBe(0);
    expect(await daemonHealth(loadConfig(env))).toBeUndefined();
    expect(errors).not.toContain("uncaught");
  } finally { if (child.exitCode === null) child.kill("SIGKILL"); await exited; await rm(root, { recursive: true, force: true }); }
}, 15000);

test("CLI listen and unlisten support socket-bridge parent discovery, --codex auto/explicit resolution, flags, and error exits", async () => {
  const f = await fixture();
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const state = await LocalState.open(f.stateDir);
  state.claimDaemon(process.pid, http.port);
  const captured: CallerInput[] = [], original = f.service.call.bind(f.service);
  const spy = vi.spyOn(f.service, "call").mockImplementation((name, input, meta) => { captured.push(meta ?? {}); return original(name, input, meta); });
  const env = { ...process.env, ...f.env, CITO_DAEMON_PORT: String(http.port), CITO_UI_HEADLESS: "1" };
  const realRoot = await realpath(f.root);
  const cli = (args: string[], cwd = realRoot) => exec(process.execPath, [resolve("bin/cito-mcp"), ...args], { env, cwd, timeout: 10000 });
  const db = new DatabaseSync(join(f.codexHome, "state_5.sqlite"));
  const codexUuid = "01234567-89ab-4cde-8123-0123456789ab";
  try {
    // 1. Default mode: sends caller_pid: process.ppid (no codexThreadId) and walks parent to Pi socket bridge.
    const defaultListen = await cli(["listen", "--tag", "cli-tag", "Working", "on", "V6"]);
    expect(defaultListen.stdout).toMatch(/^Listening as cli-tag-[a-z0-9]+ \(self\): Working on V6\n$/);
    expect(captured.at(-1)).toMatchObject({ meta: { caller_pid: process.pid } });
    expect((captured.at(-1) as { meta: Record<string, unknown> }).meta).not.toHaveProperty("codexThreadId");

    const teamListen = await cli(["listen", "--team"]);
    expect(teamListen.stdout).toMatch(new RegExp(`^Listening as cli-tag-[a-z0-9]+ \\(workspace\\): ${basename(realRoot)}\n$`));

    const defaultUnlisten = await cli(["unlisten", "--hub", "default"]);
    expect(defaultUnlisten.stdout).toBe("Unlistened (1 removed)\n");
    expect((await cli(["unlisten"])).stdout).toBe("Unlistened (0 removed)\n");

    // 2. Codex mode: --codex auto-resolution, --codex=<id>, and --codex <uuid> (no caller_pid/endpoint_pid/pi_pid).
    await expect(cli(["listen", "--codex"])).rejects.toMatchObject({ code: 1, stderr: expect.stringContaining("Codex thread metadata is required") });
    db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY, title TEXT, cwd TEXT, git_origin_url TEXT, git_branch TEXT, archived INTEGER, name TEXT, updated_at INTEGER)");
    db.prepare("INSERT INTO threads VALUES (?, 'Codex title', ?, 'https://github.com/acme/service.git', 'main', 0, 'Codex session', 100)").run(codexUuid, realRoot);

    const codexAuto = await cli(["listen", "--codex", "--scope=workspace", "--hub=default", "--tag=cdx", "Codex", "review"]);
    expect(codexAuto.stdout).toMatch(/^Listening as cdx-[a-z0-9]+ \(workspace\): Codex review\n$/);
    expect(captured.at(-1)).toMatchObject({ meta: { codexThreadId: codexUuid, cwd: realRoot } });
    expect((captured.at(-1) as { meta: Record<string, unknown> }).meta).not.toHaveProperty("caller_pid");
    expect((captured.at(-1) as { meta: Record<string, unknown> }).meta).not.toHaveProperty("endpoint_pid");
    expect((captured.at(-1) as { meta: Record<string, unknown> }).meta).not.toHaveProperty("pi_pid");

    // Archive thread and verify `cito unlisten --codex` resolves archived thread via allowArchived: true.
    db.exec("UPDATE threads SET archived = 1");
    expect((await cli(["unlisten", "--codex"])).stdout).toBe("Unlistened (1 removed)\n");

    // Explicit --codex=<id> and --codex <uuid>.
    db.exec("UPDATE threads SET archived = 0");
    expect((await cli(["listen", `--codex=${codexUuid}`, "--scope", "self", "Explicit", "equals"])).stdout)
      .toMatch(/^Listening as cdx-[a-z0-9]+ \(self\): Explicit equals\n$/);
    expect((await cli(["unlisten", "--codex", codexUuid])).stdout).toBe("Unlistened (1 removed)\n");

    // 3. Error exits on invalid flags / arguments.
    for (const badArgs of [
      ["listen", "--scope", "self", "--team"],
      ["listen", "--scope", "invalid"],
      ["listen", "--unknown-flag"],
      ["unlisten", "unexpected-positional"],
    ]) {
      await expect(cli(badArgs)).rejects.toMatchObject({ code: 1, stderr: expect.stringMatching(/\S/) });
    }
  } finally {
    db.close(); spy.mockRestore(); state.releaseDaemon(process.pid); state.close(); await http.close(); await f.close();
  }
}, 20000);
