import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expandHomeDir } from "../../src/shared/index.js";
import { SocketBridgeAdapter, mutateConfigFile } from "../../src/adapters/socket-bridge.js";
import type { HarnessSetupDescriptor, PluginManifest } from "../../src/adapters/types.js";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/pi/plugin.json", import.meta.url);
const piManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

const PI_PACKAGE_SPEC = "npm:@tefx/cito";

function transformPiSettingsJson(existing: string | undefined): {
  status: "created" | "updated" | "unchanged" | "conflict";
  nextContent?: string;
  detail: string;
} {
  if (existing === undefined) {
    return {
      status: "created",
      nextContent: JSON.stringify({ packages: [PI_PACKAGE_SPEC] }, null, 2) + "\n",
      detail: "Created Pi settings.json with @tefx/cito package",
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
  const packages = Array.isArray(obj.packages) ? [...obj.packages] : [];
  if (packages.includes(PI_PACKAGE_SPEC)) {
    return {
      status: "unchanged",
      detail: "Pi settings.json already includes @tefx/cito",
    };
  }
  packages.push(PI_PACKAGE_SPEC);
  obj.packages = packages;
  return {
    status: "updated",
    nextContent: JSON.stringify(obj, null, 2) + "\n",
    detail: "Added npm:@tefx/cito to Pi settings.json packages",
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

function resolvePiPaths(env: NodeJS.ProcessEnv): { piHome: string; settingsPath: string; mcpPath: string } {
  const piHome = expandHomeDir(env.CITO_PI_HOME ?? "~/.pi/agent", "CITO_PI_HOME", env.HOME);
  return {
    piHome,
    settingsPath: join(piHome, "settings.json"),
    mcpPath: join(piHome, "mcp", "global.mcp.json"),
  };
}

const piSetup: HarnessSetupDescriptor = {
  inspect: async (env: NodeJS.ProcessEnv) => {
    const { piHome, settingsPath, mcpPath } = resolvePiPaths(env);
    const installed = existsSync(piHome) || existsSync(settingsPath) || existsSync(mcpPath);
    let settingsOk = false;
    let mcpOk = false;
    if (existsSync(settingsPath)) {
      try {
        settingsOk = transformPiSettingsJson(readFileSync(settingsPath, "utf8")).status === "unchanged";
      } catch {}
    }
    if (existsSync(mcpPath)) {
      try {
        mcpOk = transformPiMcpJson(readFileSync(mcpPath, "utf8"), false).status === "unchanged";
      } catch {}
    }
    return {
      installed,
      configured: settingsOk && mcpOk,
      paths: [settingsPath, mcpPath],
    };
  },
  plan: async (env: NodeJS.ProcessEnv, options: { force?: boolean }) => {
    const { settingsPath, mcpPath } = resolvePiPaths(env);
    const a1 = await mutateConfigFile({
      target: "pi",
      path: settingsPath,
      dryRun: true,
      transform: content => transformPiSettingsJson(content),
    });
    const a2 = await mutateConfigFile({
      target: "pi",
      path: mcpPath,
      dryRun: true,
      transform: content => transformPiMcpJson(content, Boolean(options.force)),
    });
    return [a1, a2];
  },
  apply: async (env: NodeJS.ProcessEnv, options: { force?: boolean }) => {
    const { settingsPath, mcpPath } = resolvePiPaths(env);
    const a1 = await mutateConfigFile({
      target: "pi",
      path: settingsPath,
      dryRun: false,
      transform: content => transformPiSettingsJson(content),
    });
    const a2 = await mutateConfigFile({
      target: "pi",
      path: mcpPath,
      dryRun: false,
      transform: content => transformPiMcpJson(content, Boolean(options.force)),
    });
    return [a1, a2];
  },
};

export const piAdapter = new SocketBridgeAdapter("pi", {
  manifest: piManifest,
  pidKeys: ["pi_pid"],
  setup: piSetup,
});

export default piAdapter;
