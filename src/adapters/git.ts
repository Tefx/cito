import { execFile } from "node:child_process";
import { basename } from "node:path";
import { promisify } from "node:util";

const exec = promisify(execFile);

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
  async function git(args: string[]): Promise<string | null> {
    try {
      const { stdout } = await exec("git", ["-C", cwd, ...args], { timeout: 1000 });
      return stdout.trim() || null;
    } catch { return null; }
  }
  const [origin, branch] = await Promise.all([
    git(["remote", "get-url", "origin"]), git(["rev-parse", "--abbrev-ref", "HEAD"]),
  ]);
  return { repo: normalizeRepo(origin), branch, title: sessionName || basename(cwd) };
}
