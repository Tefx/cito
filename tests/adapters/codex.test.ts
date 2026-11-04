import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { codexThreadId, readCodexThreadOptional, resolveCaller, extractCallerPidRoots, CodexDesktopAdapter,
  queueCodexWake, triggerCodexWake } from "../../src/adapters/index.js";
import { loadConfig } from "../../src/shared/index.js";
import type { ListenerRow } from "../../src/shared/state-types.js";
import { isolatedState, unixServer } from "./support.js";

const threadId = "01234567-89ab-4cde-8123-0123456789ab";
describe("non-intrusive Codex metadata and wake effects", () => {
  let state: Awaited<ReturnType<typeof isolatedState>>;
  beforeEach(async () => { state = await isolatedState(); });
  afterEach(async () => { await state.cleanup(); });
  function database(archived = 0, name = "Session name") {
    const db = new DatabaseSync(join(state.codexHome, "state_5.sqlite"));
    db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY, title TEXT, cwd TEXT, git_origin_url TEXT, git_branch TEXT, archived INTEGER, name TEXT)");
    db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?)").run(threadId, "Thread title", state.root, "https://github.com/Owner/Repo.git", "topic", archived, name);
    return db;
  }
  it.each([
    { "x-codex-turn-metadata": JSON.stringify({ thread_id: threadId }) },
    { "x-codex-turn-metadata": { session_id: threadId } }, { "openai/threadId": threadId }, { codexThreadId: threadId },
  ])("extracts explicit thread claim %j", meta => { expect(codexThreadId(meta)).toBe(threadId); });
  it("validates all explicit claims before state access", async () => {
    const config = loadConfig(state.env);
    for (const meta of [ { codexThreadId: "a", "openai/threadId": "b" }, { codexThreadId: "a", pi_pid: 123 }, { endpoint_pid: 1, caller_pid: 2 } ]) {
      await expect(resolveCaller(meta, config)).rejects.toMatchObject({ code: "INVALID_INPUT" });
    }
    for (const pid of [0, -1, 0.5, Number.MAX_SAFE_INTEGER + 1, "12"]) {
      await expect(resolveCaller({ pi_pid: pid }, config)).rejects.toMatchObject({ code: "PI_BRIDGE_UNAVAILABLE" });
    }
    expect(extractCallerPidRoots({ pi_pid: 12, nested: { parent_pid: 13 }, http_client_pid: 14 })).toEqual([12]);
    expect(extractCallerPidRoots({ nested: { parent_pid: 13 }, http_client_pid: 14 })).toEqual([13]);
    expect(extractCallerPidRoots({ http_transport: true, http_client_pid: 14 })).toEqual([14]);
    expect(extractCallerPidRoots({ http_transport: true })).toEqual([]);
    expect(extractCallerPidRoots()).toEqual([process.ppid]);
  });
  it("reads exact SQLite row, keeps background threads open, and never contacts Desktop IPC", async () => {
    const db = database(); let connections = 0;
    const server = await unixServer(join(state.codexHome, "ipc/ipc.sock"), socket => { connections++; socket.destroy(); });
    try {
      expect(readCodexThreadOptional(threadId, state)).toMatchObject({ status: "found", title: "Session name", repo: "Owner/Repo", branch: "topic" });
      expect(await resolveCaller({ codexThreadId: threadId, http_transport: true, http_client_pid: process.pid }, loadConfig(state.env)))
        .toMatchObject({ harness: "codex", session_id: threadId, cwd: state.root });
      expect(connections).toBe(0);
      expect(readCodexThreadOptional("' OR 1=1 --", state)).toEqual({ status: "unavailable" });
      db.exec("UPDATE threads SET archived = 1");
      expect(readCodexThreadOptional(threadId, state)).toEqual({ status: "archived" });
      await expect(resolveCaller({ codexThreadId: threadId }, loadConfig(state.env))).rejects.toMatchObject({ code: "SESSION_NOT_OPEN" });
      expect(await resolveCaller({ codexThreadId: threadId, cwd: state.root }, loadConfig(state.env), { requireOpen: false })).toMatchObject({ cwd: state.root });
    } finally { db.close(); await server.stop(); }
  });
  it("missing or incompatible databases stay optional and are never created", async () => {
    expect(readCodexThreadOptional(threadId, state)).toEqual({ status: "unavailable" });
    await expect(readFile(join(state.codexHome, "state_5.sqlite"))).rejects.toMatchObject({ code: "ENOENT" });
    const db = new DatabaseSync(join(state.codexHome, "state_5.sqlite"));
    try { expect(readCodexThreadOptional(threadId, state)).toEqual({ status: "unavailable" }); } finally { db.close(); }
  });
  it("busy SQLite metadata is unavailable and falls through to the explicit cwd", async () => {
    const db = database();
    try {
      db.exec("PRAGMA journal_mode = DELETE; BEGIN EXCLUSIVE");
      expect(readCodexThreadOptional(threadId, state)).toEqual({ status: "unavailable" });
      expect((await resolveCaller({ codexThreadId: threadId, cwd: state.root }, loadConfig(state.env))).cwd).toBe(state.root);
    } finally { db.exec("ROLLBACK"); db.close(); }
  });
  it("fallback order is explicit cwd, OS cwd, cached cwd, process cwd", async () => {
    const config = loadConfig(state.env), meta = { codexThreadId: threadId, http_transport: true };
    const cached = { harness: "codex", session_id: threadId, cwd: state.root, title: "cached", repo: null, branch: null };
    expect((await resolveCaller({ ...meta, cwd: state.codexHome, http_client_pid: process.pid }, config, { lookupCachedSession: () => cached })).cwd).toBe(state.codexHome);
    expect((await resolveCaller({ ...meta, http_client_pid: process.pid }, config, { lookupCachedSession: () => cached })).cwd).toBe(process.cwd());
    expect((await resolveCaller(meta, config, { lookupCachedSession: () => cached })).cwd).toBe(state.root);
    expect((await resolveCaller(meta, config)).cwd).toBe(process.cwd());
  });
  it("probe archives definitively; unavailable metadata uses desktop process liveness", async () => {
    const config = loadConfig(state.env), adapter = new CodexDesktopAdapter(), signal = new AbortController().signal;
    const row = { session_id: threadId } as ListenerRow;
    config.codexDesktopRunning = async () => true;
    expect(await adapter.probe(row, config, signal)).toEqual({ status: "alive" });
    config.codexDesktopRunning = async () => false;
    expect(await adapter.probe(row, config, signal)).toEqual({ status: "closed_definitive" });
    const db = database(1); config.codexDesktopRunning = async () => { throw new Error("must not probe process"); };
    try { expect(await adapter.probe(row, config, signal)).toEqual({ status: "closed_definitive" }); } finally { db.close(); }
  });
  it("executes queue --resume and silent deeplink with literal argv and CODEX_HOME", async () => {
    const log = join(state.root, "argv"), bin = join(state.root, "cli.cjs");
    await writeFile(bin, `#!${process.execPath}\nrequire('node:fs').writeFileSync(${JSON.stringify(log)}, JSON.stringify([process.argv.slice(2), process.env.CODEX_HOME]));`, { mode: 0o700 });
    const card = 'quoted "card"; $(never)\nline two';
    await queueCodexWake(threadId, card, { codexBin: bin, env: { ...process.env, CODEX_HOME: state.codexHome } });
    expect(JSON.parse(await readFile(log, "utf8"))).toEqual([["queue", "--resume", "--thread", threadId, "--message", card], state.codexHome]);
    await triggerCodexWake(threadId, { openBin: bin });
    expect(JSON.parse(await readFile(log, "utf8"))[0]).toEqual(["-g", "codex://threads/wake/" + threadId]);
    await expect(queueCodexWake(threadId, card, { codexBin: "/missing" })).rejects.toMatchObject({ code: "ENOENT" });
    const controller = new AbortController(); controller.abort();
    await expect(queueCodexWake(threadId, card, { codexBin: bin, signal: controller.signal })).rejects.toMatchObject({ name: "AbortError" });
  });
});
