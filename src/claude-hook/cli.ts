import { readFile } from "node:fs/promises";
import { loadConfig, type CitoConfig } from "../shared/index.js";
import {
  claudeBadgePath, drainClaudeWakes, readClaudeSessionAlias, resolveClaudeSessionId,
  validateClaudeSessionId, writeClaudeBadge, type ClaudeWake,
} from "../adapters/claude.js";

/** Only events that can carry text back into the turn may drain the queue; others must leave it intact. */
const INJECTABLE = new Set(["Stop", "SessionStart", "UserPromptSubmit"]);
const USAGE = "Usage: cito-claude-hook (reads a Claude Code hook payload on stdin)\n"
  + "Install on Stop, SessionStart and UserPromptSubmit to deliver queued Cito wakes.\n";

export interface HookInput {
  session_id?: unknown;
  hook_event_name?: unknown;
  cwd?: unknown;
  stop_hook_active?: unknown;
}
export function parseHookInput(text: string): HookInput {
  if (!text.trim()) return {};
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected a hook payload object");
  return value as HookInput;
}
export function renderWakes(wakes: readonly ClaudeWake[]): string {
  return wakes.map(wake => wake.content).join("\n\n");
}
/**
 * `Stop` is the only boundary that can restart an idle turn, so a wake blocks it with the card as the
 * reason. The context events can only prepend text to a turn the user already started.
 */
export function hookOutput(event: string, text: string): Record<string, unknown> {
  if (event === "Stop") return { decision: "block", reason: text };
  return { hookSpecificOutput: { hookEventName: event, additionalContext: text } };
}
async function readStdin(): Promise<string> {
  if (process.stdin.isTTY) return "";
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}
async function stripBadgeUnreadCount(config: CitoConfig, sessionId: string): Promise<void> {
  try {
    const current = (await readFile(claudeBadgePath(config, sessionId), "utf8")).replace(/\r?\n$/, "");
    const stripped = current.replace(/ \(\d+\)$/, "");
    if (stripped && stripped !== current) await writeClaudeBadge(config, sessionId, stripped);
  } catch { /* Best-effort badge update when daemon is unreachable. */ }
}
export async function main(argv: string[] = process.argv.slice(2), config: CitoConfig = loadConfig()): Promise<void> {
  if (argv.includes("--help") || argv.includes("-h")) { process.stderr.write(USAGE); return; }
  const emit = (value: Record<string, unknown>) => { process.stdout.write(JSON.stringify(value) + "\n"); };
  try {
    const input = parseHookInput(await readStdin());
    const event = typeof input.hook_event_name === "string" && input.hook_event_name.trim()
      ? input.hook_event_name.trim() : argv[0]?.trim() ?? "";
    if (!INJECTABLE.has(event)) { emit({}); return; }
    const cwd = typeof input.cwd === "string" && input.cwd.trim() ? input.cwd.trim() : process.cwd();
    const explicitSession = typeof input.session_id === "string" && input.session_id.trim()
      ? input.session_id
      : process.env.CLAUDE_SESSION_ID?.trim() || undefined;
    // The hook payload or CLAUDE_SESSION_ID names the live session; a manual run falls back to the newest transcript.
    const session = explicitSession
      ? validateClaudeSessionId(explicitSession)
      : await resolveClaudeSessionId(cwd, config);
    if (!session) { emit({}); return; }
    const wakes = await drainClaudeWakes(config, session);
    if (wakes.length) {
      const alias = await readClaudeSessionAlias(config, session);
      const ackSession = alias ?? session;
      await Promise.allSettled([
        stripBadgeUnreadCount(config, session),
        ...(alias ? [stripBadgeUnreadCount(config, alias)] : []),
      ]);
      await fetch(`http://127.0.0.1:${config.daemonPort}/mcp`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
        },
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "tools/call",
          params: {
            name: "cito_inbox",
            arguments: { ack_all: false, ack_ids: wakes.map(w => w.batch_id) },
            _meta: { claudeSessionId: ackSession, cwd },
          },
        }),
        signal: AbortSignal.timeout(500),
      }).then(res => res.text()).catch(() => {});
      emit(hookOutput(event, renderWakes(wakes)));
      return;
    }
    emit({});
  } catch (error) {
    // A hook failure must never break the user's session: report on stderr and stay neutral.
    process.stderr.write("cito-claude-hook: " + (error instanceof Error ? error.message : String(error)) + "\n");
    emit({});
  }
}
