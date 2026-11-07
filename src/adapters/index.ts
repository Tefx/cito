import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { register } from "node:module";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { CitoError, harnessIdSchema, loadConfig, type CitoConfig } from "../shared/index.js";
import { HUMAN_CHANNEL_REGISTRY, getHumanChannelPlugin, getHumanChannelPlugins, rejectedHumanPluginIds } from "../hub/engine.js";
import { BUILTIN_HUMAN_PLUGINS, BUILTIN_PLUGINS } from "../../plugins/index.js";
import { BASE_PID_KEYS, getAllPidKeys, gitMetadata, hasExplicitPid, registerPidKeys, resetPidKeys } from "./git.js";
import { SocketBridgeAdapter, defaultCliDescriptor, identifyBridge } from "./socket-bridge.js";
import type {
  AdapterTier,
  AdapterTint,
  AvailableSessionSummary,
  Caller,
  CallerDetectionContext,
  CallerInput,
  HarnessAdapter,
  HumanChannelPlugin,
  PluginFactory,
  PluginManifest,
  StdioChannelBridge,
  UiHarnessMetadata,
} from "./types.js";

export * from "../../plugins/index.js";
export * from "./git.js";
export * from "./socket-bridge.js";
export * from "./types.js";
export * from "./wake-card.js";
export { CitoError } from "../shared/index.js";

const ADAPTER_TIERS = ["stable", "experimental", "custom"] as const satisfies readonly AdapterTier[];
const ADAPTER_TINTS = ["indigo", "teal", "orange", "blue", "purple", "green", "pink", "gray"] as const satisfies readonly AdapterTint[];

const pluginFieldSchema = z.object({
  key: z.string().trim().min(1).max(64).regex(/^[a-zA-Z][a-zA-Z0-9_]*$/),
  scope: z.enum(["hub", "user"]),
  label: z.string().trim().min(1).max(64),
  type: z.enum(["string", "secret", "url", "boolean"]),
  required: z.boolean().optional(),
  placeholder: z.string().max(128).optional(),
}).strict();

const pluginManifestSchema = z.object({
  id: harnessIdSchema,
  kind: z.enum(["agent", "human"]).optional(),
  displayName: z.string().trim().min(1).max(64).refine(v => !/[\x00-\x1f\x7f]/.test(v)),
  tier: z.enum(ADAPTER_TIERS).optional(),
  sfSymbol: z.string().trim().min(1).max(64).optional(),
  tint: z.enum(ADAPTER_TINTS).optional(),
  cliFlag: z.boolean().optional(),
  fields: z.array(pluginFieldSchema).optional(),
  pairing: z.object({
    supported: z.boolean(),
    actionLabel: z.string().trim().min(1).max(64).optional(),
  }).strict().optional(),
}).strict();

export const RESERVED_CLI_FLAGS = new Set([
  "hub", "team", "scope", "tag", "harness", "session", "help", "version", "setup",
]);

export const BUILTIN_PLUGIN_IDS = new Set<string>(BUILTIN_PLUGINS.map(p => p.id));
export const BUILTIN_HUMAN_PLUGIN_IDS = new Set<string>(BUILTIN_HUMAN_PLUGINS.map(p => p.id));
const BUILTIN_ORDER = new Map<string, number>(BUILTIN_PLUGINS.map((p, index) => [p.id, index]));

const directBuiltinDir = resolve(fileURLToPath(new URL("../../plugins", import.meta.url)));
const distBuiltinDir = resolve(fileURLToPath(new URL("../../../plugins", import.meta.url)));

export { HUMAN_CHANNEL_REGISTRY, getHumanChannelPlugin, getHumanChannelPlugins };
export const ADAPTER_REGISTRY = new Map<string, HarnessAdapter>();
const builtinHumanAdapters = new Map<string, HarnessAdapter>();
export const rejectedPluginIds = rejectedHumanPluginIds;
const externalPluginIds = new Set<string>();
const externalHumanPluginIds = new Set<string>();
const autoInstantiatedIds = new Set<string>();

