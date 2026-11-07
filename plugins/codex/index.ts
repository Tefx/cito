import { execFile } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { promisify } from "node:util";
import { CitoError, expandHomeDir, loadConfig, type CitoConfig } from "../../src/shared/index.js";
import type { ListenerRow } from "../../src/shared/state-types.js";
import { hasExplicitPid, normalizeRepo, resolveExplicitCaller, type RepoMetadata } from "../../src/adapters/git.js";
import type {
  AvailableSessionSummary,
  Caller,
  CallerDetectionContext,
  DeliverContext,
  DeliverOutcome,
  HarnessAdapter,
  HarnessCliDescriptor,
  PluginManifest,
  ProbeOutcome,
} from "../../src/adapters/types.js";

const exec = promisify(execFile);
const CLI_UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const DEFAULT_CODEX_BIN = "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/codex/plugin.json", import.meta.url);
const codexManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

export interface CodexPluginConfig {
  codexHome: string;
  codexBin: string;
  codexDesktopRunning?: (signal: AbortSignal) => Promise<boolean>;
}
export interface CodexConfigInput {
  env?: NodeJS.ProcessEnv;
  plugins?: Record<string, Record<string, unknown>>;
  codexHome?: string;
  codexBin?: string;
  codexDesktopRunning?: (signal: AbortSignal) => Promise<boolean>;
}

export function loadCodexConfig(config: CodexConfigInput = loadConfig()): CodexPluginConfig {
  const pluginCfg = (config.plugins?.codex ?? {}) as {
    codexHome?: unknown;
    codexBin?: unknown;
    codexDesktopRunning?: unknown;
  };
  const env = config.env ?? process.env;
  const rawHome = typeof pluginCfg.codexHome === "string"
    ? pluginCfg.codexHome
    : typeof config.codexHome === "string"
    ? config.codexHome
    : env.CITO_CODEX_HOME ?? "~/.codex";
  const rawBin = typeof pluginCfg.codexBin === "string"
    ? pluginCfg.codexBin
    : typeof config.codexBin === "string"
    ? config.codexBin
    : env.CITO_CODEX_BIN ?? DEFAULT_CODEX_BIN;
  if (!rawBin.trim()) throw new Error("CITO_CODEX_BIN must not be empty");
  const desktopRunning = typeof pluginCfg.codexDesktopRunning === "function"
    ? pluginCfg.codexDesktopRunning as (signal: AbortSignal) => Promise<boolean>
    : typeof config.codexDesktopRunning === "function"
    ? config.codexDesktopRunning
    : undefined;
  return {
    codexHome: expandHomeDir(rawHome, "CITO_CODEX_HOME"),
    codexBin: rawBin,
    ...(desktopRunning ? { codexDesktopRunning: desktopRunning } : {}),
  };
}

export interface CodexCaller extends RepoMetadata { harness: "codex"; session_id: string; cwd: string }
export interface CodexDriverOptions { codexBin?: string; openBin?: string; signal?: AbortSignal; env?: NodeJS.ProcessEnv }
function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function nonempty(value: unknown): value is string { return typeof value === "string" && value.trim().length > 0; }
export function codexThreadId(meta: Record<string, unknown> = {}): string {
  let turn = meta["x-codex-turn-metadata"];
  if (typeof turn === "string") { try { turn = JSON.parse(turn); } catch { turn = undefined; } }
  const expectedCodex = !hasExplicitPid(meta) && meta.expected_harness === "codex" ? meta.expected_session_id : undefined;
  const candidates = [...(record(turn) ? [turn.thread_id, turn.session_id] : []), meta["openai/threadId"], meta.codexThreadId, expectedCodex];
  const ids = [...new Set(candidates.filter(nonempty).map(value => value.trim()))];
  if (ids.length > 1) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  const id = ids[0];
  if (!id) throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required");
  return id;
}

