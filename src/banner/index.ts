import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { createConnection } from "node:net";
import { chmod, mkdir, mkdtemp, rename, rm, stat, symlink, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import type { CitoConfig } from "../shared/index.js";
import { loadConfig } from "../shared/index.js";
import { acquireFlock, type PosixLock } from "./flock.js";
export { acquireFlock, type PosixLock } from "./flock.js";

export interface BannerPayload {
  target_harness: string;
  target_session_id: string;
  target_title: string;
  from_user: string;
  from_host: string;
  from_title: string;
  message: string;
  effective_wake: boolean;
  count?: number;
}
export interface BannerOptions {
  config?: Pick<CitoConfig, "stateDir">;
  /** Environment for the detached HUD (headless/timing/opener overrides in tests). */
  env?: NodeJS.ProcessEnv;
  /** Source override for installed layouts; defaults to the project Swift source. */
  sourcePath?: string;
  diagnostic?: (message: string) => void;
}
export type BannerResult = { displayed: true } | { displayed: false; reason: string };
const exec = promisify(execFile);
const sourcePath = fileURLToPath(new URL("../macos/cito-banner.swift", import.meta.url));
// tsc emits JS under dist/src but does not copy Swift resources. Support that
// project-native layout as well as direct tsx execution from src/.
const builtSourcePath = fileURLToPath(new URL("../../../src/macos/cito-banner.swift", import.meta.url));

async function exists(path: string): Promise<boolean> {
  try { await stat(path); return true; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return false; throw error; }
}

/** Caller holds banner.lock; compile into an unpublished temp file then rename. */
export async function ensureBannerCompiled(stateDir: string, source?: string): Promise<string> {
  source ??= await exists(sourcePath) ? sourcePath : builtSourcePath;
  const bin = join(stateDir, "bin");
  const binary = join(bin, "cito-banner");
  const sourceStat = await stat(source);
  try { if ((await stat(binary)).mtimeMs >= sourceStat.mtimeMs) return binary; }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  await mkdir(bin, { recursive: true, mode: 0o700 });
  const scratch = await mkdtemp(join(bin, ".banner-build-"));
  try {
    const temporary = join(scratch, "cito-banner");
    await exec("/usr/bin/swiftc", [source, "-o", temporary], { timeout: 120_000, maxBuffer: 1024 * 1024 });
    await chmod(temporary, 0o755);
    await rename(temporary, binary);
  } finally { await rm(scratch, { recursive: true, force: true }); }
  return binary;
}

/** Connection and acknowledgement have independent deadlines. */
async function deliver(socketPath: string, payload: BannerPayload, connectMs: number): Promise<boolean> {
  return new Promise((resolveDelivery) => {
    const socket = createConnection(socketPath);
    let finished = false;
    let response = "";
    let timeout = setTimeout(() => finish(false), connectMs);
    function finish(ok: boolean) {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      socket.destroy();
      resolveDelivery(ok);
    }
    socket.once("error", () => finish(false));
    socket.once("close", () => finish(false));
    socket.once("connect", () => {
      clearTimeout(timeout);
      timeout = setTimeout(() => finish(false), 500);
      socket.write(JSON.stringify(payload) + "\n");
    });
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      response += chunk;
      if (response.length > 4096) { finish(false); return; }
      const end = response.indexOf("\n");
      if (end < 0) return;
      try { finish(JSON.parse(response.slice(0, end)).ok === true); }
      catch { finish(false); }
    });
  });
}

/** Best effort visibility; delivery failures never launch a duplicate instance. */
export async function showBanner(payload: BannerPayload, options: BannerOptions = {}): Promise<BannerResult> {
  const env = { ...process.env, ...options.env };
  const stateDir = resolve((options.config ?? loadConfig(env)).stateDir);
  const sockets = join(stateDir, "sockets");
  await mkdir(sockets, { recursive: true, mode: 0o700 });
  // Node's connect() has the same sun_path limit as bind(). A short-lived
  // directory symlink avoids process-global chdir and supports long state paths.
  // /tmp is deliberately used on macOS: TMPDIR itself can be a long path.
  const alias = await mkdtemp(join(process.platform === "darwin" ? "/tmp" : tmpdir(), "cito-hud-"));
  let lock: PosixLock | null = null;
  const deadline = performance.now() + 2500;
  const skip = (reason: string): BannerResult => {
    (options.diagnostic ?? console.warn)(`cito-banner skipped: ${reason}`);
    return { displayed: false, reason };
  };
  try {
    await symlink(sockets, join(alias, "s"));
    const socket = join(alias, "s", "banner.sock");
    const senderPath = join(sockets, "banner.lock");
    const instancePath = join(sockets, "banner-instance.lock");
    lock = await acquireFlock(senderPath, Math.max(0, deadline - performance.now()));
    if (!lock) return skip("sender lock timeout");
    if (await exists(socket) && await deliver(socket, payload, 300)) return { displayed: true };
    let probe = await acquireFlock(instancePath, 0);
    if (!probe) {
      const readinessDeadline = performance.now() + 500;
      while (performance.now() < readinessDeadline) {
        if (await deliver(socket, payload, Math.min(300, Math.max(1, readinessDeadline - performance.now())))) return { displayed: true };
        await delay(50);
      }
      await lock.release();
      lock = null;
      await delay(300);
      const remaining = deadline - performance.now();
      if (remaining <= 0) return skip("sender retry budget exhausted");
      lock = await acquireFlock(senderPath, remaining);
      if (!lock) return skip("sender re-lock timeout");
      if (await deliver(socket, payload, 100)) return { displayed: true };
      probe = await acquireFlock(instancePath, 0);
      if (!probe) return skip("existing instance remains busy");
    }
    await probe.release();
    // Only a successful instance probe permits removing the stale endpoint.
    try { await unlink(join(sockets, "banner.sock")); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
    const binary = await ensureBannerCompiled(stateDir, options.sourcePath);
    const child = spawn(binary, [], {
      detached: true, stdio: "ignore", env: { ...env, CITO_STATE_DIR: stateDir },
    });
    await new Promise<void>((resolveSpawn, reject) => {
      child.once("error", reject);
      child.once("spawn", resolveSpawn);
    });
    child.unref();
    // First-launch AppKit initialization can exceed one second during concurrent builds.
    const startupDeadline = performance.now() + 2500;
    while (performance.now() < startupDeadline) {
      if (await exists(socket) && await deliver(socket, payload, 300)) return { displayed: true };
      await delay(25);
    }
    return skip("HUD startup/ack timeout");
  } finally {
    await lock?.release();
    await rm(alias, { recursive: true, force: true });
  }
}
