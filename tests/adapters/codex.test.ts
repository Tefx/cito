import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { bridgeSocketPath, codexThreadId, discoverAllAvailableSessions, readCodexThreadOptional, resolveCodexThreadId, resolveCaller, extractCallerPidRoots, CodexDesktopAdapter,
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
    { "x-codex-turn-metadata": { session_id: threadId } }, { "openai/threadId": " " + threadId + " " }, { codexThreadId: threadId },
    { expected_harness: "codex", expected_session_id: threadId },
  ])("extracts explicit thread claim %j", meta => { expect(codexThreadId(meta)).toBe(threadId); });
  it("guards expected_session_id when an explicit socket PID is present", () => {
    expect(() => codexThreadId({ expected_harness: "codex", expected_session_id: threadId, endpoint_pid: process.pid }))
      .toThrow(expect.objectContaining({ code: "BINDING_REQUIRED" }));
  });
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
  it("adapter merges HTTP/body thread claims and rejects conflicting aliases and explicit bridge PIDs", async () => {
    const config = loadConfig(state.env), adapter = new CodexDesktopAdapter();
    const detect = (meta: Record<string, unknown>, header?: string | string[]) =>
      adapter.detectExplicitCaller({ meta: { cwd: state.root, ...meta }, headers: { "x-codex-turn-metadata": header } }, config);
    expect(await detect({}, JSON.stringify({ thread_id: threadId }))).toMatchObject({ harness: "codex", session_id: threadId, cwd: state.root });
    expect(await detect({ "x-codex-turn-metadata": { thread_id: threadId, turn_id: "body" } }, '{"trace_id":"header"}'))
      .toMatchObject({ session_id: threadId });
    expect(await detect({ "x-codex-turn-metadata": JSON.stringify({ session_id: threadId }) }, JSON.stringify({ thread_id: threadId })))
      .toMatchObject({ session_id: threadId });
    expect(await detect({}, "invalid JSON")).toBeNull();
    for (const meta of [{ codexThreadId: "other" }, { "openai/threadId": "other" },
      { "x-codex-turn-metadata": { session_id: "other" } }, { endpoint_pid: 1 }, { caller_pid: 1 }]) {
      await expect(detect(meta, JSON.stringify({ thread_id: threadId }))).rejects.toMatchObject({ code: "INVALID_INPUT" });
    }
    await expect(detect({}, [JSON.stringify({ thread_id: threadId }), '{"session_id":"other"}']))
      .rejects.toMatchObject({ code: "INVALID_INPUT" });
    await expect(detect({ "x-codex-turn-metadata": { thread_id: threadId } }, '{"session_id":"other"}'))
      .rejects.toMatchObject({ code: "INVALID_INPUT" });
    await expect(resolveCaller({ meta: { endpoint_pid: 1 }, headers: { "x-codex-turn-metadata": JSON.stringify({ thread_id: threadId }) } }, config))
      .rejects.toMatchObject({ code: "INVALID_INPUT" });
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
  it("resolves Codex thread ID from state_5.sqlite with updated_at vs rowid ordering, cwd precedence, and allowArchived fallback", () => {
    expect(() => resolveCodexThreadId(state.root, state)).toThrowError(expect.objectContaining({ code: "BINDING_REQUIRED" }));
    const db = new DatabaseSync(join(state.codexHome, "state_5.sqlite"));
    try {
      expect(() => resolveCodexThreadId(state.root, state)).toThrowError(expect.objectContaining({ code: "BINDING_REQUIRED" }));
      db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, archived INTEGER)");
      expect(() => resolveCodexThreadId(state.root, state)).toThrowError(expect.objectContaining({ code: "BINDING_REQUIRED" }));
      db.prepare("INSERT INTO threads VALUES (?, ?, ?)").run("thread-cwd-1", state.root, 0);
      db.prepare("INSERT INTO threads VALUES (?, ?, ?)").run("thread-cwd-2", state.root, 0);
      db.prepare("INSERT INTO threads VALUES (?, ?, ?)").run("thread-other-3", "/other/path", 0);
      // Prefers archived = 0 AND cwd = ?, ordered by rowid DESC when updated_at column is absent.
      expect(resolveCodexThreadId(state.root, state)).toBe("thread-cwd-2");
      // Falls back to archived = 0 when no open thread matches cwd.
      expect(resolveCodexThreadId("/unmatched/cwd", state)).toBe("thread-other-3");
      // When updated_at exists, orders by updated_at DESC, rowid DESC.
      db.exec("DROP TABLE threads; CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, archived INTEGER, updated_at INTEGER)");
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?)").run("older-rowid-newer-ts", state.root, 0, 500);
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?)").run("newer-rowid-older-ts", state.root, 0, 100);
      expect(resolveCodexThreadId(state.root, state)).toBe("older-rowid-newer-ts");
      // Archive all threads: default allowArchived=false throws BINDING_REQUIRED; allowArchived=true resolves archived thread.
      db.exec("UPDATE threads SET archived = 1");
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?)").run("archived-other", "/other/path", 1, 999);
      expect(() => resolveCodexThreadId(state.root, state)).toThrowError(expect.objectContaining({ code: "BINDING_REQUIRED" }));
      expect(resolveCodexThreadId(state.root, state, { allowArchived: true })).toBe("older-rowid-newer-ts");
      expect(resolveCodexThreadId("/unmatched/cwd", state, { allowArchived: true })).toBe("archived-other");
      // Filters out subagent threads via thread_source and source while keeping NULL values via COALESCE
      db.exec("DROP TABLE threads; CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, archived INTEGER, thread_source TEXT, source TEXT, recency_at_ms INTEGER, updated_at INTEGER)");
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?)").run("subagent-source-col", state.root, 0, "subagent", null, 9000, 900);
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?)").run("subagent-json-col", state.root, 0, "user", '{"subagent":"review"}', 8000, 800);
      db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?)").run("null-cols-user-thread", state.root, 0, null, null, 7000, 700);
      expect(resolveCodexThreadId(state.root, state)).toBe("null-cols-user-thread");
    } finally { db.close(); }
  });
  it("discovers open Codex user threads when Desktop is running, excludes subagents/active listeners, and merges with live sockets up to 10 candidates", async () => {
    const config = loadConfig(state.env), adapter = new CodexDesktopAdapter();
    const db = new DatabaseSync(join(state.codexHome, "state_5.sqlite"));
    servers: {
      const sockServer = await unixServer(bridgeSocketPath(state.stateDir, "pi", process.pid), socket => {
        socket.once("data", () => socket.end(JSON.stringify({
          ok: true, endpoint_pid: process.pid, session_id: "pi-live-1", session_name: "Pi Build", cwd: state.root,
        }) + "\n"));
      });
      try {
        db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY, name TEXT, title TEXT, cwd TEXT, git_branch TEXT, archived INTEGER, thread_source TEXT, source TEXT, recency_at_ms INTEGER)");
        db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)").run("sub-1", "Subagent 1", null, state.root, "main", 0, "subagent", null, 99999);
        db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)").run("sub-2", "Subagent 2", null, state.root, "main", 0, "user", '{"subagent":"worker"}', 99998);
        db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)").run("arch-1", "Archived", null, state.root, "main", 1, "user", null, 99997);
        for (let i = 1; i <= 10; i++) {
          db.prepare("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)").run(
            `codex-u-${i}`, i === 10 ? "   " : `Thread ${i}`, i === 10 ? "Fallback Title 10" : null,
            state.root, i % 2 === 0 ? "feat/v7" : null, 0, i === 9 ? null : "user", null, i * 1000,
          );
        }
        // Returns [] when Codex Desktop is not running
        config.codexDesktopRunning = async () => false;
        expect(await adapter.discoverAvailableSessions(config)).toEqual([]);
        // Returns up to 8 most recent non-archived user threads when Desktop is running
        config.codexDesktopRunning = async () => true;
        const codexOnly = await adapter.discoverAvailableSessions(config);
        expect(codexOnly).toHaveLength(8);
        expect(codexOnly[0]).toMatchObject({ harness: "codex", session_id: "codex-u-10", title: "Fallback Title 10", branch: "feat/v7" });
        expect(codexOnly[1]).toMatchObject({ harness: "codex", session_id: "codex-u-9", title: "Thread 9", branch: null });
        expect(codexOnly.some(c => c.session_id.startsWith("sub-") || c.session_id === "arch-1")).toBe(false);
        // Combined discovery puts live socket sessions first, filters active listeners, and caps at 10
        const combined = await discoverAllAvailableSessions(config, [{ harness: "codex", session_id: "codex-u-10" }]);
        expect(combined).toHaveLength(8); // 1 pi + 7 remaining codex (out of 8 returned by codex adapter)
        expect(combined[0]).toMatchObject({ harness: "pi", session_id: "pi-live-1", endpoint_pid: process.pid });
        expect(combined.some(c => c.session_id === "codex-u-10")).toBe(false);
        // Filtering by active endpoint_pid also excludes the socket candidate
        const withoutPi = await discoverAllAvailableSessions(config, [{ harness: "pi", session_id: "other", endpoint_pid: process.pid }]);
        expect(withoutPi.some(c => c.harness === "pi")).toBe(false);
        // Per-harness quota retains up to 8 Pi and 8 Codex without starving either group
        const ppidServer = await unixServer(bridgeSocketPath(state.stateDir, "pi", process.ppid), socket => {
          socket.once("data", () => socket.end(JSON.stringify({
            ok: true, endpoint_pid: process.ppid, session_id: "pi-live-2", session_name: "Pi Newer", cwd: state.root, updated_at_ms: Date.now() + 50_000,
          }) + "\n"));
        });
        try {
          const allOpen = await discoverAllAvailableSessions(config, []);
          expect(allOpen.filter(c => c.harness === "pi").map(c => c.session_id)).toEqual(["pi-live-2", "pi-live-1"]);
          expect(allOpen.filter(c => c.harness === "codex")).toHaveLength(8);
        } finally {
          await ppidServer.stop();
        }
      } finally {
        db.close();
        await sockServer.stop();
      }
    }
  });
});
