import { afterAll, afterEach, beforeAll, describe, expect, test } from "vitest";
import { spawn, execFile, type ChildProcess } from "node:child_process";
import { promisify } from "node:util";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, stat, unlink, utimes, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { DatabaseSync } from "node:sqlite";
import { acquireFlock, ensureBannerCompiled, showBanner, type BannerPayload } from "../../src/banner/index.js";

const exec = promisify(execFile);
const payload: BannerPayload = {
  target_harness: "pi", target_session_id: "session-1", target_title: "Receiver",
  from_user: "alice", from_host: "laptop", from_title: "Sender", message: "first message", effective_wake: true,
};
interface Fixture {
  state: string;
  env: NodeJS.ProcessEnv;
  socket: string;
  binary: string;
  log: string;
}
let root: string;
let cached: string;
const fixtures: Fixture[] = [];
const children: ChildProcess[] = [];

async function waitFor(check: () => Promise<boolean> | boolean, timeout = 3000): Promise<void> {
  const deadline = performance.now() + timeout;
  while (performance.now() < deadline) { if (await check()) return; await delay(10); }
  throw new Error("condition timed out");
}
async function processes(f: Fixture): Promise<number[]> {
  const { stdout } = await exec("/bin/ps", ["-axo", "pid=,command="]);
  return stdout.split("\n").flatMap(line => {
    const match = line.trim().match(/^(\d+)\s+(.+)$/);
    return match?.[2] === f.binary ? [Number(match[1])] : [];
  });
}
async function fixture(dismissMs = 1200): Promise<Fixture> {
  // Longer than sun_path: exercises relative native bind AND concurrent-safe
  // Node connections through the short directory alias.
  const state = join(await mkdtemp(join(root, "state-")), "nested-state-".repeat(10));
  await mkdir(join(state, "bin"), { recursive: true });
  await mkdir(join(state, "sockets"), { recursive: true });
  const binary = join(state, "bin/cito-banner");
  await copyFile(cached, binary);
  await chmod(binary, 0o755);
  const log = join(state, "opened-urls");
  const opener = join(state, "opener");
  await writeFile(opener, '#!/bin/sh\nprintf "%s\\n" "$1" >> "$CITO_BANNER_OPEN_LOG"\n', { mode: 0o700 });
  const f = {
    state, binary, socket: join(state, "sockets/banner.sock"), log,
    env: { ...process.env, CITO_STATE_DIR: state, CITO_BANNER_HEADLESS: "1", CITO_BANNER_DISMISS_MS: String(dismissMs),
      CITO_BANNER_URL_OPENER: opener, CITO_BANNER_OPEN_LOG: log },
  };
  fixtures.push(f);
  return f;
}
async function send(f: Fixture, value: BannerPayload = payload) {
  return showBanner(value, { env: f.env });
}
async function native(f: Fixture) {
  const child = spawn(f.binary, [], { env: f.env, stdio: ["ignore", "ignore", "pipe"] });
  children.push(child);
  let stderr = "";
  child.stderr!.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
  await waitFor(async () => {
    try { await stat(f.socket); return true; } catch { return false; }
  });
  return { child, output: () => stderr };
}
async function click(f: Fixture) {
  const pids = await processes(f);
  expect(pids).toHaveLength(1);
  process.kill(pids[0]!, "SIGUSR1");
  await waitFor(async () => (await processes(f)).length === 0);
}
function database(f: Fixture) {
  const db = new DatabaseSync(join(f.state, "state.sqlite"));
  db.exec("CREATE TABLE local_listeners (harness TEXT, session_id TEXT, human_event_seq INTEGER NOT NULL)");
  const insert = db.prepare("INSERT INTO local_listeners VALUES (?, ?, ?)");
  insert.run("codex", "session-1", 7);
  insert.run("pi", "session-1", 11);
  insert.run("codex", "other", 19);
  return db;
}

