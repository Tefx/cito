import { randomUUID } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { chmod, mkdir, open, readFile, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import { CitoError, expandHomeDir, loadConfig, type CitoConfig } from "../../src/shared/index.js";
import { hasExplicitPid, resolveExplicitCaller } from "../../src/adapters/git.js";
import { SocketBridgeAdapter, mutateConfigFile } from "../../src/adapters/socket-bridge.js";
import type {
  Caller,
  CallerDetectionContext,
  HarnessCliDescriptor,
  HarnessSetupDescriptor,
  PluginManifest,
  StdioChannelBridge,
} from "../../src/adapters/types.js";
import { startClaudeChannelBridge, type ClaudeChannelOptions } from "./channel.js";
export * from "./channel.js";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/claude/plugin.json", import.meta.url);
const claudeManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

export interface ClaudePluginConfig {
  claudeHome: string;
  enableClaudeChannel: boolean;
}
export interface ClaudeConfigInput {
  env?: NodeJS.ProcessEnv;
  plugins?: Record<string, Record<string, unknown>>;
  claudeHome?: string;
  enableClaudeChannel?: boolean;
}
type QueueConfig = Pick<CitoConfig, "stateDir">;
export const CLAUDE_HARNESS = "claude";
const TRANSCRIPT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// Session identifiers become file names: keep them to one path segment with no traversal.
const QUEUE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const DRAIN_SUFFIX = ".draining-";

export function loadClaudeConfig(config: ClaudeConfigInput = loadConfig()): ClaudePluginConfig {
  const pluginCfg = (config.plugins?.claude ?? {}) as {
    claudeHome?: unknown;
    enableClaudeChannel?: unknown;
  };
  const env = config.env ?? process.env;
  const rawHome = typeof pluginCfg.claudeHome === "string"
    ? pluginCfg.claudeHome
    : typeof config.claudeHome === "string"
    ? config.claudeHome
    : env.CITO_CLAUDE_HOME ?? "~/.claude";
  const enableClaudeChannel = typeof pluginCfg.enableClaudeChannel === "boolean"
    ? pluginCfg.enableClaudeChannel
    : typeof config.enableClaudeChannel === "boolean"
    ? config.enableClaudeChannel
    : ["1", "true", "yes"].includes((env.CITO_CLAUDE_CHANNEL ?? "").trim().toLowerCase());
  return {
    claudeHome: expandHomeDir(rawHome, "CITO_CLAUDE_HOME", env.HOME),
    enableClaudeChannel,
  };
}

function nonempty(value: unknown): value is string { return typeof value === "string" && value.trim().length > 0; }
export function validateClaudeSessionId(value: unknown): string {
  if (!nonempty(value) || !QUEUE_ID.test(value.trim())) throw new CitoError("INVALID_INPUT", "Invalid Claude session identifier");
  return value.trim();
}
/** Claude Code slugs the absolute workspace path into one transcript directory per workspace. */
export function claudeProjectDir(cwd: string, config: ClaudeConfigInput = loadConfig()): string {
  return join(loadClaudeConfig(config).claudeHome, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"));
}
/** The transcript Claude Code appends to names the session it is running; /clear starts a new one. */
export async function resolveClaudeSessionId(cwd: string, config: ClaudeConfigInput = loadConfig()): Promise<string | undefined> {
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
/**
 * Claude Code names a session with `custom-title` records appended to its transcript, so the newest
 * one is the title the user sees. Transcripts reach hundreds of megabytes, and `identify` answers
 * under a sub-second budget, so only the tail is read: a session titled once and never again since
 * falls back to the workspace name rather than making discovery pay for the whole file.
 */
const TITLE_TAIL_BYTES = 128 * 1024;
const TITLE_MAX_CHARS = 200;
const TITLE_CACHE_ENTRIES = 32;
const titleCache = new Map<string, { size: number; mtimeMs: number; title: string | undefined }>();
export async function resolveClaudeSessionTitle(cwd: string, sessionId: string,
  config: ClaudeConfigInput = loadConfig()): Promise<string | undefined> {
  if (!TRANSCRIPT_ID.test(sessionId)) return undefined;
  const path = join(claudeProjectDir(cwd, config), `${sessionId}.jsonl`);
  let info;
  try { info = await stat(path); } catch { return undefined; }
  if (!info.isFile()) return undefined;
  const cached = titleCache.get(path);
  if (cached && cached.size === info.size && cached.mtimeMs === info.mtimeMs) return cached.title;
  const start = Math.max(0, info.size - TITLE_TAIL_BYTES);
  let text: string;
  const file = await open(path, "r");
  try {
    const buffer = Buffer.alloc(Math.min(info.size, TITLE_TAIL_BYTES));
    const { bytesRead } = await file.read(buffer, 0, buffer.length, start);
    text = buffer.subarray(0, bytesRead).toString("utf8");
  } finally { await file.close(); }
  const lines = text.split("\n");
  // A non-zero start almost certainly lands mid-line, and a partial line is not parseable JSON.
  if (start > 0) lines.shift();
  let title: string | undefined;
  for (let i = lines.length - 1; i >= 0 && title === undefined; i--) {
    const line = lines[i]!;
    if (!line.includes('"custom-title"')) continue;
    try {
      const record: unknown = JSON.parse(line);
      if (typeof record !== "object" || record === null) continue;
      const row = record as Record<string, unknown>;
      if (row.type !== "custom-title" || !nonempty(row.customTitle)) continue;
      const cleaned = row.customTitle.replace(/[\x00-\x1f\x7f]/g, " ").trim();
      if (cleaned) title = [...cleaned].slice(0, TITLE_MAX_CHARS).join("");
    } catch { /* A truncated or malformed record is not a title. */ }
  }
  if (title === undefined && start > 0 && cached && info.size > cached.size) {
    title = cached.title;
  }
  if (titleCache.size >= TITLE_CACHE_ENTRIES && !titleCache.has(path)) titleCache.delete(titleCache.keys().next().value!);
  titleCache.set(path, { size: info.size, mtimeMs: info.mtimeMs, title });
  return title;
}
export async function resolveClaudeTranscriptMtimeMs(cwd: string, sessionId: string,
  config: ClaudeConfigInput = loadConfig()): Promise<number> {
  if (!TRANSCRIPT_ID.test(sessionId)) return 0;
  try {
    const info = await stat(join(claudeProjectDir(cwd, config), `${sessionId}.jsonl`));
    return info.isFile() && Number.isFinite(info.mtimeMs) ? Math.round(info.mtimeMs) : 0;
  } catch { return 0; }
}
/** Explicit session claims from tool metadata or an HTTP header, with the same conflict rules as Codex threads. */
export function claudeSessionClaim(meta: Record<string, unknown> = {},
  headers?: Record<string, string | string[] | undefined>): string | undefined {
  const header = headers?.["x-claude-session-id"];
  const hasPid = hasExplicitPid(meta);
  const candidates = [meta.claudeSessionId, meta.claude_session_id,
    !hasPid && meta.expected_harness === CLAUDE_HARNESS ? meta.expected_session_id : undefined,
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
export function claudeAliasPath(config: QueueConfig, sessionId: string): string {
  return join(config.stateDir, "claude", "aliases", validateClaudeSessionId(sessionId));
}
export async function writeClaudeSessionAlias(config: QueueConfig, sessionId: string, targetSessionId: string): Promise<void> {
  const path = claudeAliasPath(config, sessionId);
  const target = validateClaudeSessionId(targetSessionId);
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  await writeFile(path, target + "\n", { mode: 0o600 });
  await chmod(path, 0o600);
}
export async function readClaudeSessionAlias(config: QueueConfig, sessionId: string): Promise<string | undefined> {
  try {
    const target = validateClaudeSessionId(await readFile(claudeAliasPath(config, sessionId), "utf8"));
    return target.startsWith("claude-") && target !== sessionId ? target : undefined;
  } catch {
    return undefined;
  }
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
export async function peekClaudeWakes(config: QueueConfig, sessionId: string): Promise<ClaudeWake[]> {
  const validId = validateClaudeSessionId(sessionId);
  const alias = await readClaudeSessionAlias(config, validId);
  const sessions = alias ? [alias, validId] : [validId];
  const wakes: ClaudeWake[] = [], seen = new Set<string>();
  for (const id of sessions) {
    let text: string;
    try { text = await readFile(claudeWakeQueuePath(config, id), "utf8"); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") continue; throw error; }
    for (const wake of parseWakes(text)) {
      if (seen.has(wake.batch_id)) continue;
      seen.add(wake.batch_id); wakes.push(wake);
    }
  }
  return wakes;
}
/**
 * Rename-then-read: an appender racing the drain recreates the queue instead of losing a card, and a
 * drain that died before unlinking leaves its staging file behind for the next run to pick up.
 */
export async function drainClaudeWakes(config: QueueConfig, sessionId: string): Promise<ClaudeWake[]> {
  const validId = validateClaudeSessionId(sessionId);
  const alias = await readClaudeSessionAlias(config, validId);
  const sessions = alias ? [alias, validId] : [validId];
  const dir = wakeDir(config);
  for (const id of sessions) {
    const path = claudeWakeQueuePath(config, id);
    try { await rename(path, path + DRAIN_SUFFIX + randomUUID()); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  }
  const prefixes = sessions.map(id => basename(claudeWakeQueuePath(config, id)) + DRAIN_SUFFIX);
  let names: string[];
  try { names = await readdir(dir); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
  const staged = await Promise.all(names.filter(name => prefixes.some(p => name.startsWith(p))).map(async name => {
    const file = join(dir, name);
    try { return { file, at: (await stat(file)).mtimeMs, text: await readFile(file, "utf8") }; }
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

const CLAUDE_HOOK_EVENTS = ["Stop", "SessionStart", "UserPromptSubmit"] as const;

function hasClaudeHookCommand(entries: unknown[]): boolean {
  return entries.some(entry => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) return false;
    const hooks = (entry as { hooks?: unknown }).hooks;
    if (!Array.isArray(hooks)) return false;
    return hooks.some(
      h =>
        h &&
        typeof h === "object" &&
        !Array.isArray(h) &&
        (h as { type?: unknown }).type === "command" &&
        (h as { command?: unknown }).command === "cito-claude-hook",
    );
  });
}

function transformClaudeSettingsJson(
  existing: string | undefined,
  force: boolean,
): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  const desiredMcp = {
    command: "cito-mcp",
    args: [] as string[],
    env: { CITO_CLAUDE_CHANNEL: "1" },
  };
  const hookItem = {
    hooks: [{ type: "command", command: "cito-claude-hook" }],
  };

  if (existing === undefined) {
    const initial = {
      mcpServers: { cito: desiredMcp },
      hooks: {
        Stop: [hookItem],
        SessionStart: [hookItem],
        UserPromptSubmit: [hookItem],
      },
    };
    return {
      status: "created",
      nextContent: JSON.stringify(initial, null, 2) + "\n",
      detail: "Created Claude Code settings.json with cito-mcp and hooks",
    };
  }

  let parsed: unknown;
  try {
    parsed = existing.trim() ? JSON.parse(existing) : {};
  } catch {
    throw new Error("Invalid JSON in Claude settings.json");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Expected JSON object in Claude settings.json");
  }

  const obj = { ...(parsed as Record<string, unknown>) };
  const mcpServers =
    obj.mcpServers && typeof obj.mcpServers === "object" && !Array.isArray(obj.mcpServers)
      ? { ...(obj.mcpServers as Record<string, unknown>) }
      : {};
  const existingCito = mcpServers.cito as { command?: unknown; args?: unknown; env?: Record<string, unknown> } | undefined;
  let mcpAlreadyOk = false;
  if (existingCito && typeof existingCito === "object" && !Array.isArray(existingCito)) {
    if (existingCito.command !== undefined && existingCito.command !== "cito-mcp" && !force) {
      return {
        status: "conflict",
        detail: "Conflicting mcpServers.cito command in Claude settings.json; pass --force to replace",
      };
    }
    mcpAlreadyOk =
      existingCito.command === "cito-mcp" &&
      Array.isArray(existingCito.args) &&
      existingCito.args.length === 0 &&
      existingCito.env?.CITO_CLAUDE_CHANNEL === "1";
  }

  const hooksObj =
    obj.hooks && typeof obj.hooks === "object" && !Array.isArray(obj.hooks)
      ? { ...(obj.hooks as Record<string, unknown>) }
      : {};
  let hooksAlreadyOk = true;
  for (const eventName of CLAUDE_HOOK_EVENTS) {
    const currentList = Array.isArray(hooksObj[eventName]) ? [...(hooksObj[eventName] as unknown[])] : [];
    if (!hasClaudeHookCommand(currentList)) {
      hooksAlreadyOk = false;
      currentList.push(hookItem);
    }
    hooksObj[eventName] = currentList;
  }

  if (mcpAlreadyOk && hooksAlreadyOk) {
    return {
      status: "unchanged",
      detail: "Claude Code settings.json already configured",
    };
  }

  mcpServers.cito = desiredMcp;
  obj.mcpServers = mcpServers;
  obj.hooks = hooksObj;

  return {
    status: "updated",
    nextContent: JSON.stringify(obj, null, 2) + "\n",
    detail: "Updated Claude Code settings.json with cito-mcp and hooks",
  };
}

function isClaudeHookCmd(h: unknown): boolean {
  if (!h || typeof h !== "object" || Array.isArray(h)) return false;
  const rec = h as { type?: unknown; command?: unknown };
  return rec.type === "command" && typeof rec.command === "string" && (rec.command === "cito-claude-hook" || rec.command.endsWith("/cito-claude-hook"));
}

function removeClaudeSettingsJson(
  existing: string | undefined,
): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  if (existing === undefined) {
    return {
      status: "unchanged",
      detail: "Claude settings.json does not exist",
    };
  }
  let parsed: unknown;
  try {
    parsed = existing.trim() ? JSON.parse(existing) : {};
  } catch {
    throw new Error("Invalid JSON in Claude settings.json");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Expected JSON object in Claude settings.json");
  }
  const obj = { ...(parsed as Record<string, unknown>) };
  let changed = false;

  if (obj.mcpServers && typeof obj.mcpServers === "object" && !Array.isArray(obj.mcpServers)) {
    const mcpServers = { ...(obj.mcpServers as Record<string, unknown>) };
    if ("cito" in mcpServers) {
      delete mcpServers.cito;
      changed = true;
      if (Object.keys(mcpServers).length > 0) {
        obj.mcpServers = mcpServers;
      } else {
        delete obj.mcpServers;
      }
    }
  }

  if (obj.hooks && typeof obj.hooks === "object" && !Array.isArray(obj.hooks)) {
    const hooksObj = { ...(obj.hooks as Record<string, unknown>) };
    for (const [eventName, rawList] of Object.entries(hooksObj)) {
      if (!Array.isArray(rawList)) continue;
      const nextGroups: unknown[] = [];
      for (const entry of rawList) {
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
          nextGroups.push(entry);
          continue;
        }
        const group = { ...(entry as Record<string, unknown>) };
        if (Array.isArray(group.hooks)) {
          const filteredHooks = group.hooks.filter(h => !isClaudeHookCmd(h));
          if (filteredHooks.length !== group.hooks.length) {
            changed = true;
          }
          if (filteredHooks.length > 0) {
            group.hooks = filteredHooks;
            nextGroups.push(group);
          }
        } else {
          nextGroups.push(group);
        }
      }
      if (nextGroups.length > 0) {
        hooksObj[eventName] = nextGroups;
      } else {
        delete hooksObj[eventName];
      }
    }
    if (Object.keys(hooksObj).length > 0) {
      obj.hooks = hooksObj;
    } else {
      delete obj.hooks;
    }
  }

  if (!changed) {
    return {
      status: "unchanged",
      detail: "Claude settings.json has no Cito entries",
    };
  }
  return {
    status: "updated",
    nextContent: JSON.stringify(obj, null, 2) + "\n",
    detail: "Removed Cito mcpServers and hooks from Claude settings.json",
  };
}

export class ClaudeCodeAdapter extends SocketBridgeAdapter {
  override readonly setup: HarnessSetupDescriptor = {
    inspect: async (env: NodeJS.ProcessEnv) => {
      const claudeHome = expandHomeDir(env.CITO_CLAUDE_HOME ?? "~/.claude", "CITO_CLAUDE_HOME", env.HOME);
      const settingsPath = join(claudeHome, "settings.json");
      const installed = existsSync(claudeHome) || existsSync(settingsPath);
      let configured = false;
      if (existsSync(settingsPath)) {
        try {
          configured = transformClaudeSettingsJson(readFileSync(settingsPath, "utf8"), false).status === "unchanged";
        } catch {}
      }
      return { installed, configured, paths: [settingsPath] };
    },
    plan: async (env: NodeJS.ProcessEnv, options: { force?: boolean }) => {
      const claudeHome = expandHomeDir(env.CITO_CLAUDE_HOME ?? "~/.claude", "CITO_CLAUDE_HOME", env.HOME);
      const settingsPath = join(claudeHome, "settings.json");
      return [
        await mutateConfigFile({
          target: CLAUDE_HARNESS,
          path: settingsPath,
          dryRun: true,
          transform: content => transformClaudeSettingsJson(content, Boolean(options.force)),
        }),
      ];
    },
    apply: async (env: NodeJS.ProcessEnv, options: { force?: boolean }) => {
      const claudeHome = expandHomeDir(env.CITO_CLAUDE_HOME ?? "~/.claude", "CITO_CLAUDE_HOME", env.HOME);
      const settingsPath = join(claudeHome, "settings.json");
      return [
        await mutateConfigFile({
          target: CLAUDE_HARNESS,
          path: settingsPath,
          dryRun: false,
          transform: content => transformClaudeSettingsJson(content, Boolean(options.force)),
        }),
      ];
    },
    remove: async (env: NodeJS.ProcessEnv) => {
      const claudeHome = expandHomeDir(env.CITO_CLAUDE_HOME ?? "~/.claude", "CITO_CLAUDE_HOME", env.HOME);
      const settingsPath = join(claudeHome, "settings.json");
      const rootClaudeJson = join(dirname(claudeHome), ".claude.json");
      const actions = [
        await mutateConfigFile({
          target: CLAUDE_HARNESS,
          path: settingsPath,
          dryRun: false,
          transform: content => removeClaudeSettingsJson(content),
        }),
      ];
      if (existsSync(rootClaudeJson)) {
        actions.push(
          await mutateConfigFile({
            target: CLAUDE_HARNESS,
            path: rootClaudeJson,
            dryRun: false,
            transform: content => removeClaudeSettingsJson(content),
          }),
        );
      }
      return actions;
    },
  };
  override readonly cli: HarnessCliDescriptor = {
    consumesNextArg: (cmd, nextArg) => {
      if (cmd !== "unlisten" && !TRANSCRIPT_ID.test(nextArg) && !/^claude-[1-9][0-9]*$/i.test(nextArg)) {
        return false;
      }
      validateClaudeSessionId(nextArg);
      return true;
    },
    resolveMeta: async (_cmd, explicitId, cwd, config) => {
      let sessionId = explicitId ? validateClaudeSessionId(explicitId) : undefined;
      if (!sessionId) {
        const envId = (config.env?.CLAUDE_SESSION_ID ?? process.env.CLAUDE_SESSION_ID)?.trim();
        if (envId) sessionId = validateClaudeSessionId(envId);
      }
      if (!sessionId) {
        const live = await this.discoverAvailableSessions(config);
        sessionId = live.find(s => s.cwd === cwd)?.session_id;
      }
      if (!sessionId) {
        sessionId = await resolveClaudeSessionId(cwd, config);
      }
      if (!sessionId) {
        throw new CitoError("BINDING_REQUIRED", "Claude session metadata is required");
      }
      return { claudeSessionId: sessionId, cwd };
    },
  };

  constructor(manifest: PluginManifest = claudeManifest) {
    // A queued card only reaches the model at the next hook boundary, so it is never proof of a read.
    super(CLAUDE_HARNESS, { confirmDelivery: false, manifest });
  }
  async startStdioChannel(
    config: CitoConfig,
    transport: { send(message: JSONRPCMessage): Promise<void> },
    options: ClaudeChannelOptions = {},
  ): Promise<StdioChannelBridge | undefined> {
    if (!loadClaudeConfig(config).enableClaudeChannel) return undefined;
    return startClaudeChannelBridge(config, transport, options);
  }
  /**
   * Session liveness stays with the socket bridge: a transcript on disk outlives the process that
   * wrote it, so a missing or stale transcript never proves a session closed.
   */
  async detectExplicitCaller(ctx: CallerDetectionContext, _config: CitoConfig): Promise<Caller | null> {
    const session = claudeSessionClaim(ctx.meta, ctx.headers);
    if (!session) return null;
    if (hasExplicitPid(ctx.meta)) {
      throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    }
    return resolveExplicitCaller(CLAUDE_HARNESS, session, ctx);
  }
}

export const claudeAdapter = new ClaudeCodeAdapter(claudeManifest);
export default (manifest: PluginManifest) => new ClaudeCodeAdapter(manifest);
