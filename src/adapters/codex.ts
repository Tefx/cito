import { execFile } from "node:child_process";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { promisify } from "node:util";
import { CitoError, loadConfig, type CitoConfig } from "../shared/index.js";
import { normalizeRepo, gitMetadata, type RepoMetadata } from "./git.js";
import type { ListenerRow } from "../shared/state-types.js";
import type { AvailableSessionSummary, Caller, CallerDetectionContext, HarnessAdapter, ProbeOutcome, DeliverContext, DeliverOutcome } from "./types.js";

const exec = promisify(execFile);
type CodexConfig = Pick<CitoConfig, "codexHome">;
export interface CodexCaller extends RepoMetadata { harness: "codex"; session_id: string; cwd: string }
export interface CodexDriverOptions { codexBin?: string; openBin?: string; signal?: AbortSignal; env?: NodeJS.ProcessEnv }
function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function nonempty(value: unknown): value is string { return typeof value === "string" && value.trim().length > 0; }
export function codexThreadId(meta: Record<string, unknown> = {}): string {
  let turn = meta["x-codex-turn-metadata"];
  if (typeof turn === "string") { try { turn = JSON.parse(turn); } catch { turn = undefined; } }
  const expectedCodex = meta.expected_harness === "codex" ? meta.expected_session_id : undefined;
  const candidates = [...(record(turn) ? [turn.thread_id, turn.session_id] : []), meta["openai/threadId"], meta.codexThreadId, expectedCodex];
  const ids = [...new Set(candidates.filter(nonempty))];
  if (ids.length > 1) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  const id = ids[0];
  if (!id) throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required");
  return id;
}

function userThreadFilters(cols: Set<string>): string[] {
  const filters: string[] = [];
  if (cols.has("thread_source")) filters.push("COALESCE(thread_source, 'user') = 'user'");
  if (cols.has("source")) filters.push('COALESCE(source, \'\') NOT LIKE \'{"subagent":%\'');
  return filters;
}
function threadOrderBy(cols: Set<string>): string {
  return [
    ...(cols.has("recency_at_ms") ? ["recency_at_ms DESC"] : []),
    ...(cols.has("updated_at") ? ["updated_at DESC"] : []),
    "rowid DESC",
  ].join(", ");
}

export function resolveCodexThreadId(cwd: string, config: CodexConfig = loadConfig(), options: { allowArchived?: boolean } = {}): string {
  let db: DatabaseSync | undefined;
  try {
    db = new DatabaseSync(join(config.codexHome, "state_5.sqlite"), { readOnly: true });
    db.exec("PRAGMA busy_timeout = 1000");
    const columns = db.prepare("PRAGMA table_info(threads)").all() as { name?: unknown }[];
    if (!columns.length) throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required");
    const cols = new Set(columns.flatMap(col => typeof col.name === "string" ? [col.name] : []));
    const userFilters = userThreadFilters(cols), order = threadOrderBy(cols);
    const whereClause = (preds: string[]) => preds.length ? `WHERE ${preds.join(" AND ")} ` : "";
    const queries: [string, ...string[]][] = [
      [`SELECT id FROM threads ${whereClause(["archived = 0", "cwd = ?", ...userFilters])}ORDER BY ${order} LIMIT 1`, cwd],
      [`SELECT id FROM threads ${whereClause(["archived = 0", ...userFilters])}ORDER BY ${order} LIMIT 1`],
      ...(options.allowArchived ? [
        [`SELECT id FROM threads ${whereClause(["cwd = ?", ...userFilters])}ORDER BY ${order} LIMIT 1`, cwd] as [string, string],
        [`SELECT id FROM threads ${whereClause(userFilters)}ORDER BY ${order} LIMIT 1`] as [string],
      ] : []),
    ];
    for (const [sql, ...params] of queries) {
      const row = db.prepare(sql).get(...params) as { id?: unknown } | undefined;
      if (nonempty(row?.id)) return row.id;
    }
  } catch (error) {
    if (error instanceof CitoError) throw error;
    throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required", { cause: error });
  } finally {
    db?.close();
  }
  throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required");
}

