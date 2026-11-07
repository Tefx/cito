import { expect, test } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createMcpServer, daemonHealth } from "../../src/mcp/server.js";
import { loadConfig } from "../../src/shared/index.js";
import { fixture, waitFor, freePort } from "./support.js";

const exec = promisify(execFile);
async function stopDaemon(env: NodeJS.ProcessEnv) {
  const health = await daemonHealth(loadConfig(env)); if (!health) return;
  process.kill(health.pid, "SIGTERM");
  await waitFor(() => { try { process.kill(health.pid, 0); return false; } catch { return true; } }, 10000);
}
test("SDK lists exactly five strict tools and routes _meta through real MCP requests", async () => {
  const f = await fixture(), server = createMcpServer(f.service), client = new Client({ name: "test", version: "1" });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  try {
    await server.connect(serverTransport); await client.connect(clientTransport);
    const listed = await client.listTools();
    expect(listed.tools.map(tool => tool.name)).toEqual(["cito_listen", "cito_unlisten", "cito_find", "cito_send", "cito_inbox"]);
    expect(listed.tools.every(tool => tool.inputSchema.additionalProperties === false)).toBe(true);
    expect(listed.tools.find(tool => tool.name === "cito_listen")!.inputSchema.properties?.tag)
      .toMatchObject({ type: "string", maxLength: 64 });
    const sendTool = listed.tools.find(tool => tool.name === "cito_send")!;
    expect(sendTool.description).toContain("lightweight control plane");
    expect(sendTool.description).toContain("65,536");
    const listenTool = listed.tools.find(tool => tool.name === "cito_listen")!;
    expect(listenTool.description).toContain("lightweight control plane");
    const toSchema = sendTool.inputSchema.properties?.to as { pattern: string };
    expect(new RegExp(toSchema.pattern).test("mcp-receiver-pbtjxcxkgw3y")).toBe(true);
    expect(new RegExp(toSchema.pattern).test("c_pbtjxcxkgw3y")).toBe(true);
    const listened = await client.callTool({ name: "cito_listen", arguments: { about: "MCP receiver", tag: "MCP receiver" }, _meta: f.pi.meta });
    expect(listened.isError).toBe(false);
    const enrolled = JSON.parse((listened.content as { text: string }[])[0]!.text);
    expect(enrolled).toMatchObject({ harness: "pi", status: "listening", listener_id: expect.stringMatching(/^mcp-receiver-[a-z2-7]{12}$/) });
    const sent = await client.callTool({ name: "cito_send", arguments: { to: enrolled.listener_id, message: "over MCP" } });
    expect(sent.isError).toBe(false);
    expect(JSON.parse((sent.content as { text: string }[])[0]!.text)).toMatchObject({ to: enrolled.listener_id, status: "wake_accepted" });
    for (const name of ["cito_listen", "cito_unlisten", "cito_find", "cito_send", "cito_inbox"]) {
      const invalid = await client.callTool({ name, arguments: { extra: true } });
      expect(invalid.isError).toBe(true);
      expect(JSON.parse((invalid.content as { text: string }[])[0]!.text)).toMatchObject({ error: { code: "INVALID_INPUT", retryable: false } });
    }
    const unsupported = await client.callTool({ name: "cito_find", arguments: { harness: "Bad Harness" } });
    expect(JSON.parse((unsupported.content as { text: string }[])[0]!.text)).toMatchObject({ error: { code: "UNSUPPORTED_HARNESS" } });
  } finally { await client.close(); await server.close(); await f.close(); }
});

test("installed stdio entrypoint initializes private local state without requiring NATS for inbox", async () => {
  const root = await mkdtemp("/tmp/cito-stdio-");
  const port = await freePort();
  const env = { PATH: process.env.PATH!, CITO_STATE_DIR: join(root, "state"), CITO_CODEX_HOME: join(root, "codex"), CITO_DAEMON_PORT: String(port), CITO_UI_HEADLESS: "1" };
  const transport = new StdioClientTransport({ command: process.execPath, args: [resolve("bin/cito-mcp")], env: {
    ...env, CITO_USER: "test",
    CITO_NATS_URL: "nats://127.0.0.1:1" }, stderr: "pipe" });
  const client = new Client({ name: "stdio-test", version: "1" });
  try {
    await client.connect(transport);
    expect((await client.listTools()).tools).toHaveLength(5);
    const result = await client.callTool({ name: "cito_inbox", arguments: {} });
    expect(result.isError).toBe(true);
    expect(JSON.parse((result.content as { text: string }[])[0]!.text)).toMatchObject({ error: { code: "BINDING_REQUIRED" } });
    expect((await stat(join(root, "state/state.sqlite"))).mode & 0o777).toBe(0o600);
  } finally { await client.close(); await stopDaemon(env); await rm(root, { recursive: true, force: true }); }
});

test.each([
  { CITO_NATS_URL: "ftp://wrong" }, { CITO_AUTH_MODE: "broken" }, { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: "" },
  { CITO_USER: "bad user" },
])("stdio invalid configuration fails fast with exit 1 and stderr: %j", async invalid => {
  const root = await mkdtemp("/tmp/cito-stdio-invalid-");
  try {
    await expect(exec(process.execPath, [resolve("bin/cito-mcp")], { env: {
      PATH: process.env.PATH, CITO_STATE_DIR: join(root, "state"), CITO_CODEX_HOME: join(root, "codex"), ...invalid }, timeout: 5000 }))
      .rejects.toMatchObject({ code: 1, stdout: "", stderr: expect.stringContaining("cito-mcp:") });
    await expect(stat(join(root, "state"))).rejects.toMatchObject({ code: "ENOENT" });
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("stdio EOF shuts down without leaking a process", async () => {
  const root = await mkdtemp("/tmp/cito-eof-");
  const env = { PATH: process.env.PATH, CITO_STATE_DIR: join(root, "state"), CITO_CODEX_HOME: join(root, "codex"), CITO_DAEMON_PORT: String(await freePort()), CITO_UI_HEADLESS: "1" };
  const child = spawn(process.execPath, [resolve("bin/cito-mcp")], { env, stdio: ["pipe", "pipe", "pipe"] });
  const stopped = new Promise<number | null>(resolve => child.once("exit", resolve));
  try { child.stdin.end(); expect(await stopped).toBe(0); }
  finally { if (child.exitCode === null) child.kill("SIGKILL"); await stopped; await stopDaemon(env); await rm(root, { recursive: true, force: true }); }
});
