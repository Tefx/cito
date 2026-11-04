import { DatabaseSync, type SQLInputValue } from "node:sqlite";
import { chmod, mkdir, open } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { CitoError, brokerAgeMs, harnessSchema, listenerIdSchema, sanitizeMessage } from "../shared/index.js";

export const registrationSchema = z.object({
  listener_id: listenerIdSchema, principal_id: z.string().min(1), owner_user: z.string().min(1),
  host_id: z.string().min(1), host_name: z.string(), harness: harnessSchema, session_id: z.string().min(1),
  cwd: z.string(), repo: z.string().nullable(), branch: z.string().nullable(), title: z.string(), about: z.string(),
  scope: z.enum(["self", "workspace"]), allow_peer_wake: z.boolean(),
  state: z.enum(["ACTIVE", "UNLISTENED"]), binding_generation: z.number().int().positive(),
  owner_token: z.string().uuid(), unattended_wakes_remaining: z.number().int().min(0).max(3),
});
export type Registration = z.infer<typeof registrationSchema>;
export const signalSchema = z.object({
  signal_id: z.string().min(1).max(128), to: listenerIdSchema, target_generation: z.number().int().positive(),
  sender_endpoint_key: z.string().min(1),
  from: z.object({ principal_id: z.string().min(1), user: z.string(), host_id: z.string(), host_name: z.string(),
    harness: harnessSchema.nullable(), session_id: z.string().nullable().default(null), listener_id: listenerIdSchema.nullable().default(null),
    title: z.string().default(""), about: z.string().nullable().default(null) }),
  message: z.string().transform(sanitizeMessage), requested_wake: z.boolean(), in_reply_to: z.string().nullable(),
});
export type Signal = z.infer<typeof signalSchema>;
export interface ListenerRow {
  listener_id: string; principal_id: string; owner_user: string; host_id: string; host_name: string;
  harness: "codex" | "pi"; session_id: string; pi_pid: number | null; cwd: string; repo: string | null;
  branch: string | null; title: string; about: string; scope: "self" | "workspace"; allow_peer_wake: number;
  status: "ACTIVE" | "REBINDING" | "UNLISTENED" | "FENCED_INACTIVE";
  binding_generation: number; owner_token: string; kv_revision: number; persisted_seq: number;
  unattended_wake_count: number; last_bridge_epoch: string | null; retired_bridge_epochs_json: string;
  last_bridge_count: number; human_event_seq: number; last_consumed_human_seq: number;
  consecutive_probe_failures: number; replay_gap_detected: number; replay_gap_range: string | null;
  dispatcher_token: string | null; lease_until_ms: number; updated_at_ms: number;
}
export interface SignalRow {
  signal_id: string; listener_id: string; seq: number; binding_generation: number; target_generation: number;
  from_principal_id: string; from_user: string; from_host_id: string; from_host_name: string;
  from_harness: "pi" | "codex" | null; from_session_id: string | null; from_listener_id: string | null;
  from_title: string; from_about: string | null; sender_endpoint_key: string; message: string;
  requested_wake: number; effective_wake: number; held_reason: string | null; in_reply_to: string | null;
  created_at: string; expires_at_ms: number;
  state: "RECEIVED" | "DISPATCHING" | "DISPATCHED" | "STORED_PASSIVE" | "EXPIRED_UNSENT";
  batch_id: string | null; batch_expires_at_ms: number | null; dispatcher_token: string | null;
  queue_written: number; acknowledged_at_ms: number | null; batch_card: string | null;
}
export interface SentRow {
  dedup_id: string; request_hash: string; signal_id: string; seq: number; to_listener_id: string;
  requested_wake: number; effective_wake: number; status: string; held_reason: string | null;
  expires_at_iso: string; expires_at_ms: number;
}