export function registerAdapter(adapter: HarnessAdapter, options?: { allowOverride?: boolean }): void {
  if (!harnessIdSchema.safeParse(adapter.id).success) {
    throw new CitoError("UNSUPPORTED_HARNESS", "Invalid adapter id");
  }
  if ((BUILTIN_PLUGIN_IDS.has(adapter.id) || BUILTIN_HUMAN_PLUGIN_IDS.has(adapter.id)) && ADAPTER_REGISTRY.has(adapter.id) && !options?.allowOverride) {
    throw new CitoError("INVALID_INPUT", `Cannot override reserved built-in plugin '${adapter.id}'`);
  }
  if (adapter.pidKeys) registerPidKeys(adapter.pidKeys);
  rejectedPluginIds.delete(adapter.id);
  autoInstantiatedIds.delete(adapter.id);
  ADAPTER_REGISTRY.set(adapter.id, adapter);
}

export function registerHumanChannelPlugin(plugin: HumanChannelPlugin, options?: { allowOverride?: boolean }): void {
  if (!harnessIdSchema.safeParse(plugin.id).success) {
    throw new CitoError("UNSUPPORTED_HARNESS", "Invalid human channel plugin id");
  }
  if ((BUILTIN_PLUGIN_IDS.has(plugin.id) || BUILTIN_HUMAN_PLUGIN_IDS.has(plugin.id)) && HUMAN_CHANNEL_REGISTRY.has(plugin.id) && !options?.allowOverride) {
    throw new CitoError("INVALID_INPUT", `Cannot override reserved built-in plugin '${plugin.id}'`);
  }
  rejectedPluginIds.delete(plugin.id);
  HUMAN_CHANNEL_REGISTRY.set(plugin.id, plugin);
}

for (const plugin of BUILTIN_PLUGINS) {
  registerAdapter(plugin, { allowOverride: true });
}
for (const humanPlugin of BUILTIN_HUMAN_PLUGINS) {
  registerHumanChannelPlugin(humanPlugin, { allowOverride: true });
  if ("probe" in humanPlugin && typeof humanPlugin.probe === "function" && "deliver" in humanPlugin && typeof humanPlugin.deliver === "function") {
    builtinHumanAdapters.set(humanPlugin.id, humanPlugin as unknown as HarnessAdapter);
  }
}

function ensurePluginLoaderHook(): void {
  const g = globalThis as { __citoPluginHookRegistered?: boolean };
  if (g.__citoPluginHookRegistered) return;
  g.__citoPluginHookRegistered = true;
  const origEmitWarning = process.emitWarning;
  process.emitWarning = ((warning: string | Error, ...args: unknown[]) => {
    const msg = typeof warning === "string" ? warning : warning?.message ?? "";
    if (msg.includes("stripTypeScriptTypes") || msg.includes("module.register()")) return;
    return (origEmitWarning as (...a: unknown[]) => void).call(process, warning, ...args);
  }) as typeof process.emitWarning;
  const selfUrl = import.meta.url.startsWith("file://") ? import.meta.url : pathToFileURL(import.meta.url).href;
  const hookSource = `import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { fileURLToPath } from "node:url";
process.emitWarning = () => {};
const SDK_URL = ${JSON.stringify(selfUrl)};
export async function resolve(specifier, context, nextResolve) {
  if (specifier === "@tefx/cito/plugin-sdk") {
    return { url: SDK_URL, shortCircuit: true };
  }
  try {
    return await nextResolve(specifier, context);
  } catch (error) {
    if (error?.code === "ERR_MODULE_NOT_FOUND" && /^\\.\\.?\\/.*\\.js$/.test(specifier)) {
      return nextResolve(specifier.slice(0, -3) + ".ts", context);
    }
    throw error;
  }
}
export async function load(url, context, nextLoad) {
  const cleanUrl = url.split("?")[0];
  if (cleanUrl.endsWith(".ts")) {
    const source = await readFile(fileURLToPath(cleanUrl), "utf8");
    return {
      format: "module",
      shortCircuit: true,
      source: stripTypeScriptTypes(source, { mode: "strip", sourceUrl: cleanUrl }),
    };
  }
  return nextLoad(url, context);
}`;
  register(`data:text/javascript,${encodeURIComponent(hookSource)}`, selfUrl);
}

