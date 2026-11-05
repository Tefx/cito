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
runner.bindCommandContext();
runner.setUIContext({
  select: async () => undefined, confirm: async () => false, input: async () => undefined,
  notify: (message, type) => { process.send({ type: "ui-notify", message, level: type }); },
  onTerminalInput: () => () => {},
  setStatus: (key, value) => { process.send({ type: "ui-status", key, value }); },
  setWorkingMessage: () => {}, setWorkingVisible: () => {}, setWorkingIndicator: () => {},
  setHiddenThinkingLabel: () => {}, setWidget: () => {}, setFooter: () => {}, setHeader: () => {},
  setTitle: () => {}, custom: async () => undefined, pasteToEditor: () => {}, setEditorText: () => {},
  getEditorText: () => "", editor: async () => undefined, get theme() { return {}; },
  getAllThemes: () => [], getTheme: () => undefined, setTheme: () => ({ success: false }),
  getToolsExpanded: () => false, setToolsExpanded: () => {},
}, "interactive");
// Observe the real ExtensionRunner.bindCore sendMessage boundary. The interactive
// TUI owns transcript insertion and turn execution; this fixture does neither.
runner.bindCore({ ...loaded.runtime, sendMessage: (message, options) => {
  process.send({ type: "core-send-message", message, options });
} }, {
  getModel: () => undefined, getScopedModels: () => [], isIdle: () => true,
  isProjectTrusted: () => true, getSignal: () => undefined,
  abort: async () => {}, hasPendingMessages: () => false, shutdown: () => {},
  getContextUsage: () => undefined, compact: () => {}, getSystemPrompt: () => "",
});
await runner.emit({ type: "session_start", reason: "startup" });
if (errors.length) throw new Error(JSON.stringify(errors));
const citoCmd = runner.getCommand("cito");
process.send({
  type: "ready",
  session_id: manager.getSessionId(),
  command: citoCmd ? {
    name: citoCmd.name,
    description: citoCmd.description,
    completions_empty: await citoCmd.getArgumentCompletions?.(""),
    completions_o: await citoCmd.getArgumentCompletions?.(" o "),
    completions_te: await citoCmd.getArgumentCompletions?.("te"),
    completions_none: await citoCmd.getArgumentCompletions?.("xyz"),
  } : null,
});
process.on("message", async message => {
  try {
    if (message.op === "command") {
      const cmd = runner.getCommand(message.name);
      await cmd.handler(message.args ?? "", runner.createCommandContext());
      process.send({ type: "command-done", id: message.id });
    } else if (message.op === "shutdown") {
      await runner.emit({ type: "session_shutdown", reason: "quit" });
      process.send({ type: "shutdown", errors }, () => process.exit(0));
    } else if (message.op === "exit") {
      // Deliberately skip the host shutdown event: verify the process exit fallback.
      process.exit(0);
    }
  } catch (error) {
    process.send({ type: "error", error: String(error) });
    process.exit(1);
  }
});
