import { ensurePluginsLoaded } from "../adapters/index.js";
import { expandHomeDir, loadConfig, validateHttpUrl, type HubAuthMode } from "../shared/config.js";
import { normalizeIssuer } from "../shared/identity.js";
import { startHubServer, type HubServerOptions } from "./server.js";

export interface ParsedHubCliConfig extends HubServerOptions {
  port: number;
  bind: string;
  authMode: HubAuthMode;
}

export function parseHubCliArgs(
  argv: readonly string[] = process.argv.slice(3),
  env: NodeJS.ProcessEnv = process.env,
): ParsedHubCliConfig {
  let rawPort: string | undefined;
  let rawBind: string | undefined;
  let rawAuth: string | undefined;
  let rawToken: string | undefined;
  let rawIssuer: string | undefined;
  let rawClientId: string | undefined;
  let rawDataDir: string | undefined;

  const readVal = (arg: string, idx: number, flag: string): [string, number] => {
    if (arg.startsWith(flag + "=")) {
      return [arg.slice(flag.length + 1), idx];
    }
    const next = argv[idx + 1];
    if (next === undefined || next.startsWith("-")) {
      throw new Error(`Missing value for ${flag}`);
    }
    return [next, idx + 1];
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg === "--port" || arg.startsWith("--port=")) {
      [rawPort, i] = readVal(arg, i, "--port");
    } else if (arg === "--bind" || arg.startsWith("--bind=")) {
      [rawBind, i] = readVal(arg, i, "--bind");
    } else if (arg === "--auth" || arg.startsWith("--auth=")) {
      [rawAuth, i] = readVal(arg, i, "--auth");
    } else if (arg === "--token" || arg.startsWith("--token=")) {
      [rawToken, i] = readVal(arg, i, "--token");
    } else if (arg === "--issuer" || arg.startsWith("--issuer=")) {
      [rawIssuer, i] = readVal(arg, i, "--issuer");
    } else if (arg === "--client-id" || arg.startsWith("--client-id=")) {
      [rawClientId, i] = readVal(arg, i, "--client-id");
    } else if (arg === "--data-dir" || arg.startsWith("--data-dir=")) {
      [rawDataDir, i] = readVal(arg, i, "--data-dir");
    } else if (arg.startsWith("-")) {
      throw new Error(`Unknown flag: ${arg}`);
    } else {
      throw new Error(`Unexpected argument: ${arg}`);
    }
  }

  const authStr = (rawAuth ?? env.CITO_AUTH_MODE ?? "none").trim();
  if (authStr !== "none" && authStr !== "token" && authStr !== "oidc") {
    throw new Error("CITO_AUTH_MODE must be none, token, or oidc");
  }
  const authMode: HubAuthMode = authStr;

  const portStr = rawPort ?? env.CITO_HUB_PORT ?? "4291";
  const portNum = Number(portStr);
  if (!portStr.trim() || !Number.isInteger(portNum) || portNum < 0 || portNum > 65535) {
    throw new Error("CITO_HUB_PORT must be an integer in 0..65535");
  }

  const bindStr = (rawBind ?? env.CITO_HUB_BIND ?? (authMode === "none" ? "127.0.0.1" : "0.0.0.0")).trim();
  if (!bindStr) {
    throw new Error("CITO_HUB_BIND must not be empty");
  }

  const token = rawToken ?? env.CITO_HUB_TOKEN;
  if (token !== undefined && !token.trim()) {
    throw new Error("CITO_HUB_TOKEN must not be empty");
  }
  if (authMode === "token" && !token?.trim()) {
    throw new Error("CITO_HUB_TOKEN is required in token mode");
  }

  const issuerInput = rawIssuer ?? env.CITO_OIDC_ISSUER;
  const clientIdInput = rawClientId ?? env.CITO_OIDC_CLIENT_ID;
  let oidcIssuer: string | undefined;
  let oidcClientId: string | undefined;
  if (authMode === "oidc" || issuerInput !== undefined) {
    if (!issuerInput?.trim()) {
      throw new Error("CITO_OIDC_ISSUER is required in oidc mode");
    }
    oidcIssuer = normalizeIssuer(validateHttpUrl(issuerInput.trim(), "CITO_OIDC_ISSUER"));
    if (!clientIdInput?.trim()) {
      throw new Error("CITO_OIDC_CLIENT_ID is required in oidc mode");
    }
    oidcClientId = clientIdInput.trim();
  }

  const dataDirInput = rawDataDir ?? env.CITO_HUB_DATA_DIR;
  const dataDir = dataDirInput !== undefined ? expandHomeDir(dataDirInput, "CITO_HUB_DATA_DIR", env.HOME) : undefined;

  return {
    port: portNum,
    bind: bindStr,
    authMode,
    ...(token !== undefined ? { token: token.trim() } : {}),
    ...(oidcIssuer !== undefined ? { oidcIssuer } : {}),
    ...(oidcClientId !== undefined ? { oidcClientId } : {}),
    ...(dataDir !== undefined ? { dataDir } : {}),
  };
}

export async function runHubCli(
  argv: readonly string[] = process.argv.slice(3),
  env: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  let config: ParsedHubCliConfig;
  try {
    config = parseHubCliArgs(argv, env);
  } catch (error) {
    process.stderr.write("cito hub: " + (error instanceof Error ? error.message : String(error)) + "\n");
    process.exitCode = 1;
    return;
  }

  try {
    try {
      await ensurePluginsLoaded(loadConfig(env));
    } catch { /* Standalone hub without local config.json falls back to default stateDir plugins. */ }
    const server = await startHubServer({
      ...config,
      onError: err => {
        process.stderr.write("cito hub: " + err.message + "\n");
      },
    });
    const displayHost = server.bind === "0.0.0.0" || server.bind === "::" ? "<host>" : server.bind;
    const joinUrl = `http://${displayHost}:${server.port}`;
    const joinCmd =
      config.authMode === "oidc"
        ? `cito setup --hub team --url ${joinUrl} --auth oidc --issuer ${config.oidcIssuer} --client-id ${config.oidcClientId}`
        : config.authMode === "token"
        ? `cito setup --hub tailnet --url ${joinUrl} --auth token --token <token>`
        : `cito setup --hub tailnet --url ${joinUrl}`;

    process.stderr.write(`cito hub: ready on http://${server.bind}:${server.port} (auth=${config.authMode})\n`);
    process.stderr.write(`cito hub: join with: ${joinCmd}\n`);

    await new Promise<void>(resolve => {
      const stop = () => {
        process.removeListener("SIGINT", stop);
        process.removeListener("SIGTERM", stop);
        void server.close().finally(resolve);
      };
      process.once("SIGINT", stop);
      process.once("SIGTERM", stop);
    });
  } catch (error) {
    process.stderr.write("cito hub: " + (error instanceof Error ? error.message : String(error)) + "\n");
    process.exitCode = 1;
  }
}

export const main = () => runHubCli();