let lastPluginFingerprint = "";

async function computePluginFingerprint(pluginDirs: readonly string[]): Promise<string> {
  const parts: string[] = [];
  for (const rawDir of pluginDirs) {
    const dir = resolve(rawDir);
    if (dir === directBuiltinDir || dir === distBuiltinDir) continue;
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      parts.push(`${dir}:missing`);
      continue;
    }
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const sub = join(dir, entry.name);
      const mStat = await stat(join(sub, "plugin.json")).catch(() => undefined);
      const jStat = await stat(join(sub, "index.js")).catch(() => undefined);
      const tStat = await stat(join(sub, "index.ts")).catch(() => undefined);
      parts.push(`${sub}:${mStat?.mtimeMs ?? -1}:${jStat?.mtimeMs ?? -1}:${tStat?.mtimeMs ?? -1}`);
    }
  }
  return parts.join("|");
}

function isReservedBuiltin(id: string): boolean {
  return BUILTIN_PLUGIN_IDS.has(id) || BUILTIN_HUMAN_PLUGIN_IDS.has(id);
}

export async function loadPlugins(
  config: CitoConfig = loadConfig(),
  options?: { diagnostic?: (message: string) => void },
): Promise<void> {
  const report = options?.diagnostic ?? ((msg: string) => { process.stderr.write(`cito-plugin: ${msg}\n`); });
  for (const id of externalPluginIds) {
    ADAPTER_REGISTRY.delete(id);
  }
  externalPluginIds.clear();
  for (const id of externalHumanPluginIds) {
    HUMAN_CHANNEL_REGISTRY.delete(id);
  }
  externalHumanPluginIds.clear();
  for (const id of autoInstantiatedIds) {
    ADAPTER_REGISTRY.delete(id);
  }
  autoInstantiatedIds.clear();
  rejectedPluginIds.clear();
  resetPidKeys([...ADAPTER_REGISTRY.values()].flatMap(a => a.pidKeys ?? []));

  const seenExternal = new Set<string>();
  const pluginDirs = config.pluginDirs ?? [join(config.stateDir, "plugins")];
  for (const rawRoot of pluginDirs) {
    const rootDir = resolve(rawRoot);
    if (rootDir === directBuiltinDir || rootDir === distBuiltinDir) continue;
    let entries;
    try {
      entries = await readdir(rootDir, { withFileTypes: true });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw error;
    }
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const dirName = entry.name;
      const pluginDir = join(rootDir, dirName);
      const manifestPath = join(pluginDir, "plugin.json");
      let rawJson: unknown;
      try {
        rawJson = JSON.parse(await readFile(manifestPath, "utf8"));
      } catch (error) {
        if (!isReservedBuiltin(dirName)) rejectedPluginIds.add(dirName);
        report(`Invalid or missing plugin.json in ${pluginDir}: ${error instanceof Error ? error.message : String(error)}`);
        continue;
      }
      const parsed = pluginManifestSchema.safeParse(rawJson);
      if (!parsed.success) {
        if (!isReservedBuiltin(dirName)) rejectedPluginIds.add(dirName);
        if (rawJson && typeof rawJson === "object" && "id" in rawJson && typeof (rawJson as { id?: unknown }).id === "string") {
          const rawId = (rawJson as { id: string }).id;
          if (!isReservedBuiltin(rawId)) rejectedPluginIds.add(rawId);
        }
        report(`Invalid plugin manifest in ${manifestPath}: ${parsed.error.message}`);
        continue;
      }
      const rawManifest = parsed.data;
      if (rawManifest.id !== dirName) {
        if (!isReservedBuiltin(dirName)) rejectedPluginIds.add(dirName);
        if (!isReservedBuiltin(rawManifest.id)) rejectedPluginIds.add(rawManifest.id);
        report(`Plugin id '${rawManifest.id}' in ${manifestPath} does not match directory name '${dirName}'`);
        continue;
      }
      if (isReservedBuiltin(rawManifest.id)) {
        report(`External plugin in ${pluginDir} cannot override reserved built-in plugin '${rawManifest.id}'`);
        continue;
      }
      if (seenExternal.has(rawManifest.id)) {
        report(`Duplicate external plugin '${rawManifest.id}' in ${pluginDir}; keeping earlier registration`);
        continue;
      }
      let cliFlag = rawManifest.cliFlag ?? false;
      if (cliFlag && RESERVED_CLI_FLAGS.has(rawManifest.id)) {
        report(`Plugin '${rawManifest.id}' cannot register reserved CLI flag '--${rawManifest.id}'`);
        cliFlag = false;
      }
      const manifest: PluginManifest = {
        ...rawManifest,
        tier: "custom",
        cliFlag,
      };
      const jsPath = join(pluginDir, "index.js");
      const tsPath = join(pluginDir, "index.ts");
      const entryPath = existsSync(jsPath) ? jsPath : existsSync(tsPath) ? tsPath : undefined;
      if (entryPath) {
        try {
          ensurePluginLoaderHook();
          const st = await stat(entryPath);
          let mod: { default?: PluginFactory };
          try {
            mod = await import(`${pathToFileURL(entryPath).href}?mtime=${Math.round(st.mtimeMs)}`) as { default?: PluginFactory };
          } catch (importErr) {
            if (!String(importErr).includes("@tefx/cito/plugin-sdk")) throw importErr;
            const sdkPath = fileURLToPath(new URL("./index.js", import.meta.url));
            const rawSrc = await readFile(entryPath, "utf8");
            const rewritten = rawSrc.replace(/(['"])@tefx\/cito\/plugin-sdk\1/g, JSON.stringify(sdkPath));
            const tempEntry = join(pluginDir, `.cito-entry-${process.pid}-${Date.now()}${entryPath.endsWith(".ts") ? ".ts" : ".js"}`);
            await writeFile(tempEntry, rewritten, "utf8");
            try {
              mod = await import(pathToFileURL(tempEntry).href) as { default?: PluginFactory };
            } finally {
              await rm(tempEntry, { force: true });
            }
          }
          const exported = mod.default;
          const loaded = typeof exported === "function" ? await exported(manifest) : exported;
          if (manifest.kind === "human") {
            const hPlugin = loaded as HumanChannelPlugin | undefined;
            if (!hPlugin || typeof hPlugin !== "object" || hPlugin.id !== manifest.id || typeof hPlugin.sendNotification !== "function") {
              throw new Error(`Entry module must default-export a HumanChannelPlugin with id '${manifest.id}'`);
            }
            Object.defineProperty(hPlugin, "manifest", { value: manifest, enumerable: true, configurable: true });
            HUMAN_CHANNEL_REGISTRY.set(manifest.id, hPlugin);
            externalHumanPluginIds.add(manifest.id);
            seenExternal.add(manifest.id);
          } else {
            const adapter = loaded as HarnessAdapter | undefined;
            if (
              !adapter ||
              typeof adapter !== "object" ||
              adapter.id !== manifest.id ||
              typeof adapter.probe !== "function" ||
              typeof adapter.deliver !== "function"
            ) {
              throw new Error(`Entry module must default-export a HarnessAdapter with id '${manifest.id}'`);
            }
            Object.defineProperty(adapter, "manifest", { value: manifest, enumerable: true, configurable: true });
            if (manifest.cliFlag && !adapter.cli) {
              Object.defineProperty(adapter, "cli", { value: defaultCliDescriptor(manifest.id), enumerable: true, configurable: true });
            }
            if (adapter.pidKeys) registerPidKeys(adapter.pidKeys);
            ADAPTER_REGISTRY.set(manifest.id, adapter);
            externalPluginIds.add(manifest.id);
            seenExternal.add(manifest.id);
          }
        } catch (error) {
          rejectedPluginIds.add(manifest.id);
          ADAPTER_REGISTRY.delete(manifest.id);
          HUMAN_CHANNEL_REGISTRY.delete(manifest.id);
          report(`Failed to load plugin '${manifest.id}' from ${entryPath}: ${error instanceof Error ? error.message : String(error)}`);
        }
      } else if (manifest.kind !== "human") {
        const adapter = new SocketBridgeAdapter(manifest.id, { manifest });
        ADAPTER_REGISTRY.set(manifest.id, adapter);
        externalPluginIds.add(manifest.id);
        seenExternal.add(manifest.id);
      }
    }
  }
  lastPluginFingerprint = await computePluginFingerprint(pluginDirs);
}

export async function ensurePluginsLoaded(config: CitoConfig = loadConfig()): Promise<void> {
  const pluginDirs = config.pluginDirs ?? [join(config.stateDir, "plugins")];
  const fp = await computePluginFingerprint(pluginDirs);
  if (fp === lastPluginFingerprint) return;
  await loadPlugins(config);
}

function getHarnessOrder(harness: string): number {
  return BUILTIN_ORDER.get(harness) ?? BUILTIN_PLUGINS.length;
}

export function getUiHarnessMetadata(): UiHarnessMetadata[] {
  const items: UiHarnessMetadata[] = [];
  for (const adapter of ADAPTER_REGISTRY.values()) {
    if (rejectedPluginIds.has(adapter.id)) continue;
    const m = adapter.manifest;
    items.push({
      id: adapter.id,
      display_name: m?.displayName ?? (adapter.id.charAt(0).toUpperCase() + adapter.id.slice(1)),
      sf_symbol: m?.sfSymbol ?? "bolt.horizontal.fill",
      tint: m?.tint ?? "blue",
      tier: BUILTIN_PLUGIN_IDS.has(adapter.id) ? (m?.tier ?? "stable") : "custom",
      order: getHarnessOrder(adapter.id),
    });
  }
  items.sort((a, b) => a.order - b.order || a.id.localeCompare(b.id));
  return items;
}



export function getAdapter(harness: string): HarnessAdapter | undefined {
  if (!harnessIdSchema.safeParse(harness).success || rejectedPluginIds.has(harness)) return;
  const builtinHuman = builtinHumanAdapters.get(harness);
  if (builtinHuman) return builtinHuman;
  if (!ADAPTER_REGISTRY.has(harness)) {
    const fallback = new SocketBridgeAdapter(harness, {
      manifest: {
        id: harness,
        displayName: harness.charAt(0).toUpperCase() + harness.slice(1),
        tier: "custom",
        sfSymbol: "bolt.horizontal.fill",
        tint: "blue",
      },
    });
    ADAPTER_REGISTRY.set(harness, fallback);
    autoInstantiatedIds.add(harness);
  }
  return ADAPTER_REGISTRY.get(harness);
}

async function discoverAdapterSessions(
  adapter: HarnessAdapter,
  config: CitoConfig,
  signal?: AbortSignal,
): Promise<AvailableSessionSummary[]> {
  if (!adapter.discoverAvailableSessions) return [];
  const timeoutSignal = AbortSignal.timeout(1500);
  const combinedSignal = signal ? AbortSignal.any([signal, timeoutSignal]) : timeoutSignal;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      adapter.discoverAvailableSessions(config, combinedSignal),
      new Promise<AvailableSessionSummary[]>(resolve => {
        timer = setTimeout(() => resolve([]), 1500);
        timer.unref?.();
      }),
    ]);
  } catch {
    return [];
  } finally {
    clearTimeout(timer);
  }
}

