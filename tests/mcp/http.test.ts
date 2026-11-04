import { expect, test, vi } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { createServer } from "node:net";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { startHttpServer, parseClientPid, httpMetadata, daemonHealth } from "../../src/mcp/server.js";
import { loadConfig } from "../../src/shared/index.js";
import { fixture, waitFor } from "./support.js";

const exec = promisify(execFile);
async function freePort(): Promise<number> {
  const server = createServer(); await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as { port: number }).port; await new Promise<void>(resolve => server.close(() => resolve())); return port;
}
test("HTTP binds loopback, enforces Origin, attributes real client PID, rejects spoofing and merges Codex headers", async () => {
  const f = await fixture();
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const base = `http://127.0.0.1:${http.port}`;
  const client = new Client({ name: "http-test", version: "1" });
  const captured: Record<string, unknown>[] = [], original = f.service.call.bind(f.service);
  const spy = vi.spyOn(f.service, "call").mockImplementation((name, input, meta) => { captured.push(meta ?? {}); return original(name, input, meta); });
  try {
    expect(http.http.address()).toMatchObject({ address: "127.0.0.1" });
    expect(await (await fetch(base + "/health")).json()).toEqual({ ok: true, pid: process.pid, stateDir: f.stateDir });
    for (const origin of ["https://example.com", "null", "not a URL"]) expect((await fetch(base + "/mcp", { headers: { origin } })).status).toBe(403);
    for (const origin of ["http://localhost:9999", "http://127.0.0.1"]) expect((await fetch(base + "/health", { headers: { origin } })).status).toBe(200);
    await client.connect(new StreamableHTTPClientTransport(new URL(base + "/mcp")));
    const result = await client.callTool({ name: "cito_listen", arguments: { about: "OS attributed caller" }, _meta: { http_transport: false, http_client_pid: 999999 } });
    expect(JSON.parse((result.content as { text: string }[])[0]!.text)).toMatchObject({ status: "listening", harness: "pi" });
    expect(captured.at(-1)).toMatchObject({ http_transport: true, http_client_pid: process.pid });
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
  expect(httpMetadata({ http_transport: false, http_client_pid: 99 }, undefined)).toEqual({ http_transport: true, http_client_pid: undefined });
  expect(() => httpMetadata({ "x-codex-turn-metadata": { session_id: "a" } }, 1, '{"thread_id":"b"}')).toThrow(expect.objectContaining({ code: "INVALID_INPUT" }));
  expect(httpMetadata({ "x-codex-turn-metadata": { thread_id: "a", turn_id: "body" } }, 1, '{"trace_id":"header"}'))
    .toMatchObject({ "x-codex-turn-metadata": { thread_id: "a", turn_id: "body", trace_id: "header" } });
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
