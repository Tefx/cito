import { existsSync, readFileSync } from "node:fs";
import { SocketBridgeAdapter } from "../../src/adapters/socket-bridge.js";
import type { PluginManifest } from "../../src/adapters/types.js";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/pi/plugin.json", import.meta.url);
export const piManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

export const piAdapter = new SocketBridgeAdapter("pi", {
  manifest: piManifest,
  pidKeys: ["pi_pid"],
});

export default piAdapter;
