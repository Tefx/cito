import { execFile } from "node:child_process";
import { basename } from "node:path";
import { promisify } from "node:util";
import type { Caller, CallerDetectionContext } from "./types.js";

const exec = promisify(execFile);
export const PID_KEYS = ["endpoint_pid", "caller_pid", "pi_pid"] as const;
export function hasExplicitPid(meta: Record<string, unknown>): boolean {
  return PID_KEYS.some(key => meta[key] !== undefined);
}

/** Normalize remote paths without changing their display case. */
export function normalizeRepo(origin: string | null | undefined): string | null {
  if (!origin?.trim()) return null;
  const remote = origin.trim().replace(/\/+$/, "").replace(/\.git$/, "");
  const scp = /^[^/@\s]+@[^/:\s]+:(.+)$/.exec(remote);
  if (scp) return scp[1]!.replace(/^\/+/, "") || null;
  try {
    const url = new URL(remote);
    return url.pathname.replace(/^\/+/, "") || null;
  } catch { return null; }
}
export interface RepoMetadata { repo: string | null; branch: string | null; title: string }
export async function gitMetadata(cwd: string, sessionName?: string | null): Promise<RepoMetadata> {
  const env = { ...process.env };
  for (const key of ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"]) delete env[key];
  async function git(args: string[]): Promise<string | null> {
    try {
      const { stdout } = await exec("git", ["-C", cwd, ...args], { timeout: 1000, env });
      return stdout.trim() || null;
    } catch { return null; }
  }
  const [origin, branch] = await Promise.all([
    git(["remote", "get-url", "origin"]), git(["rev-parse", "--abbrev-ref", "HEAD"]),
  ]);
  return { repo: normalizeRepo(origin), branch, title: sessionName || basename(cwd) };
}
export async function resolveExplicitCaller(
  harness: string,
  sessionId: string,
  ctx: CallerDetectionContext,
): Promise<Caller> {
  let cwd = typeof ctx.meta.cwd === "string" && ctx.meta.cwd.trim() ? ctx.meta.cwd.trim() : undefined;
  if (!cwd && Number.isSafeInteger(ctx.httpClientPid) && ctx.httpClientPid! > 0) {
    try {
      const { stdout } = await exec("/usr/sbin/lsof", ["-a", "-d", "cwd", "-p", String(ctx.httpClientPid), "-Fn"], { timeout: 1000 });
      cwd = stdout.split("\n").find(line => line.startsWith("n"))?.slice(1) || undefined;
    } catch { /* Optional OS metadata. */ }
  }
  if (!cwd) cwd = (await ctx.lookupCachedSession?.({ harness, sessionId }))?.cwd;
  cwd ||= process.cwd();
  const title = typeof ctx.meta.title === "string" ? ctx.meta.title : undefined;
  return { harness, session_id: sessionId, cwd, ...await gitMetadata(cwd, title) };
}
