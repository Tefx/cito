import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { assertCodexThreadOpen, codexThreadId, queueCodexWake, readCodexThread, resolveCodexCaller,
  triggerCodexWake } from "../../src/adapters/index.js";
import { isolatedState, unixServer } from "./support.js";

const threadId = "01234567-89ab-4cde-8123-0123456789ab";
function frame(value: object): Buffer {
  const body = Buffer.from(JSON.stringify(value)); const header = Buffer.alloc(4); header.writeUInt32LE(body.length);
  return Buffer.concat([header, body]);
}
describe("Codex metadata, SQLite, framed IPC, and separate wake effects", () => {
  let state: Awaited<ReturnType<typeof isolatedState>>;
  const servers: Awaited<ReturnType<typeof unixServer>>[] = [];
  beforeEach(async () => {
    state = await isolatedState(); vi.stubEnv("CITO_STATE_DIR", state.stateDir); vi.stubEnv("CITO_CODEX_HOME", state.codexHome);
  });
  afterEach(async () => { await Promise.all(servers.splice(0).map(s => s.stop())); vi.unstubAllEnvs(); await state.cleanup(); });
  function database(archived = 0, title: string | null = "Thread title", name?: string | null) {
    const db = new DatabaseSync(join(state.codexHome, "state_5.sqlite"));
    try {
      db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY, title TEXT, cwd TEXT, git_origin_url TEXT, git_branch TEXT, archived INTEGER)");
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?)").run(threadId, title, state.root, "https://github.com/Owner/Repo.git", "topic", archived);
      if (name !== undefined) {
        db.exec("ALTER TABLE threads ADD COLUMN name TEXT");
        db.prepare("UPDATE threads SET name = ? WHERE id = ?").run(name, threadId);
      }
    } finally { db.close(); }
  }
  async function ipc(result: object = { ownerClientId: "owner" }, resultType = "success", extra: object = {}) {
    const requests: Record<string, unknown>[] = [];
    servers.push(await unixServer(join(state.codexHome, "ipc", "ipc.sock"), socket => {
      let buffer = Buffer.alloc(0);
      socket.on("data", chunk => {
        buffer = Buffer.concat([buffer, chunk]);
        while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32LE()) {
          const size = buffer.readUInt32LE();
          const request = JSON.parse(buffer.subarray(4, 4 + size).toString()) as Record<string, unknown>;
          buffer = buffer.subarray(4 + size); requests.push(request);
          const response = frame({ type: "response", requestId: request.requestId,
            resultType: request.method === "initialize" ? "success" : resultType,
            result: request.method === "initialize" ? { clientId: "cito-client" } : result, ...extra });
          // Fragment both the 4-byte header and body, with an unrelated event coalesced in front.
          socket.write(frame({ type: "event", method: "thread-updated" }));
          socket.write(response.subarray(0, 2));
          socket.write(response.subarray(2, 7));
          socket.write(response.subarray(7));
        }
      });
    }));
    return requests;
  }
  it.each([
    [{ "x-codex-turn-metadata": JSON.stringify({ thread_id: threadId }) }],
    [{ "x-codex-turn-metadata": { thread_id: threadId } }],
    [{ "x-codex-turn-metadata": { session_id: threadId } }],
    [{ "openai/threadId": threadId }], [{ codexThreadId: threadId }],
    [{ "x-codex-turn-metadata": "invalid JSON", "openai/threadId": threadId }],
  ])("extracts explicit thread metadata %j", meta => { expect(codexThreadId(meta)).toBe(threadId); });
  it("uses metadata precedence and fails without a thread binding", () => {
    expect(codexThreadId({ "x-codex-turn-metadata": { thread_id: "first", session_id: "second" },
      "openai/threadId": "third", codexThreadId: "fourth" })).toBe("first");
    for (const meta of [{}, { "x-codex-turn-metadata": "null" }, { codexThreadId: 123 }, { "openai/threadId": "" }]) {
      expect(() => codexThreadId(meta)).toThrow(expect.objectContaining({ code: "BINDING_REQUIRED" }));
    }
  });
  it("reads only the exact SQLite thread, normalizes repo, and resolves through a real IPC owner handshake", async () => {
    database(); const requests = await ipc();
    expect(await resolveCodexCaller({ codexThreadId: threadId }, state)).toEqual({ harness: "codex", session_id: threadId,
      cwd: state.root, title: "Thread title", repo: "Owner/Repo", branch: "topic" });
    expect(requests).toHaveLength(2);
    expect(requests[0]).toEqual({ type: "request", version: 1, requestId: expect.stringMatching(/^[0-9a-f-]{36}$/),
      method: "initialize", params: { clientType: "cli" } });
    expect(requests[1]).toEqual({ type: "request", version: 1, requestId: expect.stringMatching(/^[0-9a-f-]{36}$/),
      sourceClientId: "cito-client", method: "thread-owner-discovery",
      params: { hostId: "local", conversationId: threadId, threadId } });
    expect(requests[1]!.requestId).not.toBe(requests[0]!.requestId);
    expect(() => readCodexThread("unknown", state)).toThrow(expect.objectContaining({ code: "SESSION_NOT_OPEN" }));
    // SQL parameters cannot accidentally select all rows.
    expect(() => readCodexThread("' OR 1=1 --", state)).toThrow(expect.objectContaining({ code: "SESSION_NOT_OPEN" }));
  });
  it("rejects archived threads before any IPC and falls back to cwd basename when title is empty", async () => {
    database(1);
    await expect(resolveCodexCaller({ codexThreadId: threadId }, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
    const db = new DatabaseSync(join(state.codexHome, "state_5.sqlite"));
    try { db.exec("UPDATE threads SET archived = 0, title = '', git_origin_url = NULL, git_branch = NULL"); } finally { db.close(); }
    expect(readCodexThread(threadId, state)).toMatchObject({ title: state.root.split("/").at(-1), repo: null, branch: null });
  });
  it.each([
    { name: "  Session name  ", title: "Thread title", expected: "Session name" },
    { name: "", title: "  Thread title  ", expected: "Thread title" },
    { name: " \t ", title: "  Thread title  ", expected: "Thread title" },
    { name: null, title: "Thread title", expected: "Thread title" },
    { name: "  ", title: " \t ", expected: null },
    { name: null, title: null, expected: null },
  ])("prefers trimmed name, then title, then cwd basename: %j", ({ name, title, expected }) => {
    database(0, title, name);
    expect(readCodexThread(threadId, state).title).toBe(expected ?? state.root.split("/").at(-1));
  });
  it("allows explicit archived-thread reads for inbox/unlisten without a live IPC owner", async () => {
    database(1, "Archived title", "Archived name");
    expect(() => readCodexThread(threadId, state)).toThrow(expect.objectContaining({ code: "SESSION_NOT_OPEN" }));
    expect(() => readCodexThread(threadId, state, { allowArchived: false })).toThrow(expect.objectContaining({ code: "SESSION_NOT_OPEN" }));
    expect(readCodexThread(threadId, state, { allowArchived: true })).toMatchObject({ session_id: threadId, title: "Archived name" });
    expect(() => readCodexThread("missing", state, { allowArchived: true })).toThrow(expect.objectContaining({ code: "SESSION_NOT_OPEN" }));
    // Registration still rejects archived threads; the option belongs only to explicit reads.
    await expect(resolveCodexCaller({ codexThreadId: threadId }, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
  });
  it("never creates a missing state database", async () => {
    expect(() => readCodexThread(threadId, state)).toThrow(expect.objectContaining({ code: "SESSION_NOT_OPEN" }));
    await expect(readFile(join(state.codexHome, "state_5.sqlite"))).rejects.toMatchObject({ code: "ENOENT" });
  });
  it.each([{ ownerClientId: "owner" }, { status: "alive" }, {}])("accepts successful discovery without requiring ownerClientId: %j", async result => {
    await ipc(result);
    await expect(assertCodexThreadOpen(threadId, state)).resolves.toBeUndefined();
  });
  it.each([
    [{ status: "no-client-found", ownerClientId: "stale" }, "success"],
    [{ ownerClientId: null }, "success"], [{ ownerClientId: "" }, "success"],
    [{ ownerClientId: "owner" }, "error"], [{ status: "alive" }, "pending"],
  ])("rejects owner discovery response %j %s", async (result, resultType) => {
    await ipc(result, resultType);
    await expect(assertCodexThreadOpen(threadId, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
  });
  it("rejects error no-client-found, unreachable sockets, invalid client IDs, and malformed frames", async () => {
    await expect(assertCodexThreadOpen(threadId, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
    let server = await unixServer(join(state.codexHome, "ipc", "ipc.sock"), socket => {
      socket.on("data", chunk => {
        const request = JSON.parse(chunk.subarray(4).toString()) as Record<string, unknown>;
        socket.end(frame({ type: "response", requestId: request.requestId, resultType: "error", error: "no-client-found" }));
      });
    });
    try { await expect(assertCodexThreadOpen(threadId, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" }); }
    finally { await server.stop(); }
    await ipc({}, "success", { result: { clientId: "" } });
    await expect(assertCodexThreadOpen(threadId, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
    await servers.pop()!.stop();
    server = await unixServer(join(state.codexHome, "ipc", "ipc.sock"), socket => socket.end(Buffer.from([255, 255, 255, 127])));
    try { await expect(assertCodexThreadOpen(threadId, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" }); }
    finally { await server.stop(); }
  });
  it("bounds total IPC time even when unrelated frames keep arriving", async () => {
    servers.push(await unixServer(join(state.codexHome, "ipc", "ipc.sock"), socket => {
      const timer = setInterval(() => socket.write(frame({ type: "event" })), 100);
      socket.on("close", () => clearInterval(timer));
    }));
    await expect(assertCodexThreadOpen(threadId, state)).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
  });
  async function executable(name: string, source: string): Promise<string> {
    const path = join(state.root, name);
    await writeFile(path, `#!${process.execPath}\n${source}\n`, { mode: 0o700 });
    return path;
  }
  it("executes queue and silent deeplink as separate steps with literal argv and no shell interpolation", async () => {
    const output = join(state.root, "args.json");
    const bin = await executable("mock-cli.cjs", `require('node:fs').writeFileSync(${JSON.stringify(output)}, JSON.stringify(process.argv.slice(2)))`);
    const card = 'Card with "quotes"; $(never-execute)\nsecond line';
    await queueCodexWake(threadId, card, { codexBin: bin });
    expect(JSON.parse(await readFile(output, "utf8"))).toEqual(["queue", "--thread", threadId, "--message", card]);
    await triggerCodexWake(threadId, { openBin: bin });
    expect(JSON.parse(await readFile(output, "utf8"))).toEqual(["-g", "codex://threads/wake/" + threadId]);
    vi.stubEnv("CITO_CODEX_BIN", bin);
    await queueCodexWake(threadId, "configured executable");
    expect(JSON.parse(await readFile(output, "utf8"))).toEqual(["queue", "--thread", threadId, "--message", "configured executable"]);
  });
  it("propagates spawn and nonzero errors rather than silently running the wake step", async () => {
    await expect(queueCodexWake(threadId, "card", { codexBin: join(state.root, "missing") })).rejects.toMatchObject({ code: "ENOENT" });
    const fail = await executable("fail.cjs", "process.exit(7)");
    await expect(queueCodexWake(threadId, "card", { codexBin: fail })).rejects.toMatchObject({ code: 7 });
    await expect(triggerCodexWake(threadId, { openBin: fail })).rejects.toMatchObject({ code: 7 });
  });
  it.each(["queue", "open"] as const)("aborts an in-flight %s executable and rejects pre-aborted effects", async step => {
    const output = join(state.root, "started");
    const bin = await executable("wait.cjs", `require('node:fs').writeFileSync(${JSON.stringify(output)}, String(process.pid)); setInterval(() => {}, 1000)`);
    const controller = new AbortController();
    const options = { codexBin: bin, openBin: bin, signal: controller.signal };
    const run = () => step === "queue" ? queueCodexWake(threadId, "card", options) : triggerCodexWake(threadId, options);
    const pending = run();
    const rejected = expect(pending).rejects.toMatchObject({ name: "AbortError" });
    try { await expect.poll(async () => readFile(output, "utf8")).toMatch(/^\d+$/); }
    finally { controller.abort(); }
    await rejected;
    await expect(run()).rejects.toMatchObject({ name: "AbortError" });
  });
});
