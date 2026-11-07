import { existsSync, readFileSync } from "node:fs";
import { rm } from "node:fs/promises";
import { join } from "node:path";
import { expandHomeDir } from "../../src/shared/index.js";
import { SocketBridgeAdapter, mutateConfigFile } from "../../src/adapters/socket-bridge.js";
import type { HarnessSetupDescriptor, PluginManifest, SetupFileAction } from "../../src/adapters/types.js";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/pi/plugin.json", import.meta.url);
const piManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

const localBridgeUrl = new URL("./cito-bridge.ts", import.meta.url);
const rootBridgeUrl = new URL("../../../plugins/pi/cito-bridge.ts", import.meta.url);
function readBridgeSource(): string {
  return readFileSync(existsSync(localBridgeUrl) ? localBridgeUrl : rootBridgeUrl, "utf8");
}

const PI_LEGACY_PACKAGE_SPEC = "npm:@tefx/cito";

function transformPiExtension(existing: string | undefined): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  const desired = readBridgeSource();
  if (existing === undefined) {
    return {
      status: "created",
      nextContent: desired,
      detail: "Installed Pi cito-bridge.ts extension",
    };
  }
  if (existing === desired) {
    return {
      status: "unchanged",
      detail: "Pi cito-bridge.ts extension already up to date",
    };
  }
  return {
    status: "updated",
    nextContent: desired,
    detail: "Updated Pi cito-bridge.ts extension",
  };
}

function cleanLegacyPiSettingsJson(existing: string | undefined): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  if (existing === undefined) {
    return {
      status: "unchanged",
      detail: "Pi settings.json does not exist",
    };
  }
  let parsed: unknown;
  try {
    parsed = existing.trim() ? JSON.parse(existing) : {};
  } catch {
    throw new Error("Invalid JSON in Pi settings.json");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Expected JSON object in Pi settings.json");
  }
  const obj = { ...(parsed as Record<string, unknown>) };
  if (!Array.isArray(obj.packages) || !obj.packages.includes(PI_LEGACY_PACKAGE_SPEC)) {
    return {
      status: "unchanged",
      detail: "Pi settings.json has no legacy npm:@tefx/cito package",
    };
  }
  const filtered = obj.packages.filter(item => item !== PI_LEGACY_PACKAGE_SPEC);
  if (filtered.length > 0) {
    obj.packages = filtered;
  } else {
    delete obj.packages;
  }
  return {
    status: "updated",
    nextContent: JSON.stringify(obj, null, 2) + "\n",
    detail: "Removed legacy npm:@tefx/cito from Pi settings.json packages",
  };
}

function transformPiMcpJson(
  existing: string | undefined,
  force: boolean,
): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  const desiredEntry = { command: "cito-mcp", args: [] as string[] };
  if (existing === undefined) {
    return {
      status: "created",
      nextContent: JSON.stringify({ mcpServers: { cito: desiredEntry } }, null, 2) + "\n",
      detail: "Created Pi global.mcp.json with cito-mcp",
    };
  }
  let parsed: unknown;
  try {
    parsed = existing.trim() ? JSON.parse(existing) : {};
  } catch {
    throw new Error("Invalid JSON in Pi global.mcp.json");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Expected JSON object in Pi global.mcp.json");
  }
  const obj = { ...(parsed as Record<string, unknown>) };
  const mcpServers =
    obj.mcpServers && typeof obj.mcpServers === "object" && !Array.isArray(obj.mcpServers)
      ? { ...(obj.mcpServers as Record<string, unknown>) }
      : {};
  const existingCito = mcpServers.cito as { command?: unknown; args?: unknown } | undefined;
  if (existingCito && typeof existingCito === "object" && !Array.isArray(existingCito)) {
    if (existingCito.command === "cito-mcp" && Array.isArray(existingCito.args) && existingCito.args.length === 0) {
      return {
        status: "unchanged",
        detail: "Pi global.mcp.json already configured",
      };
    }
    if (existingCito.command !== undefined && existingCito.command !== "cito-mcp" && !force) {
      return {
        status: "conflict",
        detail: "Conflicting mcpServers.cito command in Pi global.mcp.json; pass --force to replace",
      };
    }
  }
  mcpServers.cito = desiredEntry;
  obj.mcpServers = mcpServers;
  return {
    status: "updated",
    nextContent: JSON.stringify(obj, null, 2) + "\n",
    detail: "Configured mcpServers.cito in Pi global.mcp.json",
  };
}

function removePiMcpJson(existing: string | undefined): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  if (existing === undefined) {
    return {
      status: "unchanged",
      detail: "Pi global.mcp.json does not exist",
    };
  }
  let parsed: unknown;
  try {
    parsed = existing.trim() ? JSON.parse(existing) : {};
  } catch {
    throw new Error("Invalid JSON in Pi global.mcp.json");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Expected JSON object in Pi global.mcp.json");
  }
  const obj = { ...(parsed as Record<string, unknown>) };
  const mcpServers =
    obj.mcpServers && typeof obj.mcpServers === "object" && !Array.isArray(obj.mcpServers)
      ? { ...(obj.mcpServers as Record<string, unknown>) }
      : undefined;
  if (!mcpServers || !("cito" in mcpServers)) {
    return {
      status: "unchanged",
      detail: "Pi global.mcp.json has no mcpServers.cito entry",
    };
  }
  delete mcpServers.cito;
  if (Object.keys(mcpServers).length > 0) {
    obj.mcpServers = mcpServers;
  } else {
    delete obj.mcpServers;
  }
  return {
    status: "updated",
    nextContent: JSON.stringify(obj, null, 2) + "\n",
    detail: "Removed mcpServers.cito from Pi global.mcp.json",
  };
}