export async function discoverAllAvailableSessions(
  config: CitoConfig = loadConfig(),
  activeListeners: readonly { harness: string; session_id: string; endpoint_pid?: number | null; hub?: string }[] = [],
  signal?: AbortSignal,
): Promise<AvailableSessionSummary[]> {
  await ensurePluginsLoaded(config);
  try {
    const names = await readdir(join(config.stateDir, "sockets"));
    for (const name of names) {
      const match = /^([a-z][a-z0-9_-]{0,31})-pid-[1-9][0-9]*\.sock$/.exec(name);
      if (match && !rejectedPluginIds.has(match[1]!)) getAdapter(match[1]!);
    }
  } catch { /* Missing sockets dir is normal. */ }
  const settled = await Promise.allSettled(
    [...ADAPTER_REGISTRY.values()].map(adapter => discoverAdapterSessions(adapter, config, signal)),
  );
  const raw = settled.flatMap(r => r.status === "fulfilled" ? r.value : []);
  const filtered = filterAvailableSessions(raw, activeListeners, Object.keys(config.hubs).length);
  const byHarness = new Map<string, AvailableSessionSummary[]>();
  for (const c of filtered) {
    let list = byHarness.get(c.harness);
    if (!list) { list = []; byHarness.set(c.harness, list); }
    list.push(c);
  }
  const harnesses = [...byHarness.keys()].sort((a, b) => getHarnessOrder(a) - getHarnessOrder(b) || a.localeCompare(b));
  const result: AvailableSessionSummary[] = [];
  for (const harness of harnesses) {
    const group = byHarness.get(harness)!;
    if (group.some(item => item.endpoint_pid !== undefined)) {
      group.sort((a, b) => (b.updated_at_ms ?? 0) - (a.updated_at_ms ?? 0) || (b.endpoint_pid ?? 0) - (a.endpoint_pid ?? 0));
    }
    for (const item of group.slice(0, 8)) {
      result.push(item);
    }
  }
  return result;
}