/** One synchronous transaction owner: no await is permitted inside a transaction. */
export class LocalState {
  private constructor(readonly db: DatabaseSync) {}
  static async open(stateDir: string): Promise<LocalState> {
    await mkdir(join(stateDir, "sockets"), { recursive: true, mode: 0o700 });
    await chmod(stateDir, 0o700);
    const path = join(stateDir, "state.sqlite");
    const file = await open(path, "a", 0o600); await file.close(); await chmod(path, 0o600);
    const db = new DatabaseSync(path);
    try {
      db.exec(`PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;
        CREATE TABLE IF NOT EXISTS local_listeners (
          listener_id TEXT PRIMARY KEY, principal_id TEXT NOT NULL, owner_user TEXT NOT NULL,
          host_id TEXT NOT NULL, host_name TEXT NOT NULL, harness TEXT NOT NULL CHECK(harness IN ('pi','codex')),
          session_id TEXT NOT NULL, pi_pid INTEGER, cwd TEXT NOT NULL, repo TEXT, branch TEXT,
          title TEXT NOT NULL, about TEXT NOT NULL, scope TEXT NOT NULL CHECK(scope IN ('self','workspace')),
          allow_peer_wake INTEGER NOT NULL DEFAULT 0,
          status TEXT NOT NULL CHECK(status IN ('ACTIVE','REBINDING','UNLISTENED','FENCED_INACTIVE')),
          binding_generation INTEGER NOT NULL DEFAULT 0, owner_token TEXT NOT NULL DEFAULT '',
          kv_revision INTEGER NOT NULL DEFAULT 0, persisted_seq INTEGER NOT NULL DEFAULT 0,
          unattended_wake_count INTEGER NOT NULL DEFAULT 0, last_bridge_epoch TEXT,
          retired_bridge_epochs_json TEXT NOT NULL DEFAULT '[]', last_bridge_count INTEGER NOT NULL DEFAULT 0,
          human_event_seq INTEGER NOT NULL DEFAULT 0, last_consumed_human_seq INTEGER NOT NULL DEFAULT 0,
          consecutive_probe_failures INTEGER NOT NULL DEFAULT 0, replay_gap_detected INTEGER NOT NULL DEFAULT 0,
          replay_gap_range TEXT, dispatcher_token TEXT, lease_until_ms INTEGER NOT NULL DEFAULT 0,
          updated_at_ms INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS local_signals (
          signal_id TEXT PRIMARY KEY, listener_id TEXT NOT NULL, seq INTEGER NOT NULL,
          binding_generation INTEGER NOT NULL, target_generation INTEGER NOT NULL,
          from_principal_id TEXT NOT NULL, from_user TEXT NOT NULL, from_host_id TEXT NOT NULL,
          from_host_name TEXT NOT NULL, from_harness TEXT, from_session_id TEXT, from_listener_id TEXT,
          from_title TEXT NOT NULL, from_about TEXT, sender_endpoint_key TEXT NOT NULL, message TEXT NOT NULL,
          requested_wake INTEGER NOT NULL, effective_wake INTEGER NOT NULL, held_reason TEXT, in_reply_to TEXT,
          created_at TEXT NOT NULL, expires_at_ms INTEGER NOT NULL,
          state TEXT NOT NULL CHECK(state IN ('RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE','EXPIRED_UNSENT')),
          batch_id TEXT, batch_expires_at_ms INTEGER, dispatcher_token TEXT,
          queue_written INTEGER NOT NULL DEFAULT 0, acknowledged_at_ms INTEGER, batch_card TEXT);
        CREATE INDEX IF NOT EXISTS signals_listener ON local_signals(listener_id, state, seq);
        CREATE TABLE IF NOT EXISTS local_pairwise_wakes (
          listener_id TEXT NOT NULL, sender_endpoint_key TEXT NOT NULL, admitted_at_ms INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS pairwise_window ON local_pairwise_wakes(listener_id, sender_endpoint_key, admitted_at_ms);
        CREATE TABLE IF NOT EXISTS local_sent_idempotency (
          dedup_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, signal_id TEXT NOT NULL, seq INTEGER NOT NULL DEFAULT 0,
          to_listener_id TEXT NOT NULL, requested_wake INTEGER NOT NULL, effective_wake INTEGER NOT NULL DEFAULT 0,
          status TEXT NOT NULL, held_reason TEXT, expires_at_iso TEXT NOT NULL, expires_at_ms INTEGER NOT NULL);
      `);
      db.exec("BEGIN IMMEDIATE");
      try {
        if (!db.prepare("PRAGMA table_info(local_signals)").all().some(column => column.name === "batch_card")) {
          db.exec("ALTER TABLE local_signals ADD COLUMN batch_card TEXT");
        }
        db.exec("COMMIT");
      } catch (error) { db.exec("ROLLBACK"); throw error; }
      return new LocalState(db);
    } catch (error) { db.close(); throw error; }
  }
  run(sql: string, ...values: SQLInputValue[]) { return this.db.prepare(sql).run(...values); }
  listener(id: string): ListenerRow | undefined {
    return this.db.prepare("SELECT * FROM local_listeners WHERE listener_id = ?").get(id) as unknown as ListenerRow | undefined;
  }
  signals(sql: string, ...values: SQLInputValue[]): SignalRow[] {
    return this.db.prepare(sql).all(...values) as unknown as SignalRow[];
  }
  transaction<T>(action: () => T): T {
    this.db.exec("BEGIN IMMEDIATE");
    try { const result = action(); this.db.exec("COMMIT"); return result; }
    catch (error) { this.db.exec("ROLLBACK"); throw error; }
  }
  recordOfflineGap(id: string, brokerNow: string, created: Date): void {
    if (brokerAgeMs(brokerNow, created) > 1_800_000) this.run(`UPDATE local_listeners SET replay_gap_detected = 1,
      replay_gap_range = 'offline_exceeded_30m' WHERE listener_id = ?`, id);
  }
  consumeHuman(id: string): void {
    const row = this.listener(id)!;
    if (row.human_event_seq <= row.last_consumed_human_seq) return;
    this.run(`UPDATE local_listeners SET unattended_wake_count = 0, last_consumed_human_seq = human_event_seq
      WHERE listener_id = ?`, id);
    this.run("DELETE FROM local_pairwise_wakes WHERE listener_id = ?", id);
  }
  observePi(id: string, epoch: string, count: number): void {
    const row = this.listener(id)!;
    const retired: string[] = JSON.parse(row.retired_bridge_epochs_json);
    if (retired.includes(epoch)) return;
    let delta = 0;
    if (row.last_bridge_epoch === epoch) delta = Math.max(0, count - row.last_bridge_count);
    else {
      if (row.last_bridge_epoch) retired.push(row.last_bridge_epoch);
      delta = count;
    }
    this.run(`UPDATE local_listeners SET last_bridge_epoch = ?, retired_bridge_epochs_json = ?,
      last_bridge_count = ?, human_event_seq = human_event_seq + ? WHERE listener_id = ?`,
      epoch, JSON.stringify(retired.slice(-32)), row.last_bridge_epoch === epoch ? Math.max(count, row.last_bridge_count) : count, delta, id);
  }
  admit(id: string, owner: string, signal: Signal, seq: number, created: string, expires: number,
    pairwiseNow: number): { row?: SignalRow; duplicate: boolean } {
    return this.transaction(() => {
      const target = this.listener(id);
      if (!target || target.status !== "ACTIVE" || target.owner_token !== owner) throw new CitoError("STALE_GENERATION", "Listener owner changed");
      const advance = () => this.run("UPDATE local_listeners SET persisted_seq = MAX(persisted_seq, ?) WHERE listener_id = ?", seq, id);
      const previous = this.signals("SELECT * FROM local_signals WHERE signal_id = ?", signal.signal_id)[0];
      if (previous) { advance(); return { row: previous, duplicate: true }; }
      if (signal.from.principal_id !== target.principal_id && target.scope !== "workspace") { advance(); return { duplicate: false }; }
      this.consumeHuman(id);
      const row = this.listener(id)!;
      this.run("DELETE FROM local_pairwise_wakes WHERE listener_id = ? AND (admitted_at_ms <= ? OR admitted_at_ms > ?)", id, pairwiseNow - 120_000, pairwiseNow);
      const pairs = Number(this.db.prepare("SELECT COUNT(*) AS n FROM local_pairwise_wakes WHERE listener_id = ? AND sender_endpoint_key = ?")
        .get(id, signal.sender_endpoint_key)!.n);
      const reason = signal.target_generation < row.binding_generation ? "stale_generation"
        : !signal.requested_wake ? "passive_requested"
        : signal.from.principal_id !== row.principal_id && !row.allow_peer_wake ? "peer_wake_disabled"
        : row.unattended_wake_count >= 3 ? "wake_quota_exhausted"
        : pairs >= 2 ? "pairwise_rate_limited" : null;
      const effective = reason === null;
      if (effective) {
        this.run("UPDATE local_listeners SET unattended_wake_count = unattended_wake_count + 1 WHERE listener_id = ?", id);
        this.run("INSERT INTO local_pairwise_wakes VALUES (?, ?, ?)", id, signal.sender_endpoint_key, pairwiseNow);
      }
      this.run(`INSERT INTO local_signals (signal_id, listener_id, seq, binding_generation, target_generation,
        from_principal_id, from_user, from_host_id, from_host_name, from_harness, from_session_id, from_listener_id,
        from_title, from_about, sender_endpoint_key, message, requested_wake, effective_wake, held_reason, in_reply_to,
        created_at, expires_at_ms, state) VALUES (${Array(23).fill("?").join(",")})`,
        signal.signal_id, id, seq, row.binding_generation, signal.target_generation,
        signal.from.principal_id, signal.from.user, signal.from.host_id, signal.from.host_name, signal.from.harness,
        signal.from.session_id, signal.from.listener_id, signal.from.title, signal.from.about,
        signal.sender_endpoint_key, signal.message, +signal.requested_wake, +effective, reason, signal.in_reply_to,
        created, expires, effective ? "RECEIVED" : "STORED_PASSIVE");
      advance();
      return { row: this.signals("SELECT * FROM local_signals WHERE signal_id = ?", signal.signal_id)[0]!, duplicate: false };
    });
  }
  close(): void { this.db.close(); }
}
export function registration(row: ListenerRow): Registration {
  return { listener_id: row.listener_id, principal_id: row.principal_id, owner_user: row.owner_user,
    host_id: row.host_id, host_name: row.host_name, harness: row.harness, session_id: row.session_id,
    cwd: row.cwd, repo: row.repo, branch: row.branch, title: row.title, about: row.about,
    scope: row.scope, allow_peer_wake: !!row.allow_peer_wake, state: row.status === "ACTIVE" ? "ACTIVE" : "UNLISTENED",
    binding_generation: row.binding_generation, owner_token: row.owner_token,
    unattended_wakes_remaining: Math.max(0, 3 - row.unattended_wake_count) };
}
