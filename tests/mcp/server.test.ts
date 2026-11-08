import { afterEach, expect, test } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { rm, stat } from "node:fs/promises";
import { join } from "node:path";
import { nativeFixture, object, type NativeHarness } from "../support/native-harness.js";

const fixtures: NativeHarness[] = [];
afterEach(async () => { for (const fixture of fixtures.splice(0).reverse()) await fixture.close(); });
async function fixture() { const f = await nativeFixture(); fixtures.push(f); return f; }
function textPayload(value: unknown): Record<string, unknown> {
  const result = object(value, "Tool result");
  if (!Array.isArray(result.content)) throw new Error("Missing tool content");
  const first = object(result.content[0]);
  if (typeof first.text !== "string") throw new Error("Missing tool text");
  return object(JSON.parse(first.text));
}
function strings(env: NodeJS.ProcessEnv): Record<string, string> {
  return Object.fromEntries(Object.entries(env).filter((entry): entry is [string, string] => entry[1] !== undefined));
}

test("SDK lists exactly five strict tools and routes _meta through real native MCP requests", async () => {
  const f = await fixture(), pi = await f.piBridge();
  await f.daemon();
  const client = new Client({ name: "test", version: "1" });
  try {
    await client.connect(new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${f.daemonPort}/mcp`)));
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
    const listened = await client.callTool({ name: "cito_listen", arguments: { about: "MCP receiver", tag: "MCP receiver" }, _meta: pi.meta });
    expect(listened.isError).toBe(false);
    const enrolled = textPayload(listened);
    expect(enrolled).toMatchObject({ harness: "pi", status: "listening", listener_id: expect.stringMatching(/^alice-[a-f0-9]{16}$/) });
    const sent = await client.callTool({ name: "cito_send", arguments: { to: enrolled.listener_id, message: "over MCP" }, _meta: {expected_harness:"human",expected_session_id:"default"} });
    expect(sent.isError).toBe(false);
    expect(textPayload(sent)).toMatchObject({ to: enrolled.listener_id, status: "wake_accepted" });
    for (const name of ["cito_listen", "cito_unlisten", "cito_find", "cito_send", "cito_inbox"]) {
      const invalid = await client.callTool({ name, arguments: { extra: true } });
      expect(invalid.isError).toBe(true);
      expect(textPayload(invalid)).toMatchObject({ error: { code: "INVALID_INPUT", retryable: false } });
    }
    const unsupported = await client.callTool({ name: "cito_find", arguments: { harness: "Bad Harness" } });
    expect(textPayload(unsupported)).toMatchObject({ error: { code: "UNSUPPORTED_HARNESS" } });
  } finally { await client.close(); }
});

test("installed native stdio entrypoint initializes private local state without requiring NATS for inbox", async () => {
  const f = await fixture();
  // Own the daemon separately so SDK transport teardown cannot orphan an automatically spawned child.
  await f.daemon();
  const transport = new StdioClientTransport({ command: await f.binary("bin/cito-mcp"), args: [], env: strings({
    ...f.env, CITO_USER: "test", CITO_NATS_URL: "nats://127.0.0.1:1" }), stderr: "pipe" });
  const client = new Client({ name: "stdio-test", version: "1" });
  try {
    await client.connect(transport);
    expect((await client.listTools()).tools).toHaveLength(5);
    const result = await client.callTool({ name: "cito_inbox", arguments: {} });
    expect(result.isError).toBe(true);
    expect(textPayload(result)).toMatchObject({ error: { code: "BINDING_REQUIRED" } });
    expect((await stat(join(f.stateDir, "state.sqlite"))).mode & 0o777).toBe(0o600);
  } finally { await client.close(); }
});

test.each([
  { CITO_NATS_URL: "ftp://wrong" }, { CITO_AUTH_MODE: "broken" }, { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: "" },
  { CITO_USER: "bad user" },
])("native stdio invalid configuration fails fast with exit 1 and stderr: %j", async invalid => {
  const f = await fixture();
  await rm(f.stateDir, { recursive: true });
  const result = await f.command([], { executable: "bin/cito-mcp", env: invalid, network:"deny" });
  expect(result).toMatchObject({ code: 1, stdout: "", stderr: expect.stringContaining("cito-mcp:") });
  await expect(stat(f.stateDir)).rejects.toMatchObject({ code: "ENOENT" });
});

test("native stdio EOF shuts down without leaking a process", async () => {
  const f = await fixture();
  const result = await f.command([], { executable: "bin/cito-mcp" });
  expect(result.code).toBe(0);
});