beforeAll(async () => {
  if (process.platform !== "darwin") return;
  root = await mkdtemp("/tmp/cito-banner-tests-");
  const lock = await acquireFlock(join(root, "build.lock"), 2500);
  try { cached = await ensureBannerCompiled(join(root, "cache")); }
  finally { await lock?.release(); }
}, 120_000);
afterEach(async () => {
  for (const f of fixtures.splice(0)) {
    for (const pid of await processes(f)) process.kill(pid, "SIGKILL");
    await waitFor(async () => (await processes(f)).length === 0);
  }
  for (const child of children.splice(0)) {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
});
afterAll(async () => { if (root) await rm(root, { recursive: true, force: true }); });

describe.skipIf(process.platform !== "darwin")("native HUD and lock-synchronized sender", () => {
  test("compiles on demand atomically, and recompiles an outdated executable", async () => {
    const f = await fixture(250);
    await unlink(f.binary);
    expect(await send(f)).toEqual({ displayed: true });
    expect((await stat(f.binary)).mode & 0o777).toBe(0o755);
    await waitFor(async () => (await processes(f)).length === 0);
    const old = new Date(1000);
    await utimes(f.binary, old, old);
    const lock = await acquireFlock(join(f.state, "sockets/banner.lock"), 2500);
    try { expect(await ensureBannerCompiled(f.state)).toBe(f.binary); }
    finally { await lock?.release(); }
    expect((await stat(f.binary)).mtimeMs).toBeGreaterThan(1000);
  }, 120_000);

  test("concurrent cold starts and updates share one PID and reset dismissal", async () => {
    const f = await fixture(600);
    expect(await Promise.all([send(f), send(f, { ...payload, message: "second" }), send(f)])).toEqual([
      { displayed: true }, { displayed: true }, { displayed: true },
    ]);
    const original = await processes(f);
    expect(original).toHaveLength(1);
    expect((await stat(f.socket)).mode & 0o777).toBe(0o600);
    for (const name of ["banner.lock", "banner-instance.lock"]) {
      expect((await stat(join(f.state, "sockets", name))).mode & 0o777).toBe(0o600);
    }
    await delay(400);
    expect(await send(f, { ...payload, message: "timer reset" })).toEqual({ displayed: true });
    await delay(300); // Past the initial deadline, before the refreshed deadline.
    expect(await processes(f)).toEqual(original);
    await waitFor(async () => (await processes(f)).length === 0);
    await expect(stat(f.socket)).rejects.toMatchObject({ code: "ENOENT" });
  });

  test("presentation uses the passive prefix and replaces content in place", async () => {
    const f = await fixture();
    const server = await native(f);
    expect(await send(f, { ...payload, effective_wake: false })).toEqual({ displayed: true });
    await waitFor(() => server.output().includes("[仅知会] alice@laptop · Sender"));
    expect(await send(f, { ...payload, message: "replacement" })).toEqual({ displayed: true });
    await waitFor(() => server.output().includes('"message":"replacement"'));
    const lines = server.output().split("\n").filter(line => line.startsWith("cito-banner: display "));
    const presentations = lines.map(line => JSON.parse(line.slice("cito-banner: display ".length)));
    expect(presentations.map(p => p.subtitle)).toEqual(["[仅知会] alice@laptop · Sender", "alice@laptop · Sender"]);
    expect(new Set(presentations.map(p => p.pid)).size).toBe(1);
  });

  test("process-lifetime instance lock excludes a second native process", async () => {
    const f = await fixture(5000);
    const original = await native(f);
    const duplicate = spawn(f.binary, [], { env: f.env, stdio: "ignore" });
    children.push(duplicate);
    const code = await new Promise<number | null>(resolve => duplicate.once("exit", resolve));
    expect(code).toBe(0);
    expect(await processes(f)).toEqual([original.child.pid]);
  });

  test("timer cannot retire while a sender owns banner.lock", async () => {
    const f = await fixture(200);
    await native(f);
    const lock = await acquireFlock(join(f.state, "sockets/banner.lock"), 2500);
    try {
      await delay(350);
      expect(await processes(f)).toHaveLength(1);
      expect((await stat(f.socket)).isSocket()).toBe(true);
    } finally { await lock?.release(); }
    await waitFor(async () => (await processes(f)).length === 0);
  });

  test("Case A retirement handoff re-probes and launches Case B without overlap", async () => {
    const f = await fixture(5000);
    const old = await native(f);
    expect(await send(f)).toEqual({ displayed: true });
    // Expose the unavailable-endpoint window deterministically, then request
    // real click retirement while the sender holds banner.lock. No alternate
    // server, mocked flock, or production test-only retirement path is used.
    await unlink(f.socket);
    const delivery = send(f, { ...payload, message: "handoff notification" });
    await waitFor(async () => {
      const probe = await acquireFlock(join(f.state, "sockets/banner.lock"), 0);
      if (probe) { await probe.release(); return false; }
      return true;
    });
    await delay(150); // Click's 500ms wait straddles the sender's 500ms poll.
    old.child.kill("SIGUSR1");
    let maximum = 0;
    let observing = true;
    const observations = (async () => {
      while (observing) { maximum = Math.max(maximum, (await processes(f)).length); await delay(5); }
    })();
    try { expect(await delivery).toEqual({ displayed: true }); }
    finally { observing = false; await observations; }
    const current = await processes(f);
    expect(current).toHaveLength(1);
    expect(current[0]).not.toBe(old.child.pid);
    expect(old.child.exitCode).toBe(0);
    expect(maximum).toBe(1);
    expect(await send(f, { ...payload, message: "after handoff" })).toEqual({ displayed: true });
  });

  test("busy instance with unreachable socket is skipped without unlink or duplicate spawn", async () => {
    const f = await fixture();
    const lock = await acquireFlock(join(f.state, "sockets/banner-instance.lock"), 0);
    await writeFile(f.socket, "stale endpoint must survive");
    try {
      expect(await send(f)).toMatchObject({ displayed: false, reason: "existing instance remains busy" });
      expect(await readFile(f.socket, "utf8")).toBe("stale endpoint must survive");
      expect(await processes(f)).toEqual([]);
    } finally { await lock?.release(); }
  });

  test("free instance permits stale socket removal and a fresh HUD", async () => {
    const f = await fixture();
    await writeFile(f.socket, "stale");
    expect(await send(f)).toEqual({ displayed: true });
    expect((await stat(f.socket)).isSocket()).toBe(true);
  });

  test("Codex click increments only the target Codex row and opens its deeplink", async () => {
    const f = await fixture(5000);
    const db = database(f);
    try {
      expect(await send(f, { ...payload, target_harness: "codex" })).toEqual({ displayed: true });
      await click(f);
      expect(db.prepare("SELECT human_event_seq FROM local_listeners ORDER BY rowid").all().map(row => row.human_event_seq)).toEqual([8, 11, 19]);
      expect(await readFile(f.log, "utf8")).toBe("codex://threads/session-1\n");
    } finally { db.close(); }
  });

  test("Codex click survives banner.lock contention beyond its initial 500ms wait", async () => {
    const f = await fixture(5000), db = database(f);
    let lock: Awaited<ReturnType<typeof acquireFlock>> = null;
    try {
      expect(await send(f, { ...payload, target_harness: "codex" })).toEqual({ displayed: true });
      lock = await acquireFlock(join(f.state, "sockets/banner.lock"), 2500);
      expect(lock).not.toBeNull();
      const [pid] = await processes(f);
      process.kill(pid!, "SIGUSR1");
      await delay(700);
      expect(db.prepare("SELECT human_event_seq FROM local_listeners WHERE harness = 'codex' AND session_id = 'session-1'").get()?.human_event_seq).toBe(7);
      await expect(stat(f.log)).rejects.toMatchObject({ code: "ENOENT" });
      await lock!.release(); lock = null;
      await waitFor(async () => (await processes(f)).length === 0);
      expect(db.prepare("SELECT human_event_seq FROM local_listeners ORDER BY rowid").all().map(row => row.human_event_seq)).toEqual([8, 11, 19]);
      expect(await readFile(f.log, "utf8")).toBe("codex://threads/session-1\n");
    } finally { await lock?.release(); db.close(); }
  });

  test("Codex binds session identifiers literally and URL-encodes path delimiters", async () => {
    const f = await fixture(5000);
    const db = database(f);
    const session = "thread'quoted/part?";
    db.prepare("INSERT INTO local_listeners VALUES ('codex', ?, 23)").run(session);
    try {
      expect(await send(f, { ...payload, target_harness: "codex", target_session_id: session })).toEqual({ displayed: true });
      await click(f);
      expect(db.prepare("SELECT human_event_seq FROM local_listeners WHERE session_id = ?").get(session)?.human_event_seq).toBe(24);
      expect(db.prepare("SELECT human_event_seq FROM local_listeners WHERE harness = 'codex' AND session_id = 'session-1'").get()?.human_event_seq).toBe(7);
      expect(await readFile(f.log, "utf8")).toBe("codex://threads/thread'quoted%2Fpart%3F\n");
    } finally { db.close(); }
  });

  test("Pi click dismisses without opening a URL or mutating SQLite", async () => {
    const f = await fixture(5000);
    const db = database(f);
    try {
      expect(await send(f)).toEqual({ displayed: true });
      await click(f);
      expect(db.prepare("SELECT human_event_seq FROM local_listeners ORDER BY rowid").all().map(row => row.human_event_seq)).toEqual([7, 11, 19]);
      await expect(stat(f.log)).rejects.toMatchObject({ code: "ENOENT" });
    } finally { db.close(); }
  });

  test("Codex tolerates an uninitialized database and still dismisses/opens", async () => {
    const f = await fixture(5000);
    const db = new DatabaseSync(join(f.state, "state.sqlite"));
    db.close();
    const server = await native(f);
    expect(await send(f, { ...payload, target_harness: "codex" })).toEqual({ displayed: true });
    await click(f);
    expect(server.output()).toContain("no such table: local_listeners");
    expect(await readFile(f.log, "utf8")).toBe("codex://threads/session-1\n");
  });

  test("SQLite busy timeout logs a diagnostic, then opens and dismisses", async () => {
    const f = await fixture(5000);
    const db = database(f);
    const server = await native(f);
    try {
      db.exec("BEGIN EXCLUSIVE");
      expect(await send(f, { ...payload, target_harness: "codex" })).toEqual({ displayed: true });
      await click(f);
      expect(server.output()).toContain("database is locked");
      expect(await readFile(f.log, "utf8")).toBe("codex://threads/session-1\n");
    } finally { db.exec("ROLLBACK"); db.close(); }
  });
});