export function filterAvailableSessions(
  candidates: readonly AvailableSessionSummary[],
  activeListeners: readonly { harness: string; session_id: string; endpoint_pid?: number | null; hub?: string }[],
  totalHubs: number,
): AvailableSessionSummary[] {
  const hubCount = Math.max(1, totalHubs);
  const seen = new Set<string>();
  return candidates.flatMap(c => {
    const matching = activeListeners.filter(r =>
      r.harness === c.harness && (
        r.session_id === c.session_id
        || (c.endpoint_pid !== undefined && r.endpoint_pid != null && r.endpoint_pid === c.endpoint_pid)
      ),
    );
    if (matching.some(r => r.hub === undefined)) return [];
    const hubs = [...new Set(matching.map(r => r.hub!))];
    if (hubs.length >= hubCount) return [];
    const key = `${c.harness}:${c.session_id}`;
    if (seen.has(key)) return [];
    seen.add(key);
    const { listening_hubs: _, ...rest } = c;
    return [hubs.length ? { ...rest, listening_hubs: hubs } : rest];
  });
}

export interface AdapterCliSelection {
  harness: string;
  explicitId?: string;
}

export function matchAdapterCliFlag(
  cmd: "listen" | "unlisten",
  argv: readonly string[],
  index: number,
  config: CitoConfig,
): { selection: AdapterCliSelection; nextIndex: number } | undefined {
  const arg = argv[index]!;
  for (const adapter of ADAPTER_REGISTRY.values()) {
    if (rejectedPluginIds.has(adapter.id)) continue;
    if (!adapter.manifest?.cliFlag || RESERVED_CLI_FLAGS.has(adapter.id)) continue;
    const flag = `--${adapter.id}`;
    if (arg !== flag && !arg.startsWith(flag + "=")) continue;
    if (arg.startsWith(flag + "=")) {
      const val = arg.slice(flag.length + 1).trim();
      if (!val) throw new Error(`Missing value for ${flag}`);
      if (adapter.cli?.consumesNextArg) adapter.cli.consumesNextArg(cmd, val, config);
      return { selection: { harness: adapter.id, explicitId: val }, nextIndex: index };
    }
    const next = argv[index + 1];
    if (next !== undefined && !next.startsWith("-") && adapter.cli?.consumesNextArg?.(cmd, next, config)) {
      return { selection: { harness: adapter.id, explicitId: next.trim() }, nextIndex: index + 1 };
    }
    return { selection: { harness: adapter.id }, nextIndex: index };
  }
  return undefined;
}

