// purpose: reusable isolated in-process HubServer for integration tests.
// usage: const broker = await startNats(); try { ...broker.url... } finally { await broker.stop(); }
// effects: starts a loopback HTTP/SSE HubServer and removes its temporary state on stop.
// requires: Node built-in http and sqlite; zero external binary dependencies.
import { startHubServer, type HubServer, type HubServerOptions } from "../../src/hub/server.js";

export type TestNats = HubServer;

function isLegacyConfigFile(value: object): value is { configFile?: string } {
  return "configFile" in value;
}

export async function startNats(
  config?: string | { configFile?: string } | HubServerOptions,
  _env: NodeJS.ProcessEnv = {},
): Promise<TestNats> {
  const opts: HubServerOptions =
    config && typeof config === "object" && !isLegacyConfigFile(config)
      ? config
      : {};
  const server = await startHubServer(opts);
  return {
    ...server,
    env: {
      ...server.env,
      CITO_CODEX_HOME: `${server.directory}/codex`,
    },
  };
}

export { startHubServer };