function userThreadFilters(cols: Set<string>): string[] {
  const filters: string[] = [];
  if (cols.has("thread_source")) filters.push("COALESCE(thread_source, 'user') = 'user'");
  if (cols.has("source")) filters.push("COALESCE(source, '') NOT LIKE '{\"subagent\":%'");
  return filters;
}
function threadOrderBy(cols: Set<string>): string {
  return [
    ...(cols.has("recency_at_ms") ? ["recency_at_ms DESC"] : []),
    ...(cols.has("updated_at") ? ["updated_at DESC"] : []),
    "rowid DESC",
  ].join(", ");
}

export function resolveCodexThreadId(cwd: string, config: CodexConfigInput = loadConfig(), options: { allowArchived?: boolean } = {}): string {
  const codexCfg = loadCodexConfig(config);
  let db: DatabaseSync | undefined;
  try {
    db = new DatabaseSync(join(codexCfg.codexHome, "state_5.sqlite"), { readOnly: true });
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
export function readCodexThreadOptional(threadId: string, config: CodexConfigInput = loadConfig()): OptionalCodexThread {
  const codexCfg = loadCodexConfig(config);
  let db: DatabaseSync | undefined;
  try {
    db = new DatabaseSync(join(codexCfg.codexHome, "state_5.sqlite"), { readOnly: true });
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
  await exec(options.codexBin ?? loadCodexConfig().codexBin, ["queue", "--resume", "--thread", threadId, "--message", card],
    { timeout: 10_000, signal: options.signal, env: options.env ?? process.env });
}
export class CodexDesktopAdapter implements HarnessAdapter {
  readonly id = "codex";
  readonly manifest: PluginManifest;
  readonly canJump = true;
  readonly cli: HarnessCliDescriptor = {
    consumesNextArg: (cmd, nextArg, config) =>
      cmd === "unlisten" || CLI_UUID_RE.test(nextArg) || readCodexThreadOptional(nextArg, config).status !== "unavailable",
    resolveMeta: (cmd, explicitId, cwd, config) => ({
      codexThreadId: explicitId ?? resolveCodexThreadId(cwd, config, { allowArchived: cmd === "unlisten" }),
      cwd,
    }),
  };

  constructor(manifest: PluginManifest = codexManifest) {
    this.manifest = manifest;
  }

  async discoverAvailableSessions(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]> {
    const codexCfg = loadCodexConfig(config);
    try {
      if (!await (codexCfg.codexDesktopRunning ?? codexDesktopRunning)(signal ?? new AbortController().signal)) return [];
    } catch { return []; }
    let db: DatabaseSync | undefined;
    try {
      db = new DatabaseSync(join(codexCfg.codexHome, "state_5.sqlite"), { readOnly: true });
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
    if (hasExplicitPid(meta)) {
      throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    }
    const row = readCodexThreadOptional(thread, config);
    if (row.status === "found") return row;
    if (row.status === "archived" && ctx.requireOpen !== false) throw new CitoError("SESSION_NOT_OPEN", "Codex thread is archived");
    return resolveExplicitCaller(this.id, thread, { ...ctx, meta });
  }
  async probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome> {
    const codexCfg = loadCodexConfig(config);
    try {
      if (readCodexThreadOptional(row.session_id, config).status === "archived") return { status: "closed_definitive" };
      return { status: await (codexCfg.codexDesktopRunning ?? codexDesktopRunning)(signal) ? "alive" : "closed_definitive" };
    } catch (error) {
      return { status: "transient_error", error };
    }
  }
  async deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome> {
    const codexCfg = loadCodexConfig(ctx.config);
    // A recovered queue-written batch has no durable proof of a clean queue exit.
    let cleanQueueExit = false;
    if (!ctx.queueWritten) {
      const queued = await ctx.runEffect(async () => {
        await queueCodexWake(row.session_id, ctx.card, { codexBin: codexCfg.codexBin,
          signal: ctx.signal, env: { ...process.env, CODEX_HOME: codexCfg.codexHome } });
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

export const codexAdapter = new CodexDesktopAdapter(codexManifest);
export default (manifest: PluginManifest) => new CodexDesktopAdapter(manifest);