function resolvePiPaths(env: NodeJS.ProcessEnv): {
  piHome: string;
  extensionPath: string;
  mcpPath: string;
  settingsPath: string;
} {
  const piHome = expandHomeDir(env.CITO_PI_HOME ?? "~/.pi/agent", "CITO_PI_HOME", env.HOME);
  return {
    piHome,
    extensionPath: join(piHome, "extensions", "cito-bridge.ts"),
    mcpPath: join(piHome, "mcp", "global.mcp.json"),
    settingsPath: join(piHome, "settings.json"),
  };
}

function hasLegacyPiPackage(settingsPath: string): boolean {
  if (!existsSync(settingsPath)) return false;
  try {
    const raw = readFileSync(settingsPath, "utf8");
    const parsed = raw.trim() ? JSON.parse(raw) : {};
    return Boolean(
      parsed &&
      typeof parsed === "object" &&
      !Array.isArray(parsed) &&
      Array.isArray((parsed as { packages?: unknown }).packages) &&
      ((parsed as { packages: unknown[] }).packages).includes(PI_LEGACY_PACKAGE_SPEC),
    );
  } catch {
    return false;
  }
}

async function runPiSetup(
  env: NodeJS.ProcessEnv,
  options: { force?: boolean },
  dryRun: boolean,
): Promise<SetupFileAction[]> {
  const { extensionPath, mcpPath, settingsPath } = resolvePiPaths(env);
  const actions: SetupFileAction[] = [];
  actions.push(
    await mutateConfigFile({
      target: "pi",
      path: extensionPath,
      dryRun,
      transform: content => transformPiExtension(content),
    }),
  );
  actions.push(
    await mutateConfigFile({
      target: "pi",
      path: mcpPath,
      dryRun,
      transform: content => transformPiMcpJson(content, Boolean(options.force)),
    }),
  );
  if (hasLegacyPiPackage(settingsPath)) {
    actions.push(
      await mutateConfigFile({
        target: "pi",
        path: settingsPath,
        dryRun,
        transform: content => cleanLegacyPiSettingsJson(content),
      }),
    );
  }
  return actions;
}

const piSetup: HarnessSetupDescriptor = {
  inspect: async (env: NodeJS.ProcessEnv) => {
    const { piHome, extensionPath, mcpPath, settingsPath } = resolvePiPaths(env);
    const installed = existsSync(piHome) || existsSync(extensionPath) || existsSync(mcpPath) || existsSync(settingsPath);
    let extOk = false;
    let mcpOk = false;
    if (existsSync(extensionPath)) {
      try {
        extOk = readFileSync(extensionPath, "utf8").trim().length > 0;
      } catch {}
    }
    if (existsSync(mcpPath)) {
      try {
        mcpOk = transformPiMcpJson(readFileSync(mcpPath, "utf8"), false).status === "unchanged";
      } catch {}
    }
    return {
      installed,
      configured: extOk && mcpOk && !hasLegacyPiPackage(settingsPath),
      paths: [extensionPath, mcpPath],
    };
  },
  plan: async (env: NodeJS.ProcessEnv, options: { force?: boolean }) => runPiSetup(env, options, true),
  apply: async (env: NodeJS.ProcessEnv, options: { force?: boolean }) => runPiSetup(env, options, false),
  remove: async (env: NodeJS.ProcessEnv) => {
    const { extensionPath, mcpPath, settingsPath } = resolvePiPaths(env);
    const actions: SetupFileAction[] = [];
    if (existsSync(extensionPath)) {
      await rm(extensionPath, { force: true });
      actions.push({
        target: "pi",
        path: extensionPath,
        status: "updated",
        detail: "Removed Pi cito-bridge.ts extension",
      });
    } else {
      actions.push({
        target: "pi",
        path: extensionPath,
        status: "unchanged",
        detail: "Pi cito-bridge.ts extension already absent",
      });
    }
    actions.push(
      await mutateConfigFile({
        target: "pi",
        path: mcpPath,
        dryRun: false,
        transform: content => removePiMcpJson(content),
      }),
    );
    if (existsSync(settingsPath)) {
      actions.push(
        await mutateConfigFile({
          target: "pi",
          path: settingsPath,
          dryRun: false,
          transform: content => cleanLegacyPiSettingsJson(content),
        }),
      );
    }
    return actions;
  },
};

export const piAdapter = new SocketBridgeAdapter("pi", {
  manifest: piManifest,
  pidKeys: ["pi_pid"],
  setup: piSetup,
});

export default piAdapter;
