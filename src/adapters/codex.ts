import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { promisify } from "node:util";
import { CitoError, loadConfig, type CitoConfig } from "../shared/index.js";
import { normalizeRepo, type RepoMetadata } from "./git.js";

const exec = promisify(execFile);
type CodexConfig = Pick<CitoConfig, "codexHome">;
export interface CodexCaller extends RepoMetadata { harness: "codex"; session_id: string; cwd: string }
export interface CodexDriverOptions { codexBin?: string; openBin?: string; signal?: AbortSignal }
export interface ReadCodexThreadOptions { allowArchived?: boolean }
function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function nonempty(value: unknown): value is string { return typeof value === "string" && value.trim().length > 0; }
export function codexThreadId(meta: Record<string, unknown> = {}): string {
  let turn = meta["x-codex-turn-metadata"];
  if (typeof turn === "string") { try { turn = JSON.parse(turn); } catch { turn = undefined; } }
  const candidates = [...(record(turn) ? [turn.thread_id, turn.session_id] : []), meta["openai/threadId"], meta.codexThreadId];
  const id = candidates.find(nonempty);
  if (!id) throw new CitoError("BINDING_REQUIRED", "Codex thread metadata is required");
  return id;
}

/** Read only the caller's explicit thread, never guess from recency or focus. */
export function readCodexThread(threadId: string, config: CodexConfig = loadConfig(),
  options: ReadCodexThreadOptions = {}): CodexCaller {
  let db: DatabaseSync | undefined;
  try {
    db = new DatabaseSync(join(config.codexHome, "state_5.sqlite"), { readOnly: true });
    db.exec("PRAGMA busy_timeout = 1000");
    // SELECT * also supports older Desktop schemas without the optional name column.
    const row = db.prepare("SELECT * FROM threads WHERE id = ?").get(threadId);
    if (!row || (row.archived === 1 && !options.allowArchived) || !nonempty(row.cwd)) {
      throw new Error("Thread is absent, archived, or has no cwd");
    }
    return { harness: "codex", session_id: threadId, cwd: row.cwd,
      title: (typeof row.name === "string" && row.name.trim())
        || (typeof row.title === "string" && row.title.trim()) || basename(row.cwd),
      repo: normalizeRepo(typeof row.git_origin_url === "string" ? row.git_origin_url : null),
      branch: typeof row.git_branch === "string" ? row.git_branch : null };
  } catch (error) { throw new CitoError("SESSION_NOT_OPEN", "Codex thread is not open", { cause: error }); }
  finally { db?.close(); }
}

/** A single 1500ms budget includes connect, initialize, and owner discovery. */
export async function assertCodexThreadOpen(threadId: string, config: CodexConfig = loadConfig(), signal?: AbortSignal): Promise<void> {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const socket = createConnection(join(config.codexHome, "ipc", "ipc.sock"));
    let buffer = Buffer.alloc(0);
    let requestId = randomUUID();
    let phase: "initialize" | "discover" = "initialize";
    let settled = false;
    const finish = (error?: unknown) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      socket.destroy();
      if (error) reject(error); else resolve();
    };
    const closed = (cause: unknown) => finish(new CitoError("SESSION_NOT_OPEN", "Codex Desktop has no live thread owner", { cause }));
    const abort = () => finish(signal!.reason ?? new DOMException("Aborted", "AbortError"));
    const timer = setTimeout(() => closed(new Error("Codex IPC timed out")), 1500);
    signal?.addEventListener("abort", abort, { once: true });
    function send(value: object): void {
      const payload = Buffer.from(JSON.stringify(value));
      const header = Buffer.alloc(4);
      header.writeUInt32LE(payload.length);
      socket.write(Buffer.concat([header, payload]));
    }
    socket.on("connect", () => {
      if (signal?.aborted) { abort(); return; }
      send({ type: "request", version: 1, requestId, method: "initialize", params: { clientType: "cli" } });
    });
    socket.on("error", closed);
    socket.on("close", () => { if (!settled) closed(new Error("Codex IPC closed before response")); });
    socket.on("data", chunk => {
      if (settled) return;
      buffer = Buffer.concat([buffer, chunk]);
      try {
        while (buffer.length >= 4 && !settled) {
          const size = buffer.readUInt32LE(0);
          if (size === 0 || size > 4 * 1024 * 1024) throw new Error("Invalid Codex IPC frame length");
          if (buffer.length < size + 4) break;
          const response: unknown = JSON.parse(buffer.subarray(4, size + 4).toString("utf8"));
          buffer = buffer.subarray(size + 4);
          if (!record(response)) throw new Error("Invalid Codex response");
          if (response.type !== "response" || response.requestId !== requestId) continue;
          if (response.resultType !== "success") throw new Error("Codex IPC request rejected");
          const result = record(response.result) ? response.result : undefined;
          if (phase === "initialize") {
            if (!nonempty(result?.clientId)) throw new Error("Missing clientId");
            phase = "discover";
            requestId = randomUUID();
            send({ type: "request", version: 1, requestId, sourceClientId: result.clientId,
              method: "thread-owner-discovery", params: { hostId: "local", conversationId: threadId, threadId } });
          } else {
            // Success is authoritative unless the result explicitly reports no owner.
            if (result?.status === "no-client-found" || result?.ownerClientId === null || result?.ownerClientId === "") {
              throw new Error("No live owner");
            }
            finish();
          }
        }
      } catch (error) { closed(error); }
    });
  });
}
export async function resolveCodexCaller(meta: Record<string, unknown> = {}, config: CodexConfig = loadConfig()): Promise<CodexCaller> {
  const thread = readCodexThread(codexThreadId(meta), config);
  await assertCodexThreadOpen(thread.session_id, config);
  return thread;
}

/** The caller owns authorization and the listener lock around EACH separate effect. No retries here. */
export async function queueCodexWake(threadId: string, card: string, options: CodexDriverOptions = {}): Promise<void> {
  options.signal?.throwIfAborted();
  await exec(options.codexBin ?? loadConfig().codexBin, ["queue", "--thread", threadId, "--message", card],
    { timeout: 10_000, signal: options.signal });
}
export async function triggerCodexWake(threadId: string, options: CodexDriverOptions = {}): Promise<void> {
  options.signal?.throwIfAborted();
  await exec(options.openBin ?? "/usr/bin/open", ["-g", "codex://threads/wake/" + threadId],
    { timeout: 3000, signal: options.signal });
}
