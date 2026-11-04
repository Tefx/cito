import { expect, it } from "vitest";
import { fork } from "node:child_process";
import { copyFile, readFile, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { deliverPiWake, identifyPi, piSocketPath } from "../../src/adapters/index.js";
import { isolatedState } from "./support.js";

// The host is intentionally not an npm dependency. CI can point at an installed 1.5.0 package.
const host = process.env.CITO_PI_PACKAGE_DIR ?? join(homedir(), ".pi/agent/install/releases/1.5.0/node_modules/@earendil-works/pi-coding-agent");
const hostAvailable = existsSync(join(host, "dist/core/extensions/loader.js"));

it.skipIf(!hostAvailable)("loads a standalone file with Pi 1.5.0's real loader, dispatches events, persists custom entries, and cleans exit sockets", async () => {
  expect(JSON.parse(await readFile(join(host, "package.json"), "utf8")).version).toBe("1.5.0");
  const state = await isolatedState();
  try {
    const standalone = join(state.root, "bridge.ts");
    await copyFile(fileURLToPath(new URL("../../src/pi-extension/cito-bridge.ts", import.meta.url)), standalone);
    for (const stop of ["shutdown", "exit"]) {
      const child = fork(fileURLToPath(new URL("fixtures/pi-runtime.mjs", import.meta.url)), [host, standalone, state.root],
        { env: { ...state.env, PI_CODING_AGENT_DIR: join(state.root, "pi-agent") }, silent: true });
      const messages: Record<string, unknown>[] = [];
      let stderr = "";
      child.stderr!.on("data", data => { stderr += data.toString(); });
      child.on("message", value => {
        if (value && typeof value === "object" && !Array.isArray(value)) messages.push(value as Record<string, unknown>);
      });
      const exited = new Promise<number | null>(resolve => child.once("exit", code => resolve(code)));
      try {
        await expect.poll(() => {
          if (child.exitCode !== null) throw new Error(`Pi runtime exited: ${stderr}`);
          return messages.find(m => m.type === "ready");
        }, { timeout: 10_000 }).toBeDefined();
        const pid = child.pid!;
        const info = await identifyPi(pid, state);
        expect(info).toMatchObject({ session_name: "Loader session", cwd: state.root, human_event_count: 0 });
        child.send({ op: "input", source: "extension" });
        await expect.poll(() => messages.filter(m => m.type === "input-done").length).toBe(1);
        child.send({ op: "input", source: "interactive" });
        await expect.poll(() => messages.filter(m => m.type === "input-done").length).toBe(2);
        expect((await identifyPi(pid, state)).human_event_count).toBe(1);
        const endpoint = { pi_pid: pid, session_id: info.session_id };
        expect(await deliverPiWake(endpoint, "batch", "Host-loaded card", state)).toMatchObject({ deduplicated: false });
        expect(await deliverPiWake(endpoint, "batch", "Host-loaded card", state)).toMatchObject({ deduplicated: true });
        await expect.poll(() => messages.filter(m => m.type === "delivered").length).toBe(1);
        expect(messages.find(m => m.type === "delivered")).toMatchObject({
          message: { customType: "cito-notification", content: "Host-loaded card", display: true },
          options: { triggerTurn: true, deliverAs: "followUp" },
        });
        child.send({ op: stop });
        expect(await exited).toBe(0);
        await expect(stat(piSocketPath(state.stateDir, pid))).rejects.toMatchObject({ code: "ENOENT" });
        if (stop === "shutdown") {
          const receipt = messages.find(m => m.type === "shutdown");
          expect(receipt).toMatchObject({ errors: [], entries: expect.arrayContaining([
            expect.objectContaining({ type: "custom_message", customType: "cito-notification", content: "Host-loaded card" }),
          ]) });
        }
      } finally {
        if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
        await exited;
      }
    }
  } finally { await state.cleanup(); }
}, 30_000);
