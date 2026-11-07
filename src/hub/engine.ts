import { randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import { appendFileSync, chmodSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { evaluateLease, RETENTION_MS } from "../shared/lease.js";
import type { Principal } from "../shared/identity.js";
import type { HubAuthMode } from "../shared/config.js";

export const LISTENERS_BUCKET = "cito_listeners";
export const KV_LISTENERS_STREAM = "KV_cito_listeners";
export const SIGNALS_STREAM = "CITO_SIGNALS";

export interface StreamConfig {
  name: string;
  subjects: string[];
  retention: string;
  max_age: number;
  duplicate_window?: number;
  max_msgs_per_subject?: number;
  storage: string;
  discard: string;
  [key: string]: unknown;
}

export const DEFAULT_SIGNALS_STREAM_CONFIG: StreamConfig = {
  name: SIGNALS_STREAM,
  subjects: ["cito.signal.*"],
  retention: "limits",
  max_age: RETENTION_MS * 1_000_000,
  duplicate_window: RETENTION_MS * 1_000_000,
  storage: "file",
  discard: "old",
};

export const DEFAULT_KV_STREAM_CONFIG: StreamConfig = {
  name: KV_LISTENERS_STREAM,
  subjects: ["$KV.cito_listeners.>"],
  max_msgs_per_subject: 1,
  max_age: 0,
  retention: "limits",
  storage: "file",
  discard: "old",
};

export class HubError extends Error {
  readonly status: number;
  readonly code: string;
  readonly api_error?: { err_code: number };
  constructor(status: number, code: string, message: string, apiErrorCode?: number) {
    super(message);
    this.name = "HubError";
    this.status = status;
    this.code = code;
    if (apiErrorCode !== undefined) {
      this.api_error = { err_code: apiErrorCode };
    }
  }
}

export interface HubAuditEvent {
  ts_ms: number;
  principal_id: string;
  action: "register" | "unlisten" | "send" | "ack";
  target_id: string;
  listener_id?: string;
  signal_id?: string;
  detail?: Record<string, unknown>;
}

export interface HubKvEvent {
  key: string;
  revision: number;
  created_ms: number;
  value: Buffer;
  operation: "PUT" | "DEL" | "PURGE";
  principal_id: string | null;
  scope: string | null;
}

export interface HubSignalEvent {
  seq: number;
  subject: string;
  created_ms: number;
  data: Buffer;
}

export interface HubNcEvent {
  subject: string;
  data: Buffer;
}

export interface HubEngineOptions {
  dataDir?: string;
  authMode?: HubAuthMode;
  auditLogPath?: string;
  onAudit?: (event: HubAuditEvent) => void;
  now?: () => number;
}

interface ConsumerState {
  name: string;
  stream: string;
  filterSubject?: string;
  startSeq: number;
  cursorSeq: number;
  ackedSeqs: Set<number>;
  principalId: string | null;
}

export function isAllowedCitoSubject(subject: string): boolean {
  if (!subject || typeof subject !== "string") return false;
  return /^cito\.(?:signal|ack)\.[A-Za-z0-9_.*-]+$/.test(subject);
}

export function subjectMatchesFilter(subject: string, filter?: string): boolean {
  if (!filter || filter === ">" || filter === "*") return true;
  if (filter === subject) return true;
  if (filter.endsWith(".*")) {
    const prefix = filter.slice(0, -1);
    return subject.startsWith(prefix) && !subject.slice(prefix.length).includes(".");
  }
  if (filter.endsWith(".>")) {
    const prefix = filter.slice(0, -1);
    return subject.startsWith(prefix);
  }
  return false;
}

export class HubEngine {
  readonly db: DatabaseSync;
  readonly events = new EventEmitter();
  readonly authMode: HubAuthMode;
  private readonly now: () => number;
  private readonly auditLogPath?: string;
  private readonly onAudit?: (event: HubAuditEvent) => void;
  private readonly consumers = new Map<string, ConsumerState>();
  private closed = false;

  constructor(options: HubEngineOptions = {}) {
    this.authMode = options.authMode ?? "none";
    this.now = options.now ?? Date.now;
    this.auditLogPath = options.auditLogPath ?? process.env.CITO_HUB_AUDIT_LOG?.trim() ?? undefined;
    this.onAudit = options.onAudit;
    this.events.setMaxListeners(0);

    if (options.dataDir) {
      mkdirSync(options.dataDir, { recursive: true, mode: 0o700 });
      chmodSync(options.dataDir, 0o700);
      const dbPath = join(options.dataDir, "hub.sqlite");
      this.db = new DatabaseSync(dbPath);
      try { chmodSync(dbPath, 0o600); } catch {}
      this.db.exec("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;");
    } else {
      this.db = new DatabaseSync(":memory:");
    }

    this.db.exec(`
      CREATE TABLE IF NOT EXISTS hub_meta (
        key TEXT PRIMARY KEY,
        int_val INTEGER NOT NULL DEFAULT 0,
        json_val TEXT
      );
      INSERT OR IGNORE INTO hub_meta (key, int_val) VALUES ('kv_seq', 0);
      INSERT OR IGNORE INTO hub_meta (key, int_val, json_val) VALUES ('stream:CITO_SIGNALS', 0, '${JSON.stringify(DEFAULT_SIGNALS_STREAM_CONFIG)}');
      INSERT OR IGNORE INTO hub_meta (key, int_val, json_val) VALUES ('stream:KV_cito_listeners', 0, '${JSON.stringify(DEFAULT_KV_STREAM_CONFIG)}');

      CREATE TABLE IF NOT EXISTS hub_kv (
        key TEXT PRIMARY KEY,
        revision INTEGER NOT NULL,
        created_ms INTEGER NOT NULL,
        value BLOB NOT NULL,
        principal_id TEXT,
        scope TEXT
      );

      CREATE TABLE IF NOT EXISTS hub_signals (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        subject TEXT NOT NULL,
        msg_id TEXT UNIQUE,
        created_ms INTEGER NOT NULL,
        data BLOB NOT NULL,
        acked INTEGER NOT NULL DEFAULT 0
      );
      CREATE INDEX IF NOT EXISTS idx_hub_signals_subject_seq ON hub_signals(subject, seq);

      CREATE TABLE IF NOT EXISTS hub_audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts_ms INTEGER NOT NULL,
        principal_id TEXT NOT NULL,
        action TEXT NOT NULL,
        target_id TEXT NOT NULL,
        detail_json TEXT
      );
    `);
  }

  private transaction<T>(fn: () => T): T {
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const result = fn();
      this.db.exec("COMMIT");
      return result;
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }

  private nextKvSeq(): number {
    this.db.prepare("UPDATE hub_meta SET int_val = int_val + 1 WHERE key = 'kv_seq'").run();
    const row = this.db.prepare("SELECT int_val FROM hub_meta WHERE key = 'kv_seq'").get() as { int_val: number | bigint };
    return Number(row.int_val);
  }

  private recordAudit(
    principalId: string,
    action: "register" | "unlisten" | "send" | "ack",
    targetId: string,
    detail?: Record<string, unknown>,
  ): void {
    const tsMs = this.now();
    const detailJson = detail ? JSON.stringify(detail) : null;
    this.db.prepare(
      "INSERT INTO hub_audit_log (ts_ms, principal_id, action, target_id, detail_json) VALUES (?, ?, ?, ?, ?)",
    ).run(tsMs, principalId, action, targetId, detailJson);
    const event: HubAuditEvent = {
      ts_ms: tsMs,
      principal_id: principalId,
      action,
      target_id: targetId,
      ...(typeof detail?.listener_id === "string" ? { listener_id: detail.listener_id } : {}),
      ...(typeof detail?.signal_id === "string" ? { signal_id: detail.signal_id } : {}),
      ...(detail ? { detail } : {}),
    };
    if (this.auditLogPath) {
      try {
        mkdirSync(dirname(this.auditLogPath), { recursive: true });
        appendFileSync(this.auditLogPath, JSON.stringify(event) + "\n", { mode: 0o600 });
      } catch {}
    }
    this.onAudit?.(event);
  }

  private parseKvValue(valueBytes: Uint8Array): {
    principalId: string | null;
    scope: string | null;
    state: string | null;
  } {
    try {
      const parsed: unknown = JSON.parse(Buffer.from(valueBytes).toString("utf8"));
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        const obj = parsed as Record<string, unknown>;
        return {
          principalId: typeof obj.principal_id === "string" ? obj.principal_id : null,
          scope: typeof obj.scope === "string" ? obj.scope : null,
          state: typeof obj.state === "string" ? obj.state : null,
        };
      }
    } catch {}
    return { principalId: null, scope: null, state: null };
  }

  canReadKvRow(row: { principal_id: string | null; scope: string | null }, caller: Principal | null): boolean {
    if (this.authMode !== "oidc") return true;
    if (row.scope === "self" && row.principal_id && row.principal_id !== caller?.principal_id) {
      return false;
    }
    return true;
  }

  canConsumeSignal(targetId: string, caller: Principal | null): boolean {
    if (this.authMode !== "oidc") return true;
    const row = this.db.prepare("SELECT principal_id FROM hub_kv WHERE key = ?").get(targetId) as { principal_id: string | null } | undefined;
    if (row && row.principal_id && row.principal_id !== caller?.principal_id) {
      return false;
    }
    return true;
  }

  streamInfo(stream: string): {
    config: StreamConfig;
    state: { messages: number; last_seq: number; first_seq: number; bytes: number; consumer_count: number };
    ts: string;
  } {
    const meta = this.db.prepare("SELECT json_val FROM hub_meta WHERE key = ?").get("stream:" + stream) as { json_val?: string } | undefined;
    if (!meta?.json_val) {
      throw new HubError(404, "10059", `stream not found: ${stream}`, 10059);
    }
    const config = JSON.parse(meta.json_val) as StreamConfig;
    const ts = new Date(this.now()).toISOString();
    if (stream === KV_LISTENERS_STREAM) {
      const countRow = this.db.prepare("SELECT COUNT(*) AS n FROM hub_kv").get() as { n: number | bigint };
      const seqRow = this.db.prepare("SELECT int_val FROM hub_meta WHERE key = 'kv_seq'").get() as { int_val: number | bigint };
      const messages = Number(countRow.n);
      const lastSeq = Number(seqRow.int_val);
      return {
        config,
        state: {
          messages,
          last_seq: lastSeq,
          first_seq: messages > 0 ? 1 : 0,
          bytes: 0,
          consumer_count: 0,
        },
        ts,
      };
    }
    if (stream === SIGNALS_STREAM) {
      const countRow = this.db.prepare("SELECT COUNT(*) AS n, COALESCE(MAX(seq), 0) AS max_seq, COALESCE(MIN(seq), 0) AS min_seq FROM hub_signals").get() as {
        n: number | bigint;
        max_seq: number | bigint;
        min_seq: number | bigint;
      };
      const messages = Number(countRow.n);
      return {
        config,
        state: {
          messages,
          last_seq: Number(countRow.max_seq),
          first_seq: Number(countRow.min_seq),
          bytes: 0,
          consumer_count: this.consumers.size,
        },
        ts,
      };
    }
    return {
      config,
      state: { messages: 0, last_seq: 0, first_seq: 0, bytes: 0, consumer_count: 0 },
      ts,
    };
  }

  streamAdd(config: Partial<StreamConfig> & { name: string }): ReturnType<HubEngine["streamInfo"]> {
    const existing = this.db.prepare("SELECT json_val FROM hub_meta WHERE key = ?").get("stream:" + config.name) as { json_val?: string } | undefined;
    if (!existing?.json_val) {
      const merged: StreamConfig = {
        ...DEFAULT_SIGNALS_STREAM_CONFIG,
        ...config,
      };
      this.db.prepare("INSERT OR REPLACE INTO hub_meta (key, int_val, json_val) VALUES (?, 0, ?)").run(
        "stream:" + config.name,
        JSON.stringify(merged),
      );
    }
    return this.streamInfo(config.name);
  }

  streamUpdate(stream: string, patch: Partial<StreamConfig>): ReturnType<HubEngine["streamInfo"]> {
    const info = this.streamInfo(stream);
    const updated: StreamConfig = { ...info.config, ...patch, name: stream };
    this.db.prepare("UPDATE hub_meta SET json_val = ? WHERE key = ?").run(JSON.stringify(updated), "stream:" + stream);
    return this.streamInfo(stream);
  }

  streamGetMessage(
    stream: string,
    seq: number,
    _caller: Principal | null,
  ): { seq: number; subject: string; created_ms: number; data_b64: string } {
    if (stream !== SIGNALS_STREAM) {
      throw new HubError(404, "10037", "no message found", 10037);
    }
    const row = this.db.prepare("SELECT seq, subject, created_ms, data FROM hub_signals WHERE seq = ?").get(seq) as {
      seq: number | bigint;
      subject: string;
      created_ms: number | bigint;
      data: Uint8Array;
    } | undefined;
    if (!row) {
      throw new HubError(404, "10037", "no message found", 10037);
    }
    return {
      seq: Number(row.seq),
      subject: row.subject,
      created_ms: Number(row.created_ms),
      data_b64: Buffer.from(row.data).toString("base64"),
    };
  }

  kvGet(
    key: string,
    caller: Principal | null,
  ): { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" } | { forbidden_scope: true } | null {
    const row = this.db.prepare(
      "SELECT key, revision, created_ms, value, principal_id, scope FROM hub_kv WHERE key = ?",
    ).get(key) as {
      key: string;
      revision: number | bigint;
      created_ms: number | bigint;
      value: Uint8Array;
      principal_id: string | null;
      scope: string | null;
    } | undefined;
    if (!row) return null;
    if (!this.canReadKvRow(row, caller)) {
      const { state } = this.parseKvValue(row.value);
      const lease = evaluateLease(state === "ACTIVE" ? "ACTIVE" : "UNLISTENED", this.now(), Number(row.created_ms));
      if (lease === "ACTIVE" || lease === "DISCONNECTED_RETAINED") {
        return { forbidden_scope: true };
      }
      return null;
    }
    return {
      key: row.key,
      revision: Number(row.revision),
      created_ms: Number(row.created_ms),
      value_b64: Buffer.from(row.value).toString("base64"),
      operation: "PUT",
    };
  }

  private assertKvWriteAllowed(
    existing: { principal_id: string | null } | undefined,
    incomingPrincipalId: string | null,
    caller: Principal | null,
  ): string | null {
    if (this.authMode === "oidc") {
      if (!caller) {
        throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
      }
      if (incomingPrincipalId !== null && incomingPrincipalId !== caller.principal_id) {
        throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot write registration for another principal");
      }
      if (existing?.principal_id && existing.principal_id !== caller.principal_id) {
        throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot overwrite registration owned by another principal");
      }
      return incomingPrincipalId ?? caller.principal_id;
    }
    return incomingPrincipalId ?? caller?.principal_id ?? null;
  }

  kvCreate(key: string, valueBytes: Uint8Array, caller: Principal | null): { revision: number } {
    const { principalId, scope, state } = this.parseKvValue(valueBytes);
    let revision = 0;
    let createdMs = 0;
    let storedPrincipal: string | null = null;
    const buf = Buffer.from(valueBytes);
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      storedPrincipal = this.assertKvWriteAllowed(existing, principalId, caller);
      if (existing) {
        throw new HubError(409, "10071", "wrong last sequence", 10071);
      }
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare(
        "INSERT INTO hub_kv (key, revision, created_ms, value, principal_id, scope) VALUES (?, ?, ?, ?, ?, ?)",
      ).run(key, revision, createdMs, buf, storedPrincipal, scope);
      this.recordAudit(
        storedPrincipal ?? caller?.principal_id ?? "anonymous",
        state === "UNLISTENED" ? "unlisten" : "register",
        key,
        { listener_id: key, revision, scope },
      );
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: buf,
      operation: "PUT",
      principal_id: storedPrincipal,
      scope,
    };
    this.events.emit("kv", ev);
    return { revision };
  }

  kvUpdate(key: string, valueBytes: Uint8Array, expectedRevision: number, caller: Principal | null): { revision: number } {
    const { principalId, scope, state } = this.parseKvValue(valueBytes);
    let revision = 0;
    let createdMs = 0;
    let storedPrincipal: string | null = null;
    const buf = Buffer.from(valueBytes);
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      storedPrincipal = this.assertKvWriteAllowed(existing, principalId, caller);
      if (!existing || Number(existing.revision) !== Number(expectedRevision)) {
        throw new HubError(409, "10071", "wrong last sequence", 10071);
      }
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare(
        "UPDATE hub_kv SET revision = ?, created_ms = ?, value = ?, principal_id = ?, scope = ? WHERE key = ?",
      ).run(revision, createdMs, buf, storedPrincipal, scope, key);
      this.recordAudit(
        storedPrincipal ?? caller?.principal_id ?? "anonymous",
        state === "UNLISTENED" ? "unlisten" : "register",
        key,
        { listener_id: key, revision, scope },
      );
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: buf,
      operation: "PUT",
      principal_id: storedPrincipal,
      scope,
    };
    this.events.emit("kv", ev);
    return { revision };
  }

  kvPut(key: string, valueBytes: Uint8Array, caller: Principal | null): { revision: number } {
    const { principalId, scope, state } = this.parseKvValue(valueBytes);
    let revision = 0;
    let createdMs = 0;
    let storedPrincipal: string | null = null;
    const buf = Buffer.from(valueBytes);
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      storedPrincipal = this.assertKvWriteAllowed(existing, principalId, caller);
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare(
        "INSERT OR REPLACE INTO hub_kv (key, revision, created_ms, value, principal_id, scope) VALUES (?, ?, ?, ?, ?, ?)",
      ).run(key, revision, createdMs, buf, storedPrincipal, scope);
      this.recordAudit(
        storedPrincipal ?? caller?.principal_id ?? "anonymous",
        state === "UNLISTENED" ? "unlisten" : "register",
        key,
        { listener_id: key, revision, scope },
      );
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: buf,
      operation: "PUT",
      principal_id: storedPrincipal,
      scope,
    };
    this.events.emit("kv", ev);
    return { revision };
  }

  kvDelete(key: string, caller: Principal | null): { deleted: boolean } {
    let revision = 0;
    let createdMs = 0;
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      this.assertKvWriteAllowed(existing, null, caller);
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare("DELETE FROM hub_kv WHERE key = ?").run(key);
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: Buffer.alloc(0),
      operation: "DEL",
      principal_id: caller?.principal_id ?? null,
      scope: null,
    };
    this.events.emit("kv", ev);
    return { deleted: true };
  }

  kvKeys(caller: Principal | null): { keys: string[] } {
    const rows = this.db.prepare("SELECT key, principal_id, scope FROM hub_kv ORDER BY revision ASC").all() as {
      key: string;
      principal_id: string | null;
      scope: string | null;
    }[];
    return {
      keys: rows.filter(r => this.canReadKvRow(r, caller)).map(r => r.key),
    };
  }

  kvSnapshot(
    keyFilter: string | undefined,
    caller: Principal | null,
  ): {
    entries: { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" }[];
  } {
    const rows = (
      keyFilter && !keyFilter.includes("*") && !keyFilter.includes(">")
        ? this.db.prepare("SELECT key, revision, created_ms, value, principal_id, scope FROM hub_kv WHERE key = ? ORDER BY revision ASC").all(keyFilter)
        : this.db.prepare("SELECT key, revision, created_ms, value, principal_id, scope FROM hub_kv ORDER BY revision ASC").all()
    ) as {
      key: string;
      revision: number | bigint;
      created_ms: number | bigint;
      value: Uint8Array;
      principal_id: string | null;
      scope: string | null;
    }[];
    const visible = rows.filter(r => this.canReadKvRow(r, caller));
    return {
      entries: visible.map(r => ({
        key: r.key,
        revision: Number(r.revision),
        created_ms: Number(r.created_ms),
        value_b64: Buffer.from(r.value).toString("base64"),
        operation: "PUT" as const,
      })),
    };
  }

  jsPublish(
    subject: string,
    dataBytes: Uint8Array,
    opts: { msgID?: string } | undefined,
    caller: Principal | null,
  ): { seq: number; duplicate: boolean; stream: string } {
    if (!isAllowedCitoSubject(subject)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", `PERMISSIONS_VIOLATION: subject '${subject}' is not permitted`);
    }
    if (subject.startsWith("cito.ack.")) {
      this.ncPublish(subject, dataBytes, caller);
      return { seq: 0, duplicate: false, stream: "" };
    }
    const targetId = subject.slice("cito.signal.".length);
    let parsedSignal: { signal_id?: unknown; from?: { principal_id?: unknown } } | undefined;
    try {
      const raw: unknown = JSON.parse(Buffer.from(dataBytes).toString("utf8"));
      if (raw && typeof raw === "object" && !Array.isArray(raw)) {
        parsedSignal = raw as { signal_id?: unknown; from?: { principal_id?: unknown } };
      }
    } catch {}

    if (this.authMode === "oidc") {
      if (!caller) {
        throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
      }
      if (parsedSignal && "from" in parsedSignal) {
        const fromPrincipal = parsedSignal.from?.principal_id;
        if (typeof fromPrincipal !== "string" || fromPrincipal !== caller.principal_id) {
          throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: signal sender principal mismatch");
        }
      }
      const targetRow = this.db.prepare("SELECT principal_id, scope FROM hub_kv WHERE key = ?").get(targetId) as {
        principal_id: string | null;
        scope: string | null;
      } | undefined;
      if (targetRow && targetRow.scope === "self" && targetRow.principal_id && targetRow.principal_id !== caller.principal_id) {
        throw new HubError(403, "FORBIDDEN_TARGET_SCOPE", "Target listener is private to another user");
      }
    }

    const msgId = opts?.msgID ?? null;
    let seq = 0;
    let duplicate = false;
    let createdMs = 0;
    const buf = Buffer.from(dataBytes);
    this.transaction(() => {
      if (msgId) {
        const existing = this.db.prepare("SELECT seq FROM hub_signals WHERE msg_id = ?").get(msgId) as { seq: number | bigint } | undefined;
        if (existing) {
          seq = Number(existing.seq);
          duplicate = true;
          return;
        }
      }
      createdMs = this.now();
      const res = this.db.prepare(
        "INSERT INTO hub_signals (subject, msg_id, created_ms, data, acked) VALUES (?, ?, ?, ?, 0)",
      ).run(subject, msgId, createdMs, buf);
      seq = Number(res.lastInsertRowid);
      this.recordAudit(
        caller?.principal_id ?? "anonymous",
        "send",
        targetId,
        {
          listener_id: targetId,
          ...(typeof parsedSignal?.signal_id === "string" ? { signal_id: parsedSignal.signal_id } : {}),
          seq,
        },
      );
    });

    if (!duplicate) {
      const ev: HubSignalEvent = {
        seq,
        subject,
        created_ms: createdMs,
        data: buf,
      };
      this.events.emit("signal", ev);
    }
    return { seq, duplicate, stream: SIGNALS_STREAM };
  }

  ncPublish(subject: string, dataBytes: Uint8Array, caller: Principal | null): { ok: true } {
    if (!isAllowedCitoSubject(subject)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", `PERMISSIONS_VIOLATION: subject '${subject}' is not permitted`);
    }
    if (subject.startsWith("cito.signal.")) {
      this.jsPublish(subject, dataBytes, undefined, caller);
      return { ok: true };
    }
    const signalId = subject.slice("cito.ack.".length);
    const buf = Buffer.from(dataBytes);
    this.recordAudit(caller?.principal_id ?? "anonymous", "ack", signalId, { signal_id: signalId });
    const ev: HubNcEvent = { subject, data: buf };
    this.events.emit("nc", ev);
    return { ok: true };
  }

  ncSubscribeCheck(subject: string, _caller: Principal | null): { ok: true } {
    if (!isAllowedCitoSubject(subject)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", `PERMISSIONS_VIOLATION: subject '${subject}' is not permitted`);
    }
    return { ok: true };
  }

  consumerAdd(
    stream: string,
    config: {
      durable_name?: string;
      name?: string;
      filter_subject?: string;
      deliver_policy?: string;
      opt_start_seq?: number;
      [key: string]: unknown;
    } = {},
    caller: Principal | null,
  ): {
    name: string;
    stream_name: string;
    config: Record<string, unknown>;
    backlog: { seq: number; subject: string; created_ms: number; data_b64: string }[];
  } {
    const filterSubject = config.filter_subject;
    if (this.authMode === "oidc" && filterSubject?.startsWith("cito.signal.")) {
      const targetId = filterSubject.slice("cito.signal.".length);
      if (!this.canConsumeSignal(targetId, caller)) {
        throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot consume signals for another principal");
      }
    }
    const name = config.durable_name ?? config.name ?? ("c_" + randomUUID().replaceAll("-", "").slice(0, 12));
    const startSeq = config.deliver_policy === "by_start_sequence" && typeof config.opt_start_seq === "number"
      ? Math.max(1, config.opt_start_seq)
      : 1;
    const rows = (
      filterSubject && !filterSubject.includes("*") && !filterSubject.includes(">")
        ? this.db.prepare("SELECT seq, subject, created_ms, data FROM hub_signals WHERE subject = ? AND seq >= ? ORDER BY seq ASC").all(filterSubject, startSeq)
        : this.db.prepare("SELECT seq, subject, created_ms, data FROM hub_signals WHERE seq >= ? ORDER BY seq ASC").all(startSeq)
    ) as {
      seq: number | bigint;
      subject: string;
      created_ms: number | bigint;
      data: Uint8Array;
    }[];
    const filtered = rows.filter(r => subjectMatchesFilter(r.subject, filterSubject) && (
      !r.subject.startsWith("cito.signal.") || this.canConsumeSignal(r.subject.slice("cito.signal.".length), caller)
    ));
    this.consumers.set(name, {
      name,
      stream,
      filterSubject,
      startSeq,
      cursorSeq: startSeq,
      ackedSeqs: new Set<number>(),
      principalId: caller?.principal_id ?? null,
    });
    return {
      name,
      stream_name: stream,
      config,
      backlog: filtered.map(r => ({
        seq: Number(r.seq),
        subject: r.subject,
        created_ms: Number(r.created_ms),
        data_b64: Buffer.from(r.data).toString("base64"),
      })),
    };
  }

  consumerGetBacklog(
    name: string,
    caller: Principal | null,
  ): {
    filterSubject?: string;
    startSeq: number;
    backlog: { seq: number; subject: string; created_ms: number; data_b64: string }[];
  } {
    const consumer = this.consumers.get(name);
    const filterSubject = consumer?.filterSubject;
    const startSeq = consumer?.startSeq ?? 1;
    const rows = (
      filterSubject && !filterSubject.includes("*") && !filterSubject.includes(">")
        ? this.db.prepare("SELECT seq, subject, created_ms, data FROM hub_signals WHERE subject = ? AND seq >= ? ORDER BY seq ASC").all(filterSubject, startSeq)
        : this.db.prepare("SELECT seq, subject, created_ms, data FROM hub_signals WHERE seq >= ? ORDER BY seq ASC").all(startSeq)
    ) as {
      seq: number | bigint;
      subject: string;
      created_ms: number | bigint;
      data: Uint8Array;
    }[];
    const filtered = rows.filter(r => {
      const seqNum = Number(r.seq);
      if (consumer?.ackedSeqs.has(seqNum)) return false;
      if (!subjectMatchesFilter(r.subject, filterSubject)) return false;
      if (r.subject.startsWith("cito.signal.") && !this.canConsumeSignal(r.subject.slice("cito.signal.".length), caller)) return false;
      return true;
    });
    return {
      filterSubject,
      startSeq,
      backlog: filtered.map(r => ({
        seq: Number(r.seq),
        subject: r.subject,
        created_ms: Number(r.created_ms),
        data_b64: Buffer.from(r.data).toString("base64"),
      })),
    };
  }

  consumerNext(
    _stream: string,
    name: string,
    caller: Principal | null,
  ): { seq: number; subject: string; created_ms: number; data_b64: string } | null {
    const { backlog } = this.consumerGetBacklog(name, caller);
    const consumer = this.consumers.get(name);
    for (const item of backlog) {
      if (!consumer || item.seq >= consumer.cursorSeq) {
        if (consumer) consumer.cursorSeq = item.seq + 1;
        return item;
      }
    }
    return null;
  }

  consumerAck(_stream: string, name: string, seq: number, _caller: Principal | null): { acked: true } {
    const consumer = this.consumers.get(name);
    consumer?.ackedSeqs.add(seq);
    this.db.prepare("UPDATE hub_signals SET acked = 1 WHERE seq = ?").run(seq);
    return { acked: true };
  }

  consumerDelete(_stream: string, name: string, _caller: Principal | null): { deleted: true } {
    this.consumers.delete(name);
    return { deleted: true };
  }

  handleRpc(op: string, params: Record<string, unknown>, caller: Principal | null): unknown {
    switch (op) {
      case "stream_info":
        return this.streamInfo(String(params.stream ?? ""));
      case "stream_add":
        return this.streamAdd((params.config ?? {}) as Partial<StreamConfig> & { name: string });
      case "stream_update":
        return this.streamUpdate(String(params.stream ?? ""), (params.patch ?? {}) as Partial<StreamConfig>);
      case "stream_get_message":
        return this.streamGetMessage(String(params.stream ?? ""), Number(params.seq ?? 0), caller);
      case "kv_get":
        return this.kvGet(String(params.key ?? ""), caller);
      case "kv_create":
        return this.kvCreate(String(params.key ?? ""), Buffer.from(String(params.data_b64 ?? ""), "base64"), caller);
      case "kv_update":
        return this.kvUpdate(
          String(params.key ?? ""),
          Buffer.from(String(params.data_b64 ?? ""), "base64"),
          Number(params.revision ?? 0),
          caller,
        );
      case "kv_put":
        return this.kvPut(String(params.key ?? ""), Buffer.from(String(params.data_b64 ?? ""), "base64"), caller);
      case "kv_delete":
        return this.kvDelete(String(params.key ?? ""), caller);
      case "kv_keys":
        return this.kvKeys(caller);
      case "kv_snapshot":
        return this.kvSnapshot(typeof params.key === "string" ? params.key : undefined, caller);
      case "js_publish":
        return this.jsPublish(
          String(params.subject ?? ""),
          Buffer.from(String(params.data_b64 ?? ""), "base64"),
          params.opts as { msgID?: string } | undefined,
          caller,
        );
      case "nc_publish":
        return this.ncPublish(String(params.subject ?? ""), Buffer.from(String(params.data_b64 ?? ""), "base64"), caller);
      case "nc_subscribe_check":
        return this.ncSubscribeCheck(String(params.subject ?? ""), caller);
      case "consumer_add":
        return this.consumerAdd(String(params.stream ?? SIGNALS_STREAM), (params.config ?? {}) as Record<string, unknown>, caller);
      case "consumer_backlog":
        return this.consumerGetBacklog(String(params.name ?? ""), caller);
      case "consumer_next":
        return this.consumerNext(String(params.stream ?? SIGNALS_STREAM), String(params.name ?? ""), caller);
      case "consumer_ack":
        return this.consumerAck(String(params.stream ?? SIGNALS_STREAM), String(params.name ?? ""), Number(params.seq ?? 0), caller);
      case "consumer_delete":
        return this.consumerDelete(String(params.stream ?? SIGNALS_STREAM), String(params.name ?? ""), caller);
      case "account_info": {
        const kvCount = Number((this.db.prepare("SELECT COUNT(*) AS n FROM hub_kv").get() as { n: number | bigint }).n);
        const sigCount = Number((this.db.prepare("SELECT COUNT(*) AS n FROM hub_signals").get() as { n: number | bigint }).n);
        return {
          streams: kvCount === 0 && sigCount === 0 ? 0 : 2,
          consumers: this.consumers.size,
          memory: 0,
          storage: 0,
        };
      }
      default:
        throw new HubError(400, "INVALID_INPUT", `Unknown hub RPC op: ${op}`);
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.events.removeAllListeners();
    try { this.db.close(); } catch {}
  }
}
