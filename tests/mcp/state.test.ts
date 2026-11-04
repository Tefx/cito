import { afterEach, expect, test } from "vitest";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { LocalState, type Signal } from "../../src/mcp/state.js";

const cleanup: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close(); });
async function setup() {
  const dir = await mkdtemp("/tmp/cito-state-"); cleanup.push(() => rm(dir, { recursive: true, force: true }));
  const state = await LocalState.open(dir); cleanup.push(async () => state.close());
  for (const hub of ["personal", "rda"]) state.run(`INSERT INTO local_listeners (hub,listener_id,principal_id,owner_user,host_id,host_name,
    harness,session_id,endpoint_pid,cwd,title,about,scope,status,binding_generation,owner_token)
    VALUES (?, 'c_abcdefgh2345','alice','alice','host','host','cursor','s',123,'/tmp','Title','About','self','ACTIVE',2,'owner')`, hub);
  return state;
}
function wire(id = "s1", patch: Partial<Signal> = {}): Signal {
  return { signal_id: id, to: "c_abcdefgh2345", target_generation: 2, sender_endpoint_key: "sender",
    from: { principal_id: "alice", user: "alice", host_id: "other", host_name: "other", harness: null, listener_id: null, title: "Sender", about: null },
    message: "Hello", in_reply_to: null, ...patch };
}
function admit(state: LocalState, signal: Signal, seq: number, hub = "personal") {
  return state.admit("c_abcdefgh2345", "owner", signal, seq, new Date().toISOString(), Date.now() + 60_000, hub);
}
test("admission is hub scoped, deduplicates and advances the cursor without any quota", async () => {
  const state = await setup();
  const first = admit(state, wire(), 1).row!;
  expect(first).toMatchObject({ hub: "personal", state: "RECEIVED", held_reason: null });
  expect(admit(state, wire(), 3)).toEqual({ row: first, duplicate: true });
  expect(admit(state, wire(), 1, "rda").duplicate).toBe(false);
  for (let i = 4; i <= 12; i++) expect(admit(state, wire("s" + i), i).row!.state).toBe("RECEIVED");
  expect(state.listener("c_abcdefgh2345", "personal")!.persisted_seq).toBe(12);
  expect(state.listener("c_abcdefgh2345", "rda")!.persisted_seq).toBe(1);
});
test("private cross-principal messages drop silently, workspace messages wake, stale generations stay passive", async () => {
  const state = await setup(), peer = wire().from; peer.principal_id = "bob";
  expect(admit(state, wire("denied", { from: peer }), 1).row).toBeUndefined();
  expect(state.listener("c_abcdefgh2345", "personal")!.persisted_seq).toBe(1);
  state.run("UPDATE local_listeners SET scope = 'workspace' WHERE hub = 'personal'");
  expect(admit(state, wire("peer", { from: peer }), 2).row).toMatchObject({ state: "RECEIVED", held_reason: null });
  expect(admit(state, wire("stale", { target_generation: 1 }), 3).row).toMatchObject({ state: "STORED_PASSIVE", held_reason: "stale_generation" });
});
test("stale owners cannot admit or advance cursor and transaction errors roll back", async () => {
  const state = await setup();
  expect(() => state.admit("c_abcdefgh2345", "wrong", wire(), 1, new Date().toISOString(), Date.now(), "personal")).toThrow("Listener owner changed");
  expect(() => state.transaction(() => { state.run("UPDATE local_listeners SET persisted_seq = 10"); throw new Error("rollback"); })).toThrow("rollback");
  expect(state.listener("c_abcdefgh2345", "personal")!.persisted_seq).toBe(0);
});
test("schema reset atomically discards pre-V4 state, removes pairwise table and preserves V4 rows on reopening", async () => {
  const dir = await mkdtemp("/tmp/cito-upgrade-");
  try {
    const db = new DatabaseSync(join(dir, "state.sqlite"));
    db.exec("CREATE TABLE local_listeners (legacy TEXT); CREATE TABLE local_pairwise_wakes (legacy TEXT); PRAGMA user_version = 3;"); db.close();
    const state = await LocalState.open(dir);
    expect(state.db.prepare("PRAGMA user_version").get()!.user_version).toBe(4);
    expect(state.db.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").all().map(row => row.name))
      .toEqual(["local_listeners", "local_sent_idempotency", "local_signals"]);
    expect(state.db.prepare("PRAGMA table_info(local_listeners)").all().map(row => row.name)).toContain("endpoint_pid");
    state.run("INSERT INTO local_sent_idempotency (hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES ('rda','d','h','s','t','queued','now',1)"); state.close();
    const reopened = await LocalState.open(dir);
    try { expect(reopened.db.prepare("SELECT hub, dedup_id FROM local_sent_idempotency").all()).toEqual([{ hub: "rda", dedup_id: "d" }]); }
    finally { reopened.close(); }
  } finally { await rm(dir, { recursive: true, force: true }); }
});
