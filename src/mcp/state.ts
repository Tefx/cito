import { DatabaseSync, type SQLInputValue } from "node:sqlite";
import { chmod, mkdir, open } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { CitoError, brokerAgeMs, RETENTION_MS, harnessIdSchema, canonicalListenerIdSchema,
  unscopedListenerIdSchema, sanitizeSignalMessage, type ChannelAttachment } from "../shared/index.js";
import type { ListenerRow, SignalRow } from "../shared/state-types.js";
export type { ListenerRow, SignalRow, SentRow } from "../shared/state-types.js";
export type { ChannelAttachment } from "../shared/index.js";

export const channelAttachmentSchema = z.object({
  channel_id: z.string().min(1),
  kind: z.string().min(1),
  host_id: z.string().min(1),
  host_name: z.string(),
  delivery_mode: z.enum(["notify_fanout", "wake_exclusive"]),
  priority: z.number().int(),
  state: z.enum(["ACTIVE", "UNLISTENED"]),
  updated_at_ms: z.number(),
});

export const registrationSchema = z.object({
  listener_id: canonicalListenerIdSchema, public_id: unscopedListenerIdSchema.optional(), tag: z.string().nullable().optional(),
  principal_id: z.string().min(1), owner_user: z.string().min(1),
  host_id: z.string().min(1), host_name: z.string(), harness: harnessIdSchema, session_id: z.string().min(1),
  cwd: z.string(), repo: z.string().nullable(), branch: z.string().nullable(), title: z.string(), about: z.string(),
  scope: z.enum(["self", "workspace"]), state: z.enum(["ACTIVE", "UNLISTENED"]),
  binding_generation: z.number().int().positive(), owner_token: z.string().uuid(),
  channels: z.record(z.string(), channelAttachmentSchema).optional(),
});
export type Registration = z.infer<typeof registrationSchema>;
export const signalSchema = z.object({
  signal_id: z.string().min(1).max(128).regex(/^[^:]+$/), to: unscopedListenerIdSchema, target_generation: z.number().int().positive(),
  sender_endpoint_key: z.string().min(1),
  from: z.object({ principal_id: z.string().min(1), user: z.string(), host_id: z.string(), host_name: z.string(),
    harness: harnessIdSchema.nullable(), listener_id: unscopedListenerIdSchema.nullable().default(null),
    title: z.string().default(""), about: z.string().nullable().default(null) }),
  message: z.string().transform(sanitizeSignalMessage), in_reply_to: z.string().nullable(),
  acked_at_ms: z.number().nullable().optional(),
});
export type Signal = z.infer<typeof signalSchema>;

