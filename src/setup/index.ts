import { execFile } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { chmod, copyFile, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  ADAPTER_REGISTRY,
  BUILTIN_PLUGIN_IDS,
  mutateConfigFile,
  rejectedPluginIds,
  type HarnessSetupInspection,
  type SetupFileAction,
} from "../adapters/index.js";
import {
  CITO_VERSION,
  expandHomeDir,
  hubNameSchema,
  normalizeIssuer,
  validateHarness,
  validateHttpUrl,
  validateNatsUrl,
  type HostHubConfig,
  type HubAuthMode,
} from "../shared/index.js";
import { ensureUiCompiled } from "../ui/index.js";

export type { HarnessSetupInspection, SetupFileAction };

const exec = promisify(execFile);

export interface SetupHubInput {
  name: string;
  url: string;
  authMode?: HubAuthMode;
  token?: string;
  user?: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  defaultScope?: "self" | "workspace";
  setDefault?: boolean;
}

export interface SetupHostHubInput {
  enabled?: boolean;
  port?: number;
  token?: string;
  resetToken?: boolean;
}

export interface SetupOptions {
  env?: NodeJS.ProcessEnv;
  hub?: SetupHubInput;
  enableHubs?: string[];
  disableHubs?: string[];
  removeHubs?: string[];
  enableHarnesses?: string[];
  hostHub?: SetupHostHubInput;
  launchAtLogin?: boolean;
  force?: boolean;
  dryRun?: boolean;
}

export interface SetupHubSummary {
  url: string;
  natsUrl: string;
  authMode: HubAuthMode;
  token?: string;
  user?: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  enabled: boolean;
  defaultScope?: "self" | "workspace";
}

export interface SetupInspection {
  configPath: string;
  configExists: boolean;
  defaultHub: string;
  hubs: Record<string, SetupHubSummary>;
  harnesses: HarnessSetupInspection[];
  hostHub: HostHubConfig;
  launchAtLogin: boolean;
}

export interface SetupResult {
  ok: boolean;
  dryRun: boolean;
  configPath: string;
  defaultHub: string;
  actions: SetupFileAction[];
}

function resolvePackageRoot(): string {
  const direct = fileURLToPath(new URL("../../package.json", import.meta.url));
  if (existsSync(direct)) return dirname(direct);
  const fromDist = fileURLToPath(new URL("../../../package.json", import.meta.url));
  if (existsSync(fromDist)) return dirname(fromDist);
  return resolve(".");
}

function resolvePaths(env: NodeJS.ProcessEnv): { stateDir: string; configPath: string; homeDir: string } {
  const homeDir = expandHomeDir("~", "HOME", env.HOME);
  const stateDir = expandHomeDir(env.CITO_STATE_DIR ?? "~/.cito", "CITO_STATE_DIR", env.HOME);
  const configPath = env.CITO_CONFIG_PATH
    ? expandHomeDir(env.CITO_CONFIG_PATH, "CITO_CONFIG_PATH", env.HOME)
    : join(stateDir, "config.json");
  return { stateDir, configPath, homeDir };
}

