import { execFile } from "node:child_process";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { promisify } from "node:util";
import { CitoError, loadConfig, type CitoConfig } from "../shared/index.js";
import { normalizeRepo, type RepoMetadata } from "./git.js";
import type { ListenerRow } from "../shared/state-types.js";
import type { HarnessAdapter, ProbeOutcome, DeliverContext, DeliverOutcome } from "./types.js";

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
  const candidates = [...(record(turn) ? [turn.thread_id, turn.session_id] : []), meta["openai/threadId"], meta.codexThreadId];
  const ids = [...new Set(candidates.filter(nonempty))];
  if (ids.length > 1) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  const id = ids[0];
  if (!id) throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required");
  return id;
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
