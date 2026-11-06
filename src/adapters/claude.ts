import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, open, readFile, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { promisify } from "node:util";
import { CitoError, loadConfig, type CitoConfig } from "../shared/index.js";
import { gitMetadata } from "./git.js";
import { SocketBridgeAdapter } from "./pi.js";
import type { Caller, CallerDetectionContext } from "./types.js";

const exec = promisify(execFile);
type ClaudeConfig = Pick<CitoConfig, "claudeHome">;
type QueueConfig = Pick<CitoConfig, "stateDir">;
export const CLAUDE_HARNESS = "claude";
const TRANSCRIPT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// Session identifiers become file names: keep them to one path segment with no traversal.
const QUEUE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const DRAIN_SUFFIX = ".draining-";
const MAX_QUEUE_BYTES = 4 * 1024 * 1024;

function nonempty(value: unknown): value is string { return typeof value === "string" && value.trim().length > 0; }
export function validateClaudeSessionId(value: unknown): string {
  if (!nonempty(value) || !QUEUE_ID.test(value.trim())) throw new CitoError("INVALID_INPUT", "Invalid Claude session identifier");
  return value.trim();
}
/** Claude Code slugs the absolute workspace path into one transcript directory per workspace. */
export function claudeProjectDir(cwd: string, config: ClaudeConfig = loadConfig()): string {
  return join(config.claudeHome, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"));
}
/** The transcript Claude Code appends to names the session it is running; /clear starts a new one. */
export async function resolveClaudeSessionId(cwd: string, config: ClaudeConfig = loadConfig()): Promise<string | undefined> {
  const dir = claudeProjectDir(cwd, config);
  let names: string[];
  try { names = await readdir(dir); } catch { return undefined; }
  let newest: { id: string; at: number } | undefined;
  for (const name of names) {
    const id = name.endsWith(".jsonl") ? name.slice(0, -".jsonl".length) : "";
    if (!TRANSCRIPT_ID.test(id)) continue;
    try {
      const info = await stat(join(dir, name));
      if (info.isFile() && (!newest || info.mtimeMs > newest.at)) newest = { id, at: info.mtimeMs };
    } catch { /* A transcript removed mid-scan is simply not the newest. */ }
  }
  return newest?.id;
}
/** Explicit session claims from tool metadata or an HTTP header, with the same conflict rules as Codex threads. */
export function claudeSessionClaim(meta: Record<string, unknown> = {},
  headers?: Record<string, string | string[] | undefined>): string | undefined {
  const header = headers?.["x-claude-session-id"];
  const candidates = [meta.claudeSessionId, meta.claude_session_id,
    meta.expected_harness === CLAUDE_HARNESS ? meta.expected_session_id : undefined,
    ...(Array.isArray(header) ? header : header === undefined ? [] : [header])];
  const ids = [...new Set(candidates.filter(nonempty).map(value => value.trim()))];
  if (ids.length > 1) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  return ids.length ? validateClaudeSessionId(ids[0]) : undefined;
}

export interface ClaudeWake { batch_id: string; content: string; wake: boolean; created_at_ms: number }
function wakeDir(config: QueueConfig): string { return join(config.stateDir, "claude", "wakes"); }
export function claudeWakeQueuePath(config: QueueConfig, sessionId: string): string {
  return join(wakeDir(config), validateClaudeSessionId(sessionId) + ".jsonl");
}
export function claudeBadgePath(config: QueueConfig, sessionId: string): string {
  return join(config.stateDir, "claude", "badges", validateClaudeSessionId(sessionId));
}
/** Claude Code has no queue CLI and no wake URL scheme: the durable queue IS the delivery effect. */
export async function appendClaudeWake(config: QueueConfig, sessionId: string, wake: ClaudeWake): Promise<void> {
  const path = claudeWakeQueuePath(config, sessionId);
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  const file = await open(path, "a", 0o600);
  // fsync before the daemon is told the card is queued; a hook may only run minutes later.
  try { await file.writeFile(JSON.stringify(wake) + "\n"); await file.sync(); } finally { await file.close(); }
}
function parseWakes(text: string): ClaudeWake[] {
  return text.split("\n").flatMap(line => {
    if (!line.trim()) return [];
    try {
      const value: unknown = JSON.parse(line);
      if (!value || typeof value !== "object" || Array.isArray(value)) return [];
      const row = value as Record<string, unknown>;
      if (!nonempty(row.batch_id) || typeof row.content !== "string") return [];
      return [{ batch_id: row.batch_id, content: row.content, wake: row.wake !== false,
        created_at_ms: typeof row.created_at_ms === "number" && Number.isFinite(row.created_at_ms) ? row.created_at_ms : 0 }];
    } catch { return []; } // A torn trailing line is recovered on the next append, never on a replay.
  });
}
async function readQueue(path: string): Promise<string> {
  const info = await stat(path);
  if (info.size > MAX_QUEUE_BYTES) throw new CitoError("INVALID_INPUT", "Claude wake queue exceeds 4 MiB");
  return await readFile(path, "utf8");
}
export async function peekClaudeWakes(config: QueueConfig, sessionId: string): Promise<ClaudeWake[]> {
  try { return parseWakes(await readQueue(claudeWakeQueuePath(config, sessionId))); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
}
/**
 * Rename-then-read: an appender racing the drain recreates the queue instead of losing a card, and a
 * drain that dies before unlinking leaves its staging file behind for the next run to pick up.
 */
export async function drainClaudeWakes(config: QueueConfig, sessionId: string): Promise<ClaudeWake[]> {
  const path = claudeWakeQueuePath(config, sessionId), dir = dirname(path);
  try { await rename(path, path + DRAIN_SUFFIX + randomUUID()); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  const prefix = basename(path) + DRAIN_SUFFIX;
  let names: string[];
  try { names = await readdir(dir); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
  const staged = await Promise.all(names.filter(name => name.startsWith(prefix)).map(async name => {
    const file = join(dir, name);
    try { return { file, at: (await stat(file)).mtimeMs, text: await readQueue(file) }; }
    catch { return undefined; }
  }));
  const ordered = staged.flatMap(entry => entry ? [entry] : []).sort((a, b) => a.at - b.at || a.file.localeCompare(b.file));
  const wakes: ClaudeWake[] = [], seen = new Set<string>();
  for (const entry of ordered) {
    for (const wake of parseWakes(entry.text)) {
      if (seen.has(wake.batch_id)) continue;
      seen.add(wake.batch_id); wakes.push(wake);
    }
  }
  await Promise.all(ordered.map(entry => rm(entry.file, { force: true })));
  return wakes;
}
export async function writeClaudeBadge(config: QueueConfig, sessionId: string, badge: string | undefined): Promise<void> {
  const path = claudeBadgePath(config, sessionId);
  if (badge === undefined) { await rm(path, { force: true }); return; }
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  await writeFile(path, badge + "\n", { mode: 0o600 });
}

export class ClaudeCodeAdapter extends SocketBridgeAdapter {
  constructor() {
    // A queued card only reaches the model at the next hook boundary, so it is never proof of a read.
    super(CLAUDE_HARNESS, { confirmDelivery: false });
  }
  /**
   * Session liveness stays with the socket bridge: a transcript on disk outlives the process that
   * wrote it, so a missing or stale transcript never proves a session closed.
   */
  async detectExplicitCaller(ctx: CallerDetectionContext, _config: CitoConfig): Promise<Caller | null> {
    const session = claudeSessionClaim(ctx.meta, ctx.headers);
    if (!session) return null;
    if (["endpoint_pid", "caller_pid", "pi_pid"].some(key => ctx.meta[key] !== undefined)) {
      throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    }
    let cwd = typeof ctx.meta.cwd === "string" && ctx.meta.cwd.trim() ? ctx.meta.cwd.trim() : undefined;
    if (!cwd && Number.isSafeInteger(ctx.httpClientPid) && ctx.httpClientPid! > 0) {
      try {
        const { stdout } = await exec("/usr/sbin/lsof", ["-a", "-d", "cwd", "-p", String(ctx.httpClientPid), "-Fn"], { timeout: 1000 });
        cwd = stdout.split("\n").find(line => line.startsWith("n"))?.slice(1) || undefined;
      } catch { /* Optional OS metadata. */ }
    }
    if (!cwd) cwd = (await ctx.lookupCachedSession?.({ harness: CLAUDE_HARNESS, sessionId: session }))?.cwd;
    cwd ||= process.cwd();
    const title = typeof ctx.meta.title === "string" ? ctx.meta.title : undefined;
    return { harness: CLAUDE_HARNESS, session_id: session, cwd, ...await gitMetadata(cwd, title) };
  }
}