export async function ensureCliShims(options: {
  stateDir?: string;
  env?: NodeJS.ProcessEnv;
} = {}): Promise<{ binDir: string; paths: string[] }> {
  const env = options.env ?? process.env;
  const stateDir = options.stateDir ?? resolvePaths(env).stateDir;
  const binDir = join(stateDir, "bin");
  await mkdir(binDir, { recursive: true, mode: 0o700 });
  await chmod(binDir, 0o700).catch(() => {});

  const pkgRoot = resolvePackageRoot();
  const pkgJsonPath = join(pkgRoot, "package.json");
  const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf8")) as { bin?: Record<string, string> };
  const bins = pkg.bin ?? { cito: "bin/cito-mcp", "cito-mcp": "bin/cito-mcp" };
  const nodeBin = env.CITO_NODE_BIN?.trim() || process.execPath;
  const nativePath = join(pkgRoot, "bin", "cito-native-darwin-arm64");
  const written: string[] = [];

  for (const [binName, relTarget] of Object.entries(bins)) {
    const targetPath = join(pkgRoot, relTarget);
    const shimPath = join(binDir, binName);
    let script: string;
    if (binName.endsWith("-hook")) {
      const subMode = binName.replace(/^cito-/, "");
      script = [
        "#!/bin/sh",
        `NATIVE_BIN=${JSON.stringify(nativePath)}`,
        'if [ "${CITO_DISABLE_NATIVE:-0}" != "1" ] && [ -x "$NATIVE_BIN" ]; then',
        `  exec "$NATIVE_BIN" ${JSON.stringify(subMode)} "$@"`,
        "fi",
        `NODE_BIN="\${CITO_NODE_BIN:-${nodeBin}}"`,
        `exec "$NODE_BIN" ${JSON.stringify(targetPath)} "$@"`,
        "",
      ].join("\n");
    } else {
      script = [
        "#!/bin/sh",
        `NATIVE_BIN=${JSON.stringify(nativePath)}`,
        `NODE_BIN="\${CITO_NODE_BIN:-${nodeBin}}"`,
        'export CITO_NODE_BIN="$NODE_BIN"',
        `export CITO_MCP_ENTRY=${JSON.stringify(targetPath)}`,
        'if [ "$#" -eq 0 ] && [ "${CITO_DISABLE_NATIVE:-0}" != "1" ] && [ -x "$NATIVE_BIN" ]; then',
        '  exec "$NATIVE_BIN" mcp-stdio',
        "fi",
        `exec "$NODE_BIN" ${JSON.stringify(targetPath)} "$@"`,
        "",
      ].join("\n");
    }
    await writeFile(shimPath, script, { encoding: "utf8", mode: 0o755 });
    await chmod(shimPath, 0o755);
    written.push(shimPath);
  }

  return { binDir, paths: written };
}

export function launchAgentPlistPath(env: NodeJS.ProcessEnv = process.env): string {
  const { homeDir } = resolvePaths(env);
  return join(homeDir, "Library", "LaunchAgents", "com.tefx.cito.daemon.plist");
}

export async function inspectLaunchAtLogin(options: { env?: NodeJS.ProcessEnv } = {}): Promise<{
  enabled: boolean;
  plistPath: string;
}> {
  const env = options.env ?? process.env;
  const plistPath = launchAgentPlistPath(env);
  if (!existsSync(plistPath)) {
    return { enabled: false, plistPath };
  }
  try {
    const content = await readFile(plistPath, "utf8");
    const enabled = content.includes("com.tefx.cito.daemon") && content.includes("<true/>");
    return { enabled, plistPath };
  } catch {
    return { enabled: false, plistPath };
  }
}

function escapeXml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

export async function setLaunchAtLogin(
  enabled: boolean,
  options: { env?: NodeJS.ProcessEnv } = {},
): Promise<{ enabled: boolean; plistPath: string }> {
  const env = options.env ?? process.env;
  const { stateDir, homeDir } = resolvePaths(env);
  const plistPath = launchAgentPlistPath(env);

  if (!enabled) {
    await rm(plistPath, { force: true });
    return { enabled: false, plistPath };
  }

  await ensureCliShims({ stateDir, env });
  const pkgRoot = resolvePackageRoot();
  const citoMcpPath = join(pkgRoot, "bin", "cito-mcp");
  const nodeBin = env.CITO_NODE_BIN?.trim() || process.execPath;
  const binDir = join(stateDir, "bin");
  const pathValue = `${binDir}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`;

  const plistXml = [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
    '<plist version="1.0">',
    "<dict>",
    "  <key>Label</key>",
    "  <string>com.tefx.cito.daemon</string>",
    "  <key>ProgramArguments</key>",
    "  <array>",
    `    <string>${escapeXml(nodeBin)}</string>`,
    `    <string>${escapeXml(citoMcpPath)}</string>`,
    "    <string>daemon</string>",
    "  </array>",
    "  <key>RunAtLoad</key>",
    "  <true/>",
    "  <key>KeepAlive</key>",
    "  <false/>",
    "  <key>EnvironmentVariables</key>",
    "  <dict>",
    "    <key>HOME</key>",
    `    <string>${escapeXml(homeDir)}</string>`,
    "    <key>PATH</key>",
    `    <string>${escapeXml(pathValue)}</string>`,
    "    <key>CITO_STATE_DIR</key>",
    `    <string>${escapeXml(stateDir)}</string>`,
    "  </dict>",
    "</dict>",
    "</plist>",
    "",
  ].join("\n");

  await mkdir(dirname(plistPath), { recursive: true });
  await writeFile(plistPath, plistXml, { encoding: "utf8", mode: 0o644 });
  return { enabled: true, plistPath };
}