export async function resolveAdapterCliMeta(
  cmd: "listen" | "unlisten",
  selection: AdapterCliSelection | undefined,
  cwd: string,
  config: CitoConfig,
): Promise<Record<string, unknown>> {
  if (!selection) return { caller_pid: process.ppid };
  const adapter = ADAPTER_REGISTRY.get(selection.harness) ?? builtinHumanAdapters.get(selection.harness);
  if (!adapter || rejectedPluginIds.has(selection.harness)) {
    throw new CitoError("UNSUPPORTED_HARNESS", `Unknown harness: ${selection.harness}`);
  }
  const cli = adapter.cli ?? defaultCliDescriptor(adapter.id);
  return await cli.resolveMeta(cmd, selection.explicitId, cwd, config);
}

export async function startOptionalStdioChannel(
  config: CitoConfig,
  transport: { send(message: JSONRPCMessage): Promise<void> },
  options: { endpointPid?: number; cwd?: string; env?: NodeJS.ProcessEnv } = {},
): Promise<StdioChannelBridge | undefined> {
  await ensurePluginsLoaded(config);
  const active: StdioChannelBridge[] = [];
  for (const adapter of ADAPTER_REGISTRY.values()) {
    if (!adapter.startStdioChannel) continue;
    try {
      const bridge = await adapter.startStdioChannel(config, transport, options);
      if (bridge) active.push(bridge);
    } catch (error) {
      console.error(error);
    }
  }
  if (active.length > 1) {
    console.error("Multiple stdio channels activated in the same process; closing all stdio channels");
    await Promise.allSettled(active.map(b => b.close()));
    return undefined;
  }
  return active[0];
}

