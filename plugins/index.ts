import { piAdapter } from "./pi/index.js";
import { codexAdapter } from "./codex/index.js";
import { claudeAdapter } from "./claude/index.js";
import { humanAdapter, humanChannelPlugin } from "./human/index.js";
import { telegramPlugin } from "./telegram/index.js";

export * from "./pi/index.js";
export * from "./codex/index.js";
export * from "./claude/index.js";
export * from "./human/index.js";
export * from "./telegram/index.js";

export const BUILTIN_PLUGINS = [piAdapter, codexAdapter, claudeAdapter] as const;
export const BUILTIN_HUMAN_PLUGINS = [humanChannelPlugin, telegramPlugin] as const;