async function computeDirectoryBytes(dirPath: string): Promise<number> {
  const st = await stat(dirPath);
  if (!st.isDirectory()) return st.size;
  const entries = await readdir(dirPath);
  const sizes = await Promise.all(entries.map(name => computeDirectoryBytes(join(dirPath, name))));
  return sizes.reduce((acc, n) => acc + n, 0);
}

export async function buildMacAppBundle(options: {
  destDir?: string;
  env?: NodeJS.ProcessEnv;
} = {}): Promise<{ appPath: string; bytes: number }> {
  const env = options.env ?? process.env;
  const { stateDir } = resolvePaths(env);
  const baseDest = options.destDir
    ? expandHomeDir(options.destDir, "destDir", env.HOME)
    : join(stateDir, "Applications");
  const appPath = baseDest.endsWith(".app") ? baseDest : join(baseDest, "Cito.app");
  const contentsDir = join(appPath, "Contents");
  const macosDir = join(contentsDir, "MacOS");
  const helpersDir = join(contentsDir, "Helpers");

  await rm(appPath, { recursive: true, force: true });
  await mkdir(macosDir, { recursive: true });
  await mkdir(helpersDir, { recursive: true });

  const infoPlist = [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
    '<plist version="1.0">',
    "<dict>",
    "  <key>CFBundleIdentifier</key>",
    "  <string>one.tefx.cito</string>",
    "  <key>CFBundleName</key>",
    "  <string>Cito</string>",
    "  <key>CFBundleDisplayName</key>",
    "  <string>Cito</string>",
    "  <key>CFBundleExecutable</key>",
    "  <string>Cito</string>",
    "  <key>CFBundleShortVersionString</key>",
    `  <string>${escapeXml(CITO_VERSION)}</string>`,
    "  <key>CFBundleVersion</key>",
    `  <string>${escapeXml(CITO_VERSION)}</string>`,
    "  <key>CFBundlePackageType</key>",
    "  <string>APPL</string>",
    "  <key>LSUIElement</key>",
    "  <true/>",
    "  <key>LSMinimumSystemVersion</key>",
    "  <string>13.0</string>",
    "</dict>",
    "</plist>",
    "",
  ].join("\n");
  await writeFile(join(contentsDir, "Info.plist"), infoPlist, { encoding: "utf8", mode: 0o644 });

  const pkgRoot = resolvePackageRoot();
  const citoMcpPath = join(pkgRoot, "bin", "cito-mcp");
  const fallbackNode = env.CITO_NODE_BIN?.trim() || process.execPath;
  const launcherScript = [
    "#!/bin/sh",
    "set -e",
    'resolve_node() {',
    '  for cand in "$CITO_NODE_BIN" ' +
      `${JSON.stringify(fallbackNode)} ` +
      '"/opt/homebrew/bin/node" "/usr/local/bin/node" ' +
      '"$HOME/.local/share/mise/shims/node" "$HOME/.fnm/current/bin/node" ' +
      '"$HOME/.volta/bin/node" "$HOME/.cito/runtime/bin/node"; do',
    '    if [ -n "$cand" ] && [ -x "$cand" ]; then',
    '      echo "$cand"',
    "      return 0",
    "    fi",
    "  done",
    '  for nvm_cand in "$HOME"/.nvm/versions/node/*/bin/node; do',
    '    if [ -x "$nvm_cand" ]; then',
    '      echo "$nvm_cand"',
    "      return 0",
    "    fi",
    "  done",
    "  command -v node 2>/dev/null || true",
    "}",
    'NODE_BIN="$(resolve_node)"',
    'if [ -z "$NODE_BIN" ]; then',
    '  echo "Cito.app: Node.js (>= 22.13) not found" >&2',
    "  exit 1",
    "fi",
    'export CITO_NODE_BIN="$NODE_BIN"',
    `exec "$NODE_BIN" ${JSON.stringify(citoMcpPath)} daemon "$@"`,
    "",
  ].join("\n");
  const launcherPath = join(macosDir, "Cito");
  await writeFile(launcherPath, launcherScript, { encoding: "utf8", mode: 0o755 });
  await chmod(launcherPath, 0o755);

  const prebuiltUi = join(pkgRoot, "bin", "cito-ui-darwin-arm64");
  const uiSrc = existsSync(prebuiltUi) ? prebuiltUi : await ensureUiCompiled(stateDir);
  const helperUi = join(helpersDir, "cito-ui");
  await copyFile(uiSrc, helperUi);
  await chmod(helperUi, 0o755);

  const prebuiltNative = join(pkgRoot, "bin", "cito-native-darwin-arm64");
  if (existsSync(prebuiltNative)) {
    const helperNative = join(helpersDir, "cito-native");
    await copyFile(prebuiltNative, helperNative);
    await chmod(helperNative, 0o755);
  }

  await ensureCliShims({ stateDir, env });

  if (process.platform === "darwin") {
    await exec("/usr/bin/codesign", ["-s", "-", "--force", "--deep", appPath]).catch(() => {});
  }

  const bytes = await computeDirectoryBytes(appPath);
  return { appPath, bytes };
}