/** All transactions are synchronous: no awaited effects can interleave their writes. */
export class LocalState {
  readonly liveReceivers = new Map<string, { owner: string; abort: AbortController }>();
  readonly db: DatabaseSync;
  private constructor(db: DatabaseSync) {
    this.db = db;
  }
  static async open(stateDir: string): Promise<LocalState> {
    await mkdir(join(stateDir, "sockets"), { recursive: true, mode: 0o700 });
    await chmod(stateDir, 0o700);
    const path = join(stateDir, "state.sqlite");
    const file = await open(path, "a", 0o600); await file.close(); await chmod(path, 0o600);
    const db = new DatabaseSync(path), state = new LocalState(db);
    try {
      db.exec("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;");
      state.transaction(() => {
        if (db.prepare("PRAGMA user_version").get()!.user_version === 4) return;
        db.exec(`DROP TABLE IF EXISTS local_listeners; DROP TABLE IF EXISTS local_signals;
          DROP TABLE IF EXISTS local_pairwise_wakes; DROP TABLE IF EXISTS local_sent_idempotency;
          CREATE TABLE local_listeners (
            hub TEXT NOT NULL DEFAULT 'default', listener_id TEXT NOT NULL, public_id TEXT NOT NULL DEFAULT '', tag TEXT,
            principal_id TEXT NOT NULL, owner_user TEXT NOT NULL, host_id TEXT NOT NULL, host_name TEXT NOT NULL,
            harness TEXT NOT NULL CHECK(length(harness) > 0), session_id TEXT NOT NULL, endpoint_pid INTEGER,
            cwd TEXT NOT NULL, repo TEXT, branch TEXT, title TEXT NOT NULL, about TEXT NOT NULL,
            scope TEXT NOT NULL CHECK(scope IN ('self','workspace')),
            status TEXT NOT NULL CHECK(status IN ('ACTIVE','REBINDING','UNLISTENED','FENCED_INACTIVE')),
            binding_generation INTEGER NOT NULL DEFAULT 0, owner_token TEXT NOT NULL DEFAULT '',
            persisted_seq INTEGER NOT NULL DEFAULT 0, consecutive_probe_failures INTEGER NOT NULL DEFAULT 0,
            replay_gap_detected INTEGER NOT NULL DEFAULT 0, replay_gap_range TEXT, dispatcher_token TEXT,
            lease_until_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (hub, listener_id));
          CREATE INDEX listeners_by_session ON local_listeners(harness, session_id);
          CREATE TABLE local_signals (
            hub TEXT NOT NULL DEFAULT 'default', signal_id TEXT NOT NULL, listener_id TEXT NOT NULL, seq INTEGER NOT NULL,
            binding_generation INTEGER NOT NULL, from_user TEXT NOT NULL, from_host_name TEXT NOT NULL, from_harness TEXT,
            from_listener_id TEXT, from_title TEXT NOT NULL, from_about TEXT, message TEXT NOT NULL, held_reason TEXT,
            in_reply_to TEXT, created_at TEXT NOT NULL, expires_at_ms INTEGER NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE','EXPIRED_UNSENT')),
            batch_id TEXT, batch_expires_at_ms INTEGER, dispatcher_token TEXT, queue_written INTEGER NOT NULL DEFAULT 0,
            acknowledged_at_ms INTEGER, batch_card TEXT, PRIMARY KEY (hub, signal_id));
          CREATE INDEX signals_listener ON local_signals(hub, listener_id, state, seq);
          CREATE INDEX signals_batch ON local_signals(hub, batch_id);
          CREATE TABLE local_sent_idempotency (
            hub TEXT NOT NULL DEFAULT 'default', dedup_id TEXT NOT NULL, request_hash TEXT NOT NULL,
            signal_id TEXT NOT NULL, seq INTEGER NOT NULL DEFAULT 0, to_listener_id TEXT NOT NULL, status TEXT NOT NULL,
            held_reason TEXT, expires_at_iso TEXT NOT NULL, expires_at_ms INTEGER NOT NULL, PRIMARY KEY (hub, dedup_id));
          PRAGMA user_version = 4;`);
      });
      db.exec("CREATE TABLE IF NOT EXISTS daemon_owner (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), pid INTEGER NOT NULL, port INTEGER NOT NULL)");
      return state;
    } catch (error) { db.close(); throw error; }
  }
  daemonOwner(): { pid: number; port: number } | undefined { return this.db.prepare("SELECT pid, port FROM daemon_owner WHERE singleton = 1").get() as { pid: number; port: number } | undefined; }
  claimDaemon(pid: number, port: number, expected?: { pid: number; port: number }): boolean {
    return this.transaction(() => {
      const current = this.daemonOwner();
      if (current?.pid !== expected?.pid || current?.port !== expected?.port) return false;
      this.run("INSERT OR REPLACE INTO daemon_owner VALUES (1, ?, ?)", pid, port); return true;
    });
  }
  releaseDaemon(pid: number): void { this.run("DELETE FROM daemon_owner WHERE singleton = 1 AND pid = ?", pid); }
  run(sql: string, ...values: SQLInputValue[]) { return this.db.prepare(sql).run(...values); }
  listener(id: string, hub = "default"): ListenerRow | undefined {
    return this.db.prepare("SELECT * FROM local_listeners WHERE hub = ? AND listener_id = ?").get(hub, id) as unknown as ListenerRow | undefined;
  }
  signals(sql: string, ...values: SQLInputValue[]): SignalRow[] { return this.db.prepare(sql).all(...values) as unknown as SignalRow[]; }
  transaction<T>(action: () => T): T {
    this.db.exec("BEGIN IMMEDIATE");
    try { const result = action(); this.db.exec("COMMIT"); return result; }
    catch (error) { this.db.exec("ROLLBACK"); throw error; }
  }
  recordOfflineGap(id: string, brokerNow: string, created: Date, hub = "default"): void {
    if (brokerAgeMs(brokerNow, created) > RETENTION_MS) this.run(`UPDATE local_listeners SET replay_gap_detected = 1,
      replay_gap_range = 'offline_exceeded_30m' WHERE hub = ? AND listener_id = ?`, hub, id);
  }
  admit(
    id: string,
    owner: string,
    signal: Signal,
    seq: number,
    created: string,
    expires: number,
    hub = "default",
    ackedAtMs: number | null = signal.acked_at_ms ?? null,
  ): { row?: SignalRow; duplicate: boolean } {
    return this.transaction(() => {
      const target = this.listener(id, hub);
      if (!target || target.status !== "ACTIVE" || target.owner_token !== owner) throw new CitoError("STALE_GENERATION", "Listener owner changed");
      const advance = () => this.run("UPDATE local_listeners SET persisted_seq = MAX(persisted_seq, ?) WHERE hub = ? AND listener_id = ?", seq, hub, id);
      const previous = this.signals("SELECT * FROM local_signals WHERE hub = ? AND signal_id = ?", hub, signal.signal_id)[0];
      if (previous) { advance(); return { row: previous, duplicate: true }; }
      if (signal.from.principal_id !== target.principal_id && target.scope !== "workspace") { advance(); return { duplicate: false }; }
      const reason = signal.target_generation < target.binding_generation ? "stale_generation" : null;
      const alreadyAcked = ackedAtMs !== null && Number.isFinite(ackedAtMs);
      const initialState = (reason || alreadyAcked) ? "STORED_PASSIVE" : "RECEIVED";
      this.run(`INSERT INTO local_signals (hub, signal_id, listener_id, seq, binding_generation,
        from_user, from_host_name, from_harness, from_listener_id, from_title, from_about, message,
        held_reason, in_reply_to, created_at, expires_at_ms, state, acknowledged_at_ms) VALUES (${Array(18).fill("?").join(",")})`,
        hub, signal.signal_id, id, seq, target.binding_generation, signal.from.user, signal.from.host_name, signal.from.harness,
        signal.from.listener_id, signal.from.title, signal.from.about, signal.message, reason, signal.in_reply_to,
        created, expires, initialState, alreadyAcked ? ackedAtMs : null);
      advance();
      return { row: this.signals("SELECT * FROM local_signals WHERE hub = ? AND signal_id = ?", hub, signal.signal_id)[0]!, duplicate: false };
    });
  }
  close(): void { this.db.close(); }
}
export function registration(row: ListenerRow, channels?: Record<string, ChannelAttachment>): Registration {
  return { listener_id: row.listener_id, public_id: row.public_id, tag: row.tag,
    principal_id: row.principal_id, owner_user: row.owner_user, host_id: row.host_id, host_name: row.host_name,
    harness: row.harness, session_id: row.session_id, cwd: row.cwd, repo: row.repo, branch: row.branch,
    title: row.title, about: row.about, scope: row.scope, state: row.status === "ACTIVE" ? "ACTIVE" : "UNLISTENED",
    binding_generation: row.binding_generation, owner_token: row.owner_token,
    ...(channels ? { channels } : {}) };
}
