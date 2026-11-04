// purpose: reusable isolated official nats-server process for integration tests.
// usage: const broker = await startNats(); try { ...broker.url... } finally { await broker.stop(); }
// effects: spawns one loopback-only process and removes its temporary state on stop.
// requires: npm ci (platform binary optional dependency); no Docker or network downloads.
import { getBinaryPath } from "@eplightning/nats-server";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface TestNats {
  url: string;
  wsUrl?: string;
  directory: string;
  env: NodeJS.ProcessEnv;
  stop(): Promise<void>;
}
export async function startNats(config?: string | { configFile: string }, env: NodeJS.ProcessEnv = {}): Promise<TestNats> {
  const directory = await mkdtemp(join(tmpdir(), "cito-nats-"));
  let binary: string;
  try { binary = await getBinaryPath(); }
  catch (error) { await rm(directory, { recursive: true, force: true }); throw error; }
  const args = ["-a", "127.0.0.1", "-p", "-1", "-m", "-1"];
  if (typeof config !== "object") args.push("-js", "-sd", join(directory, "jetstream"));
  if (typeof config === "string") {
    await writeFile(join(directory, "nats.conf"), config);
    args.push("-c", join(directory, "nats.conf"));
  } else if (config) {
    args.push("-c", config.configFile);
  }
  const child = spawn(binary, args, { env: { ...process.env, ...env, CITO_NATS_STORE_DIR: join(directory, "jetstream") }, stdio: ["ignore", "pipe", "pipe"] });
  let log = "";
  let exit: number | null | undefined;
  const exited = new Promise<void>(resolve => child.once("close", code => { exit = code; resolve(); }));
  let stopPromise: Promise<void> | undefined;
  const stop = () => stopPromise ??= (async () => {
    if (exit === undefined) {
      child.kill("SIGTERM");
      const timer = setTimeout(() => child.kill("SIGKILL"), 3000);
      try { await exited; } finally { clearTimeout(timer); }
    }
    await rm(directory, { recursive: true, force: true });
  })();
  try {
    const url = await new Promise<string>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("NATS startup timed out: " + log)), 5000);
      const cleanup = () => clearTimeout(timeout);
      const receive = (chunk: Buffer) => {
        log += chunk.toString();
        if (log.length > 64_000) log = log.slice(-64_000);
        const port = /Listening for client connections on 127\.0\.0\.1:(\d+)/.exec(log)?.[1];
        if (port && log.includes("Server is ready")) { cleanup(); resolve("nats://127.0.0.1:" + port); }
      };
      child.stdout.on("data", receive);
      child.stderr.on("data", receive);
      child.once("error", error => { cleanup(); reject(error); });
      child.once("close", () => { cleanup(); reject(new Error("NATS exited during startup: " + log)); });
    });
    return {
      url, directory,
      wsUrl: /Listening for websocket clients on (ws:\/\/127\.0\.0\.1:\d+)/.exec(log)?.[1],
      env: { CITO_STATE_DIR: join(directory, "state"), CITO_CODEX_HOME: join(directory, "codex") },
      stop,
    };
  } catch (error) { await stop(); throw error; }
}