function normalizeHostHub(raw: unknown): HostHubConfig {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return { enabled: false, port: 4291, token: "" };
  }
  const obj = raw as Record<string, unknown>;
  const port = typeof obj.port === "number" && Number.isInteger(obj.port) && obj.port >= 0 && obj.port <= 65535
    ? obj.port
    : 4291;
  const token = typeof obj.token === "string" ? obj.token.trim() : "";
  return {
    enabled: obj.enabled === true,
    port,
    token,
  };
}

function normalizeExistingConfig(rawText: string | undefined): {
  default_hub: string;
  hubs: Record<string, Record<string, unknown>>;
  rest: Record<string, unknown>;
} {
  if (rawText === undefined || !rawText.trim()) {
    return {
      default_hub: "local",
      hubs: {
        local: {
          url: "local://",
          natsUrl: "local://",
          authMode: "none",
          enabled: true,
          defaultScope: "workspace",
        },
      },
      rest: {},
    };
  }
  const parsed: unknown = JSON.parse(rawText);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Expected JSON object in config.json");
  }
  const obj = { ...(parsed as Record<string, unknown>) };
  const rawHubs =
    obj.hubs && typeof obj.hubs === "object" && !Array.isArray(obj.hubs)
      ? (obj.hubs as Record<string, Record<string, unknown>>)
      : {};
  const hubs: Record<string, Record<string, unknown>> = {};
  for (const [k, v] of Object.entries(rawHubs)) {
    if (v && typeof v === "object" && !Array.isArray(v)) {
      const urlVal = typeof v.url === "string" ? v.url : typeof v.natsUrl === "string" ? v.natsUrl : typeof v.nats_url === "string" ? v.nats_url : "local://";
      const authVal = (typeof v.auth === "string" ? v.auth : typeof v.authMode === "string" ? v.authMode : typeof v.auth_mode === "string" ? v.auth_mode : "none") as HubAuthMode;
      hubs[k] = {
        ...v,
        url: urlVal,
        natsUrl: urlVal,
        authMode: authVal,
        enabled: v.enabled !== false,
      };
    }
  }
  if (Object.keys(hubs).length === 0) {
    hubs.local = {
      url: "local://",
      natsUrl: "local://",
      authMode: "none",
      enabled: true,
      defaultScope: "workspace",
    };
  }
  const defaultHub = typeof obj.default_hub === "string" && obj.default_hub in hubs
    ? obj.default_hub
    : Object.keys(hubs)[0]!;
  delete obj.hubs;
  delete obj.default_hub;
  return { default_hub: defaultHub, hubs, rest: obj };
}