export type OptionalCodexThread = { status: "archived" } | { status: "unavailable" } | ({ status: "found" } & CodexCaller);
/** Optional metadata: missing rows/schema and busy databases never prove closure. */
export function readCodexThreadOptional(threadId: string, config: CodexConfig = loadConfig()): OptionalCodexThread {
  let db: DatabaseSync | undefined;
  try {
    db = new DatabaseSync(join(config.codexHome, "state_5.sqlite"), { readOnly: true });
    db.exec("PRAGMA busy_timeout = 100");
    const row = db.prepare("SELECT * FROM threads WHERE id = ?").get(threadId);
    if (!row || row.archived === undefined) return { status: "unavailable" };
    if (row.archived !== 0) return { status: "archived" };
    if (!nonempty(row.cwd)) return { status: "unavailable" };
    return { status: "found", harness: "codex", session_id: threadId, cwd: row.cwd,
      title: (typeof row.name === "string" && row.name.trim()) || (typeof row.title === "string" && row.title.trim()) || basename(row.cwd),
      repo: normalizeRepo(typeof row.git_origin_url === "string" ? row.git_origin_url : null),
      branch: typeof row.git_branch === "string" ? row.git_branch : null };
  } catch { return { status: "unavailable" }; } finally { db?.close(); }
}
export async function codexDesktopRunning(signal?: AbortSignal): Promise<boolean> {
  const { stdout } = await exec("/bin/ps", ["-axo", "command="], { timeout: 1000, signal });
  return stdout.split("\n").some(line => /\/ChatGPT\.app\/|\/Codex\.app\//.test(line));
}

/** The caller owns authorization and the listener lock around EACH separate effect. No retries here. */
export async function queueCodexWake(threadId: string, card: string, options: CodexDriverOptions = {}): Promise<void> {
  options.signal?.throwIfAborted();
  await exec(options.codexBin ?? loadConfig().codexBin, ["queue", "--resume", "--thread", threadId, "--message", card],
    { timeout: 10_000, signal: options.signal, env: options.env ?? process.env });
}
export class CodexDesktopAdapter implements HarnessAdapter {
  readonly id = "codex";
  readonly canJump = true;
  async discoverAvailableSessions(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]> {
    try {
      if (!await (config.codexDesktopRunning ?? codexDesktopRunning)(signal ?? new AbortController().signal)) return [];
    } catch { return []; }
    let db: DatabaseSync | undefined;
    try {
      db = new DatabaseSync(join(config.codexHome, "state_5.sqlite"), { readOnly: true });
      db.exec("PRAGMA busy_timeout = 100");
      const columns = db.prepare("PRAGMA table_info(threads)").all() as { name?: unknown }[];
      const cols = new Set(columns.flatMap(col => typeof col.name === "string" ? [col.name] : []));
      if (!cols.has("id") || !cols.has("cwd") || !cols.has("archived")) return [];
      const where = ["archived = 0", ...userThreadFilters(cols)].join(" AND ");
      const order = threadOrderBy(cols);
      const rows = db.prepare(`SELECT * FROM threads WHERE ${where} ORDER BY ${order} LIMIT 8`).all() as Record<string, unknown>[];
      const summaries: AvailableSessionSummary[] = [];
      for (const row of rows) {
        if (!nonempty(row.id) || !nonempty(row.cwd)) continue;
        const title = (typeof row.name === "string" && row.name.trim())
          || (typeof row.title === "string" && row.title.trim())
          || basename(row.cwd) || "codex";
        const branch = typeof row.git_branch === "string" && row.git_branch.trim() ? row.git_branch.trim() : null;
        const rawTs = typeof row.recency_at_ms === "number" && Number.isFinite(row.recency_at_ms) ? row.recency_at_ms
          : typeof row.updated_at === "number" && Number.isFinite(row.updated_at) ? (row.updated_at < 1e11 ? row.updated_at * 1000 : row.updated_at)
          : undefined;
        summaries.push({
          harness: this.id,
          session_id: row.id,
          title,
          cwd: row.cwd,
          branch,
          ...(rawTs !== undefined ? { updated_at_ms: Math.round(rawTs) } : {}),
        });
      }
      return summaries;
    } catch { return []; } finally { db?.close(); }
  }
  async detectExplicitCaller(ctx: CallerDetectionContext, config: CitoConfig): Promise<Caller | null> {
    const claim = (meta: Record<string, unknown>) => {
      try { return codexThreadId(meta); }
      catch (error) { if (error instanceof CitoError && error.code === "BINDING_REQUIRED") return; throw error; }
    };
    const turnObject = (value: unknown): Record<string, unknown> | undefined => {
      if (typeof value === "string") { try { value = JSON.parse(value); } catch { return; } }
      return record(value) ? value : undefined;
    };
    const meta = { ...ctx.meta };
    const raw = ctx.headers?.["x-codex-turn-metadata"];
    const headers = Array.isArray(raw) ? raw : raw === undefined ? [] : [raw];
    let thread = claim(meta);
    for (const header of headers) {
      const headerThread = claim({ "x-codex-turn-metadata": header });
      if (thread && headerThread && thread !== headerThread) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata in HTTP header");
      const bodyTurn = turnObject(meta["x-codex-turn-metadata"]), headerTurn = turnObject(header);
      meta["x-codex-turn-metadata"] = bodyTurn || headerTurn ? { ...bodyTurn, ...headerTurn } : header;
      thread = claim(meta);
    }
    if (!thread) return null;
    if (["endpoint_pid", "caller_pid", "pi_pid"].some(key => meta[key] !== undefined)) {
      throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    }
    const row = readCodexThreadOptional(thread, config);
    if (row.status === "found") return row;
    if (row.status === "archived" && ctx.requireOpen !== false) throw new CitoError("SESSION_NOT_OPEN", "Codex thread is archived");
    let cwd = typeof meta.cwd === "string" && meta.cwd.trim() ? meta.cwd : undefined;
    if (!cwd && Number.isSafeInteger(ctx.httpClientPid) && ctx.httpClientPid! > 0) {
      try {
        const { stdout } = await exec("/usr/sbin/lsof", ["-a", "-d", "cwd", "-p", String(ctx.httpClientPid), "-Fn"], { timeout: 1000 });
        cwd = stdout.split("\n").find(line => line.startsWith("n"))?.slice(1) || undefined;
      } catch { /* Optional OS metadata. */ }
    }
    if (!cwd) cwd = (await ctx.lookupCachedSession?.({ harness: this.id, sessionId: thread }))?.cwd;
    cwd ||= process.cwd();
    return { harness: this.id, session_id: thread, cwd, ...await gitMetadata(cwd, typeof meta.title === "string" ? meta.title : undefined) };
  }
  async probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome> {
    try {
      if (readCodexThreadOptional(row.session_id, config).status === "archived") return { status: "closed_definitive" };
      return { status: await (config.codexDesktopRunning ?? codexDesktopRunning)(signal) ? "alive" : "closed_definitive" };
    } catch (error) {
      return { status: "transient_error", error };
    }
  }
  async deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome> {
    // A recovered queue-written batch has no durable proof of a clean queue exit.
    let cleanQueueExit = false;
    if (!ctx.queueWritten) {
      const queued = await ctx.runEffect(async () => {
        await queueCodexWake(row.session_id, ctx.card, { codexBin: ctx.config.codexBin,
          signal: ctx.signal, env: { ...process.env, CODEX_HOME: ctx.config.codexHome } });
        cleanQueueExit = true;
      }, { attempts: 3, pause: [1000, 2000], onSigtermTimeout: () => { cleanQueueExit = false; } });
      if (!queued) return "driver_unavailable";
      ctx.markQueueWritten();
    }
    const triggered = await ctx.runEffect(() => triggerCodexWake(row.session_id,
      { openBin: ctx.config.openBin, signal: ctx.signal }), { attempts: 2, pause: [500] });
    return cleanQueueExit && triggered ? "delivered" : "delivered_unconfirmed";
  }
}
export async function triggerCodexWake(threadId: string, options: CodexDriverOptions = {}): Promise<void> {
  options.signal?.throwIfAborted();
  await exec(options.openBin ?? "/usr/bin/open", ["-g", "codex://threads/wake/" + threadId],
    { timeout: 3000, signal: options.signal });
}
