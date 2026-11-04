// purpose: Exercise the installed Pi 1.5.0 loader and event runner in an isolated process.
// usage: node pi-runtime.mjs <Pi package dir> <standalone bridge.ts> <temp cwd>
// effects: Opens one temporary bridge socket; returns observations over Node IPC.
// requires: Installed Pi 1.5.0, CITO_STATE_DIR/CITO_CODEX_HOME/PI_CODING_AGENT_DIR in temp state.
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const [host, extension, cwd] = process.argv.slice(2);
const { loadExtensions } = await import(pathToFileURL(join(host, "dist/core/extensions/loader.js")));
const { ExtensionRunner } = await import(pathToFileURL(join(host, "dist/core/extensions/runner.js")));
const { SessionManager } = await import(pathToFileURL(join(host, "dist/core/session-manager.js")));
const loaded = await loadExtensions([extension], cwd);
if (loaded.errors.length) throw new Error(JSON.stringify(loaded.errors));
const manager = SessionManager.inMemory(cwd);
manager.appendSessionInfo("Loader session");
const runner = new ExtensionRunner(loaded.extensions, loaded.runtime, cwd, manager, {});
const errors = [];
runner.onError(error => errors.push(error));
runner.bindCore({ ...loaded.runtime, sendMessage: (message, options) => {
  manager.appendCustomMessageEntry(message.customType, message.content, message.display, message.details);
  process.send({ type: "delivered", message, options });
} }, {
  getModel: () => undefined, getScopedModels: () => [], isIdle: () => true,
  isProjectTrusted: () => true, getSignal: () => undefined,
  abort: async () => {}, hasPendingMessages: () => false, shutdown: () => {},
  getContextUsage: () => undefined, compact: () => {}, getSystemPrompt: () => "",
});
await runner.emit({ type: "session_start", reason: "startup" });
if (errors.length) throw new Error(JSON.stringify(errors));
process.send({ type: "ready", session_id: manager.getSessionId() });
process.on("message", async message => {
  try {
    if (message.op === "input") {
      await runner.emitInput("test input", undefined, message.source);
      process.send({ type: "input-done" });
    } else if (message.op === "shutdown") {
      await runner.emit({ type: "session_shutdown", reason: "quit" });
      process.send({ type: "shutdown", errors, entries: manager.getEntries() }, () => process.exit(0));
    } else if (message.op === "exit") {
      // Deliberately skip the host shutdown event: verify the process exit fallback.
      process.exit(0);
    }
  } catch (error) {
    process.send({ type: "error", error: String(error) });
    process.exit(1);
  }
});