function validateHubInput(hub: SetupHubInput): {
  name: string;
  entry: Record<string, unknown>;
} {
  const nameRes = hubNameSchema.safeParse(hub.name);
  if (!nameRes.success) {
    throw new Error(`Invalid hub name: ${hub.name}`);
  }
  const url = validateNatsUrl(hub.url);
  const authMode: HubAuthMode = hub.authMode ?? "none";
  if (authMode !== "none" && authMode !== "token" && authMode !== "oidc") {
    throw new Error("Hub authMode must be none, token, or oidc");
  }
  if (authMode === "token" && !hub.token?.trim()) {
    throw new Error("Hub token is required when auth is token");
  }
  let oidcIssuer: string | undefined;
  let oidcClientId: string | undefined;
  if (authMode === "oidc" || hub.oidcIssuer !== undefined) {
    if (!hub.oidcIssuer?.trim()) {
      throw new Error("Hub issuer is required when auth is oidc");
    }
    oidcIssuer = normalizeIssuer(validateHttpUrl(hub.oidcIssuer.trim(), "OIDC issuer"));
    if (!hub.oidcClientId?.trim()) {
      throw new Error("Hub client-id is required when auth is oidc");
    }
    oidcClientId = hub.oidcClientId.trim();
  }
  const cleanUser = hub.user?.trim();
  if (cleanUser !== undefined && cleanUser.length > 0 && (cleanUser.length > 64 || /[^a-zA-Z0-9._-]/.test(cleanUser))) {
    throw new Error("Hub user must contain 1-64 letters, numbers, dots, underscores or hyphens");
  }
  const defaultScope = hub.defaultScope ?? (url === "local://" ? "workspace" : "self");
  return {
    name: nameRes.data,
    entry: {
      url,
      natsUrl: url,
      authMode,
      ...(hub.token ? { token: hub.token.trim() } : {}),
      ...(cleanUser ? { user: cleanUser } : {}),
      ...(oidcIssuer ? { oidcIssuer } : {}),
      ...(oidcClientId ? { oidcClientId } : {}),
      enabled: true,
      defaultScope,
    },
  };
}

export async function inspectSetup(options: { env?: NodeJS.ProcessEnv } = {}): Promise<SetupInspection> {
  const env = options.env ?? process.env;
  const { configPath } = resolvePaths(env);
  const configExists = existsSync(configPath);
  const rawText = configExists ? readFileSync(configPath, "utf8") : undefined;
  const parsed = normalizeExistingConfig(rawText);

  const hubs: Record<string, SetupHubSummary> = {};
  for (const [name, h] of Object.entries(parsed.hubs)) {
    const url = String(h.url ?? h.natsUrl ?? "local://");
    const authMode = (h.authMode ?? "none") as HubAuthMode;
    hubs[name] = {
      url,
      natsUrl: url,
      authMode,
      ...(typeof h.token === "string" ? { token: h.token } : {}),
      ...(typeof h.user === "string" ? { user: h.user } : {}),
      ...(typeof h.oidcIssuer === "string" ? { oidcIssuer: h.oidcIssuer } : {}),
      ...(typeof h.oidcClientId === "string" ? { oidcClientId: h.oidcClientId } : {}),
      enabled: h.enabled !== false,
      ...(h.defaultScope === "self" || h.defaultScope === "workspace" ? { defaultScope: h.defaultScope } : {}),
    };
  }

  const harnesses: HarnessSetupInspection[] = [];
  for (const adapter of ADAPTER_REGISTRY.values()) {
    if (rejectedPluginIds.has(adapter.id) || !adapter.setup) continue;
    const info = await adapter.setup.inspect(env);
    const tier = BUILTIN_PLUGIN_IDS.has(adapter.id) ? (adapter.manifest?.tier ?? "stable") : "custom";
    harnesses.push({
      id: adapter.id,
      displayName: adapter.manifest?.displayName ?? adapter.id,
      tier,
      installed: info.installed,
      configured: info.configured,
      paths: info.paths,
    });
  }

  const hostHub = normalizeHostHub(parsed.rest.host_hub ?? parsed.rest.hostHub);
  const loginInfo = await inspectLaunchAtLogin({ env });

  return {
    configPath,
    configExists,
    defaultHub: parsed.default_hub,
    hubs,
    harnesses,
    hostHub,
    launchAtLogin: loginInfo.enabled,
  };
}

