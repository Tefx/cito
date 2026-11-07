import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import {
  ADAPTER_REGISTRY,
  BUILTIN_PLUGIN_IDS,
  mutateConfigFile,
  rejectedPluginIds,
  type HarnessSetupInspection,
  type SetupFileAction,
} from "../adapters/index.js";
import {
  expandHomeDir,
  hubNameSchema,
  normalizeIssuer,
  validateHarness,
  validateHttpUrl,
  validateNatsUrl,
  type HubAuthMode,
} from "../shared/index.js";

export type { HarnessSetupInspection, SetupFileAction };

export interface SetupHubInput {
  name: string;
  url: string;
  authMode?: HubAuthMode;
  token?: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  defaultScope?: "self" | "workspace";
  setDefault?: boolean;
}

export interface SetupOptions {
  env?: NodeJS.ProcessEnv;
  hub?: SetupHubInput;
  enableHubs?: string[];
  disableHubs?: string[];
  enableHarnesses?: string[];
  force?: boolean;
  dryRun?: boolean;
}

export interface SetupHubSummary {
  url: string;
  natsUrl: string;
  authMode: HubAuthMode;
  token?: string;
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
}

export interface SetupResult {
  ok: boolean;
  dryRun: boolean;
  configPath: string;
  defaultHub: string;
  actions: SetupFileAction[];
}

function resolvePaths(env: NodeJS.ProcessEnv): { stateDir: string; configPath: string } {
  const stateDir = expandHomeDir(env.CITO_STATE_DIR ?? "~/.cito", "CITO_STATE_DIR", env.HOME);
  const configPath = env.CITO_CONFIG_PATH
    ? expandHomeDir(env.CITO_CONFIG_PATH, "CITO_CONFIG_PATH", env.HOME)
    : join(stateDir, "config.json");
  return { stateDir, configPath };
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
  const defaultScope = hub.defaultScope ?? (url === "local://" ? "workspace" : "self");
  return {
    name: nameRes.data,
    entry: {
      url,
      natsUrl: url,
      authMode,
      ...(hub.token ? { token: hub.token.trim() } : {}),
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

  return {
    configPath,
    configExists,
    defaultHub: parsed.default_hub,
    hubs,
    harnesses,
  };
}

async function runSetupEngine(options: SetupOptions, dryRun: boolean): Promise<SetupResult> {
  const env = options.env ?? process.env;
  const { configPath } = resolvePaths(env);
  const enabledHarnesses = new Set((options.enableHarnesses ?? []).map(h => validateHarness(h)));
  for (const h of enabledHarnesses) {
    if (!ADAPTER_REGISTRY.has(h) || rejectedPluginIds.has(h)) {
      throw new Error(`Unknown harness: ${h}`);
    }
  }

  const validatedHub = options.hub ? validateHubInput(options.hub) : undefined;
  for (const name of [...(options.enableHubs ?? []), ...(options.disableHubs ?? [])]) {
    if (!hubNameSchema.safeParse(name).success) {
      throw new Error(`Invalid hub name: ${name}`);
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
  const enableHubs: string[] = [];
  const disableHubs: string[] = [];
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
      enableHarnesses,
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
