import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import type { CitoConfig } from "../../src/shared/index.js";
import type { ListenerRow } from "../../src/shared/state-types.js";
import type {
  Caller,
  CallerDetectionContext,
  DeliverContext,
  DeliverOutcome,
  HarnessAdapter,
  HumanChannelPlugin,
  PluginManifest,
  ProbeOutcome,
} from "../../src/adapters/types.js";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/human/plugin.json", import.meta.url);
export const humanManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

export class HumanNodeAdapter implements HarnessAdapter, HumanChannelPlugin {
  readonly id = "human";
  readonly kind = "human" as const;
  readonly manifest: PluginManifest = humanManifest;

  async detectExplicitCaller(ctx: CallerDetectionContext, config: CitoConfig): Promise<Caller | null> {
    if (ctx.meta.expected_harness !== "human") return null;
    const sessionId =
      typeof ctx.meta.expected_session_id === "string" && ctx.meta.expected_session_id.trim()
        ? ctx.meta.expected_session_id.trim()
        : "default";
    const user = (config.user || "user").toLowerCase();
    const title =
      typeof ctx.meta.title === "string" && ctx.meta.title.trim()
        ? ctx.meta.title.trim()
        : `@${user} (Me)`;
    const cwd =
      typeof ctx.meta.cwd === "string" && ctx.meta.cwd.trim()
        ? ctx.meta.cwd.trim()
        : homedir();
    return {
      harness: "human",
      session_id: sessionId,
      cwd,
      title,
      repo: null,
      branch: null,
    };
  }

  async probe(_row: ListenerRow, _config: CitoConfig, _signal: AbortSignal): Promise<ProbeOutcome> {
    return { status: "alive" };
  }

  async deliver(_row: ListenerRow, _ctx: DeliverContext): Promise<DeliverOutcome> {
    return "delivered_unconfirmed";
  }

  async sendNotification() {
    return { delivered: true };
  }
}

export const humanAdapter = new HumanNodeAdapter();
export const humanChannelPlugin: HumanChannelPlugin = humanAdapter;
export default humanAdapter;
