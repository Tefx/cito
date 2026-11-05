import { expect, it } from "vitest";
import { fork } from "node:child_process";
import { copyFile, readFile, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { bridgeSocketPath, identifyBridge, SocketBridgeAdapter, type DeliverContext } from "../../src/adapters/index.js";
import { startHttpServer } from "../../src/mcp/server.js";
import { loadConfig } from "../../src/shared/index.js";
import type { ListenerRow } from "../../src/mcp/state.js";
import { fixture } from "../mcp/support.js";
import { isolatedState } from "./support.js";

// The host is intentionally not an npm dependency. CI can point at an installed 1.5.0 package.
const host = process.env.CITO_PI_PACKAGE_DIR ?? join(homedir(), ".pi/agent/install/releases/1.5.0/node_modules/@earendil-works/pi-coding-agent");
const hostAvailable = existsSync(join(host, "dist/core/extensions/loader.js"));

it.skipIf(!hostAvailable)("loads a standalone file through Pi 1.5.0 and verifies /cito command, bindCore sendMessage arguments and socket cleanup", async () => {
  expect(JSON.parse(await readFile(join(host, "package.json"), "utf8")).version).toBe("1.5.0");
  const f = await fixture({ pi: false });
  const http = await startHttpServer({ ...f.config, daemonPort: 0 }, f.service);
  const state = await isolatedState();
  try {
    const standalone = join(state.root, "bridge.ts");
    await copyFile(fileURLToPath(new URL("../../src/pi-extension/cito-bridge.ts", import.meta.url)), standalone);
    for (const stop of ["shutdown", "exit"]) {
      const child = fork(fileURLToPath(new URL("fixtures/pi-runtime.mjs", import.meta.url)), [host, standalone, f.root],
        { env: { ...f.env, CITO_DAEMON_PORT: String(http.port), PI_CODING_AGENT_DIR: join(state.root, "pi-agent") }, silent: true });
      const messages: Record<string, unknown>[] = [];
      let stderr = "";
      child.stderr!.on("data", data => { stderr += data.toString(); });
      child.on("message", value => {
        if (value && typeof value === "object" && !Array.isArray(value)) messages.push(value as Record<string, unknown>);
      });
      const exited = new Promise<number | null>(resolve => child.once("exit", code => resolve(code)));
      const runCmd = async (id: string, args: string) => {
        child.send({ op: "command", id, name: "cito", args });
        await expect.poll(() => messages.find(m => m.type === "command-done" && m.id === id), { timeout: 10_000 }).toBeDefined();
      };
      try {
        await expect.poll(() => {
          if (child.exitCode !== null) throw new Error(`Pi runtime exited: ${stderr}`);
          return messages.find(m => m.type === "ready");
        }, { timeout: 10_000 }).toBeDefined();
        const ready = messages.find(m => m.type === "ready")!;
        expect(ready.command).toEqual({
          name: "cito",
          description: "Toggle or configure Cito session listening",
          completions_empty: [{ value: "on", label: "on" }, { value: "team", label: "team" }, { value: "off", label: "off" }],
          completions_o: [{ value: "on", label: "on" }, { value: "off", label: "off" }],
          completions_te: [{ value: "team", label: "team" }],
          completions_none: [],
        });
        const pid = child.pid!;
        const info = await identifyBridge("pi", pid, f);
        expect(info).toEqual({ ok: true, endpoint_pid: pid, pi_pid: pid, session_id: ready.session_id,
          session_name: "Loader session", cwd: f.root });
        if (stop === "shutdown") {
          await runCmd("toggle-on", "");
          await runCmd("toggle-off", "");
          await runCmd("on-custom", "on Custom listening");
          await runCmd("team-custom", "team Team listening");
          await runCmd("off-trailing", "off extra");
          await runCmd("unknown-sub", "invalid");
          await runCmd("off-clean", "off");
          const notifications = messages.filter(m => m.type === "ui-notify").map(m => [m.level, m.message]);
          expect(notifications).toEqual([
            ["info", expect.stringMatching(/^Cito listening as .+ \(self\)$/)],
            ["info", "Cito stopped listening"],
            ["info", expect.stringMatching(/^Cito listening as .+ \(self\)$/)],
            ["info", expect.stringMatching(/^Cito listening as .+ \(workspace\)$/)],
            ["warning", "Usage: /cito [on [about...] | team [about...] | off]"],
            ["warning", "Usage: /cito [on [about...] | team [about...] | off]"],
            ["info", "Cito stopped listening"],
          ]);
        }
        const row = { endpoint_pid: pid, session_id: info.session_id } as ListenerRow, adapter = new SocketBridgeAdapter("pi");
        const context: DeliverContext = { batchId: "batch", card: "Host-loaded card", config: loadConfig(f.env),
          signal: new AbortController().signal, queueWritten: false, markQueueWritten() {},
          runEffect: async submit => { await submit(); return true; } };
        expect(await adapter.deliver(row, context)).toBe("delivered");
        expect(await adapter.deliver(row, context)).toBe("delivered");
        await expect.poll(() => messages.filter(m => m.type === "core-send-message").length).toBe(1);
        // This is the real loader -> ExtensionRunner -> bindCore callback boundary;
        // interactive TUI transcript insertion and turn execution remain operator checks.
        expect(messages.filter(m => m.type === "core-send-message")).toEqual([{
          type: "core-send-message", message: { customType: "cito-notification", content: "Host-loaded card", display: true },
          options: { triggerTurn: true, deliverAs: "followUp" },
        }]);
        child.send({ op: stop });
        expect(await exited).toBe(0);
        await expect(stat(bridgeSocketPath(f.stateDir, "pi", pid))).rejects.toMatchObject({ code: "ENOENT" });
        if (stop === "shutdown") {
          const receipt = messages.find(m => m.type === "shutdown");
          expect(receipt).toEqual({ type: "shutdown", errors: [] });
        }
      } finally {
        if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
        await exited;
      }
    }
  } finally { await http.close(); await f.close(); await state.cleanup(); }
}, 30_000);