function verifyExpectedIdentity(caller: Caller, meta: Record<string, unknown>): Caller {
  if ((typeof meta.expected_harness === "string" && meta.expected_harness && caller.harness !== meta.expected_harness)
    || (typeof meta.expected_session_id === "string" && meta.expected_session_id && caller.session_id !== meta.expected_session_id)) {
    throw new CitoError("SESSION_NOT_OPEN", "Session changed since discovery");
  }
  return caller;
}
export type ResolveCallerOptions = Pick<CallerDetectionContext, "requireOpen" | "lookupCachedSession">;
// Preserve the legacy public error code without exposing driver naming to the service.
export function callerBindingError(message: string): CitoError {
  return new CitoError("PI_BRIDGE_UNAVAILABLE", message);
}
function explicitPid(meta: Record<string, unknown>): number | undefined {
  const pidKeys = getAllPidKeys();
  const values = pidKeys.filter(key => meta[key] !== undefined).map(key => meta[key]);
  if (values.some(v => typeof v !== "number" || !Number.isSafeInteger(v) || v <= 0)) {
    throw callerBindingError("Invalid endpoint PID");
  }
  if (new Set(values).size > 1) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
  return values[0] as number | undefined;
}
export function extractCallerPidRoots(meta: Record<string, unknown> = {}): number[] {
  const pid = explicitPid(meta); if (pid !== undefined) return [pid];
  const pidKeys = getAllPidKeys();
  const roots = new Set<number>(), seen = new Set<object>();
  const walk = (value: unknown) => {
    if (!value || typeof value !== "object" || seen.has(value)) return;
    seen.add(value);
    for (const [key, child] of Object.entries(value)) {
      if ([...pidKeys, "parent_pid"].includes(key) && typeof child === "number" && Number.isSafeInteger(child) && child > 0) roots.add(child);
      else if (child && typeof child === "object") walk(child);
    }
  };
  for (const value of Object.values(meta)) walk(value);
  if (roots.size) return [...roots];
  if (typeof meta.http_client_pid === "number" && Number.isSafeInteger(meta.http_client_pid) && meta.http_client_pid > 0) return [meta.http_client_pid];
  return meta.http_transport === true ? [] : [process.ppid];
}