async function runSetupEngine(options: SetupOptions, dryRun: boolean): Promise<SetupResult> {
  const env = options.env ?? process.env;
  const { stateDir, configPath } = resolvePaths(env);
  const enabledHarnesses = new Set((options.enableHarnesses ?? []).map(h => validateHarness(h)));
  for (const h of enabledHarnesses) {
    if (!ADAPTER_REGISTRY.has(h) || rejectedPluginIds.has(h)) {
      throw new Error(`Unknown harness: ${h}`);
    }
  }

  const validatedHub = options.hub ? validateHubInput(options.hub) : undefined;
  for (const name of [...(options.enableHubs ?? []), ...(options.disableHubs ?? []), ...(options.removeHubs ?? [])]) {
    if (!hubNameSchema.safeParse(name).success) {
      throw new Error(`Invalid hub name: ${name}`);
    }
  }
  for (const name of options.removeHubs ?? []) {
    if (name === "local") {
      throw new Error("Cannot remove reserved 'local' hub; disable it instead");
    }
  }

  let resolvedDefaultHub = "local";
  const configAction = await mutateConfigFile({
    target: "cito",
    path: configPath,
    dryRun,
    mode: 0o600,
    transform: existingContent => {
      const state = normalizeExistingConfig(existingContent);
      const hadRemoteBefore = Object.entries(state.hubs).some(
        ([k, v]) => k !== "local" && String(v.url ?? v.natsUrl ?? "") !== "local://",
      );

      if (validatedHub) {
        const prev = state.hubs[validatedHub.name] ?? {};
        state.hubs[validatedHub.name] = {
          ...prev,
          ...validatedHub.entry,
        };
        if (
          options.hub?.setDefault ||
          (!hadRemoteBefore && state.default_hub === "local" && validatedHub.name !== "local")
        ) {
          state.default_hub = validatedHub.name;
        }
      }

      for (const name of options.enableHubs ?? []) {
        if (!state.hubs[name]) {
          throw new Error(`Unknown hub: ${name}`);
        }
        state.hubs[name] = { ...state.hubs[name], enabled: true };
      }
      for (const name of options.disableHubs ?? []) {
        if (!state.hubs[name]) {
          throw new Error(`Unknown hub: ${name}`);
        }
        state.hubs[name] = { ...state.hubs[name], enabled: false };
      }
      for (const name of options.removeHubs ?? []) {
        if (!state.hubs[name]) {
          throw new Error(`Unknown hub: ${name}`);
        }
        delete state.hubs[name];
      }

      if (options.hostHub) {
        const prevHost = normalizeHostHub(state.rest.host_hub ?? state.rest.hostHub);
        delete state.rest.hostHub;
        const enabled = options.hostHub.enabled ?? prevHost.enabled;
        const port = options.hostHub.port ?? prevHost.port ?? 4291;
        if (!Number.isInteger(port) || port < 0 || port > 65535) {
          throw new Error("host_hub.port must be an integer in 0..65535");
        }
        const token =
          options.hostHub.resetToken || !prevHost.token
            ? options.hostHub.token?.trim() || randomBytes(32).toString("hex")
            : options.hostHub.token?.trim() || prevHost.token;
        state.rest.host_hub = { enabled, port, token };
      }

      if (!state.hubs[state.default_hub] || state.hubs[state.default_hub]!.enabled === false) {
        const firstEnabled = Object.entries(state.hubs).find(([, h]) => h.enabled !== false)?.[0];
        state.default_hub = firstEnabled ?? Object.keys(state.hubs)[0] ?? "local";
      }

      resolvedDefaultHub = state.default_hub;
      const nextObj = {
        default_hub: state.default_hub,
        hubs: state.hubs,
        ...state.rest,
      };
      const nextContent = JSON.stringify(nextObj, null, 2) + "\n";
      if (existingContent === undefined) {
        return {
          status: "created",
          nextContent,
          detail: `Created ${configPath} (default_hub: ${state.default_hub})`,
        };
      }
      if (existingContent === nextContent) {
        return {
          status: "unchanged",
          detail: `${configPath} already up to date`,
        };
      }
      return {
        status: "updated",
        nextContent,
        detail: `Updated ${configPath} (default_hub: ${state.default_hub})`,
      };
    },
  });

  const actions: SetupFileAction[] = [configAction];

  for (const adapter of ADAPTER_REGISTRY.values()) {
    if (rejectedPluginIds.has(adapter.id) || !adapter.setup) continue;
    const tier = BUILTIN_PLUGIN_IDS.has(adapter.id) ? (adapter.manifest?.tier ?? "stable") : "custom";
    const shouldConfigure = tier === "stable" || enabledHarnesses.has(adapter.id);
    if (!shouldConfigure) {
      const info = await adapter.setup.inspect(env);
      actions.push({
        target: adapter.id,
        path: info.paths[0] ?? adapter.id,
        status: "skipped",
        detail: `Skipped ${tier} harness '${adapter.id}' (pass --enable ${adapter.id} to configure)`,
      });
      continue;
    }
    const adapterActions = dryRun
      ? await adapter.setup.plan(env, { force: options.force })
      : await adapter.setup.apply(env, { force: options.force });
    actions.push(...adapterActions);
  }

  if (!dryRun) {
    await ensureCliShims({ stateDir, env });
    if (options.launchAtLogin !== undefined) {
      await setLaunchAtLogin(options.launchAtLogin, { env });
    }
  }

  return {
    ok: !actions.some(a => a.status === "conflict"),
    dryRun,
    configPath,
    defaultHub: resolvedDefaultHub,
    actions,
  };
}

