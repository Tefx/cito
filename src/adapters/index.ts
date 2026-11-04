import { loadConfig, validateHarness, type CitoConfig, type Harness } from "../shared/index.js";
import { resolveCodexCaller, type CodexCaller } from "./codex.js";
import { resolvePiCaller, type PiCaller } from "./pi.js";

export * from "./codex.js";
export * from "./git.js";
export * from "./pi.js";
export * from "./wake-card.js";

/** Receiver tools only. Outbound routing must use the target's registered identity. */
export async function resolveCaller(harness: Harness, meta: Record<string, unknown> = {},
  config: CitoConfig = loadConfig()): Promise<CodexCaller | PiCaller> {
  return validateHarness(harness) === "codex" ? resolveCodexCaller(meta, config) : resolvePiCaller(meta, config);
}