async function detectCallerWithDeadline(
  adapter: HarnessAdapter,
  ctx: CallerDetectionContext,
  config: CitoConfig,
): Promise<Caller | null> {
  if (!adapter.detectExplicitCaller) return null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      adapter.detectExplicitCaller(ctx, config),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          reject(new CitoError("PI_BRIDGE_UNAVAILABLE", `Plugin '${adapter.id}' caller detection timed out`));
        }, 1500);
        timer.unref?.();
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

const exec = promisify(execFile);
/** Resolve explicit adapter claims before implicit discovery or service effects. */
export async function resolveCaller(input: CallerInput = {}, config: CitoConfig = loadConfig(), options: ResolveCallerOptions = {}): Promise<Caller> {
  await ensurePluginsLoaded(config);
  // Direct embedders may still pass legacy metadata. HTTP callers always supply an envelope.
  const ctx: CallerDetectionContext = input.meta && typeof input.meta === "object" && !Array.isArray(input.meta)
    ? { ...input as CallerDetectionContext, ...options }
    : { meta: input as Record<string, unknown>, httpClientPid: (input as Record<string, unknown>).http_client_pid as number | undefined,
      httpTransport: (input as Record<string, unknown>).http_transport === true, ...options };
  const meta: Record<string, unknown> = { ...ctx.meta, http_client_pid: ctx.httpClientPid, http_transport: ctx.httpTransport };
  const pid = explicitPid(meta);
  let explicit: Caller | undefined;
  for (const adapter of [...ADAPTER_REGISTRY.values(), ...builtinHumanAdapters.values()]) {
    const caller = await detectCallerWithDeadline(adapter, ctx, config);
    if (!caller) continue;
    if (pid !== undefined || explicit) throw new CitoError("INVALID_INPUT", "Conflicting caller metadata");
    explicit = caller;
  }
  if (explicit) return verifyExpectedIdentity(explicit, meta);
  const roots = extractCallerPidRoots(meta);
  let failure: unknown;
  for (let ancestor of roots) {
    for (let depth = 0; depth <= 5 && ancestor > 0; depth++) {
      let names: string[] = [];
      try { names = await readdir(join(config.stateDir, "sockets")); }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      const pattern = new RegExp(`^([a-z][a-z0-9_-]{0,31})-pid-${ancestor}\\.sock$`);
      const harnesses = names.flatMap(name => {
        const match = pattern.exec(name);
        return match && !rejectedPluginIds.has(match[1]!) ? [match[1]!] : [];
      });
      const expectedHarness = typeof meta.expected_harness === "string" ? meta.expected_harness : undefined;
      harnesses.sort((a, b) => Number(b === expectedHarness) - Number(a === expectedHarness)
        || getHarnessOrder(a) - getHarnessOrder(b)
        || Number(ADAPTER_REGISTRY.get(b) instanceof SocketBridgeAdapter) - Number(ADAPTER_REGISTRY.get(a) instanceof SocketBridgeAdapter)
        || a.localeCompare(b));
      for (const harness of harnesses) {
        let info;
        try {
          info = await identifyBridge(harness, ancestor, config);
        } catch (error) { failure = error; continue; }
        getAdapter(harness);
        return verifyExpectedIdentity({ harness, endpoint_pid: info.endpoint_pid, session_id: info.session_id, cwd: info.cwd,
          ...await gitMetadata(info.cwd, info.session_name) }, meta);
      }
      if (ctx.requireOpen === false) {
        // The lookup enforces uniqueness across every retained status and hub.
        const cached = await ctx.lookupCachedSession?.({ endpointPid: ancestor });
        if (cached) return cached;
      }
      try {
        const { stdout } = await exec("/bin/ps", ["-o", "ppid=", "-p", String(ancestor)], { timeout: 1000 });
        const parent = Number(stdout.trim());
        if (!Number.isSafeInteger(parent) || parent <= 0 || parent === ancestor) break;
        ancestor = parent;
      } catch { break; }
    }
  }
  // Missing/unopen implicit ancestors leave outbound callers unbound. An explicit PID is a claim and must fail visibly.
  throw new CitoError(pid !== undefined ? "PI_BRIDGE_UNAVAILABLE" : "BINDING_REQUIRED",
    "No caller socket bridge found", { cause: failure });
}