export async function previewSetup(options: SetupOptions = {}): Promise<SetupResult> {
  return await runSetupEngine({ ...options, dryRun: true }, true);
}

export async function applySetup(options: SetupOptions = {}): Promise<SetupResult> {
  if (options.dryRun) {
    return await previewSetup(options);
  }
  return await runSetupEngine(options, false);
}

export async function runSetupCli(
  argv: readonly string[] = process.argv.slice(3),
  env: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  let statusOnly = false;
  let dryRun = false;
  let force = false;
  let jsonOutput = false;
  let hubName: string | undefined;
  let hubUrl: string | undefined;
  let hubAuth: HubAuthMode | undefined;
  let hubToken: string | undefined;
  let hubIssuer: string | undefined;
  let hubClientId: string | undefined;
  let setDefault = false;
  let launchAtLogin: boolean | undefined;
  let buildApp = false;
  let buildAppDest: string | undefined;
  let shareHub: boolean | undefined;
  let sharePort: number | undefined;
  const enableHubs: string[] = [];
  const disableHubs: string[] = [];
  const removeHubs: string[] = [];
  const enableHarnesses: string[] = [];

  const readVal = (arg: string, idx: number, flag: string): [string, number] => {
    if (arg.startsWith(flag + "=")) {
      const v = arg.slice(flag.length + 1).trim();
      if (!v) throw new Error(`Missing value for ${flag}`);
      return [v, idx];
    }
    const next = argv[idx + 1];
    if (next === undefined || next.startsWith("-")) {
      throw new Error(`Missing value for ${flag}`);
    }
    return [next, idx + 1];
  };

  try {
    for (let i = 0; i < argv.length; i++) {
      const arg = argv[i]!;
      if (arg === "--status") {
        statusOnly = true;
      } else if (arg === "--dry-run") {
        dryRun = true;
      } else if (arg === "--force") {
        force = true;
      } else if (arg === "--json") {
        jsonOutput = true;
      } else if (arg === "--default") {
        setDefault = true;
      } else if (arg === "--login-item") {
        launchAtLogin = true;
      } else if (arg === "--no-login-item") {
        launchAtLogin = false;
      } else if (arg === "--share-hub") {
        shareHub = true;
      } else if (arg === "--no-share-hub") {
        shareHub = false;
      } else if (arg === "--share-port" || arg.startsWith("--share-port=")) {
        let val: string;
        [val, i] = readVal(arg, i, "--share-port");
        sharePort = Number(val);
      } else if (arg === "--build-app" || arg.startsWith("--build-app=")) {
        buildApp = true;
        if (arg.startsWith("--build-app=")) {
          buildAppDest = arg.slice("--build-app=".length).trim() || undefined;
        } else if (argv[i + 1] !== undefined && !argv[i + 1]!.startsWith("-")) {
          buildAppDest = argv[i + 1]!;
          i += 1;
        }
      } else if (arg === "--hub" || arg.startsWith("--hub=")) {
        [hubName, i] = readVal(arg, i, "--hub");
      } else if (arg === "--url" || arg.startsWith("--url=")) {
        [hubUrl, i] = readVal(arg, i, "--url");
      } else if (arg === "--auth" || arg.startsWith("--auth=")) {
        let val: string;
        [val, i] = readVal(arg, i, "--auth");
        if (val !== "none" && val !== "token" && val !== "oidc") {
          throw new Error(`Invalid --auth mode: ${val}`);
        }
        hubAuth = val;
      } else if (arg === "--token" || arg.startsWith("--token=")) {
        [hubToken, i] = readVal(arg, i, "--token");
      } else if (arg === "--issuer" || arg.startsWith("--issuer=")) {
        [hubIssuer, i] = readVal(arg, i, "--issuer");
      } else if (arg === "--client-id" || arg.startsWith("--client-id=")) {
        [hubClientId, i] = readVal(arg, i, "--client-id");
      } else if (arg === "--enable-hub" || arg.startsWith("--enable-hub=")) {
        let val: string;
        [val, i] = readVal(arg, i, "--enable-hub");
        enableHubs.push(val);
      } else if (arg === "--disable-hub" || arg.startsWith("--disable-hub=")) {
        let val: string;
        [val, i] = readVal(arg, i, "--disable-hub");
        disableHubs.push(val);
      } else if (arg === "--remove-hub" || arg.startsWith("--remove-hub=")) {
        let val: string;
        [val, i] = readVal(arg, i, "--remove-hub");
        removeHubs.push(val);
      } else if (arg === "--enable" || arg.startsWith("--enable=")) {
        let val: string;
        [val, i] = readVal(arg, i, "--enable");
        enableHarnesses.push(val);
      } else if (arg.startsWith("-")) {
        throw new Error(`Unknown flag: ${arg}`);
      } else {
        throw new Error(`Unexpected argument: ${arg}`);
      }
    }

    if ((hubName !== undefined) !== (hubUrl !== undefined)) {
      throw new Error("--hub and --url must be specified together");
    }

    if (buildApp) {
      const built = await buildMacAppBundle({ destDir: buildAppDest, env });
      if (jsonOutput) {
        process.stdout.write(JSON.stringify(built, null, 2) + "\n");
      } else {
        process.stdout.write(`Built ${built.appPath} (${built.bytes} bytes)\n`);
      }
      return;
    }

    if (statusOnly) {
      const inspection = await inspectSetup({ env });
      if (jsonOutput) {
        process.stdout.write(JSON.stringify(inspection, null, 2) + "\n");
      } else {
        process.stdout.write(`Cito Configuration: ${inspection.configPath} (${inspection.configExists ? "exists" : "default"})\n`);
        process.stdout.write(`Default Hub: ${inspection.defaultHub}\n`);
        for (const [name, h] of Object.entries(inspection.hubs)) {
          process.stdout.write(`  - hub ${name}: ${h.url} (auth=${h.authMode}, enabled=${h.enabled})\n`);
        }
        for (const h of inspection.harnesses) {
          process.stdout.write(`  - harness ${h.id} [${h.tier}]: installed=${h.installed}, configured=${h.configured}\n`);
        }
      }
      return;
    }

    const opts: SetupOptions = {
      env,
      force,
      dryRun,
      enableHubs,
      disableHubs,
      removeHubs,
      enableHarnesses,
      ...(launchAtLogin !== undefined ? { launchAtLogin } : {}),
      ...(shareHub !== undefined || sharePort !== undefined
        ? { hostHub: { ...(shareHub !== undefined ? { enabled: shareHub } : {}), ...(sharePort !== undefined ? { port: sharePort } : {}) } }
        : {}),
      ...(hubName && hubUrl
        ? {
            hub: {
              name: hubName,
              url: hubUrl,
              authMode: hubAuth,
              token: hubToken,
              oidcIssuer: hubIssuer,
              oidcClientId: hubClientId,
              setDefault,
            },
          }
        : {}),
    };

    const result = dryRun ? await previewSetup(opts) : await applySetup(opts);
    if (jsonOutput) {
      process.stdout.write(JSON.stringify(result, null, 2) + "\n");
    } else {
      for (const action of result.actions) {
        process.stdout.write(`[${action.status}] ${action.target}: ${action.path} (${action.detail})\n`);
      }
    }
    if (!result.ok) {
      for (const conflict of result.actions.filter(a => a.status === "conflict")) {
        process.stderr.write(`cito setup: conflict in ${conflict.path}: ${conflict.detail}\n`);
      }
      process.exitCode = 1;
    }
  } catch (error) {
    process.stderr.write("cito setup: " + (error instanceof Error ? error.message : String(error)) + "\n");
    process.exitCode = 1;
  }
}
