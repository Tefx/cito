import { describe, expect, it } from "vitest";
import { createAccount, createOperator, createUser } from "nkeys.js";
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, lstat, mkdtemp, readFile, realpath, rm, symlink } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { connect } from "nats";
import { startNats } from "../shared/nats-server.js";
import { signingKey, startOidc } from "./oidc-fixture.js";

const decoder = new TextDecoder();
function isolatedEnv(directory: string): NodeJS.ProcessEnv {
  const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("CITO_")));
  return { ...env, CITO_STATE_DIR: join(directory, "state"), CITO_CODEX_HOME: join(directory, "codex") };
}
async function entrypoint(): Promise<string> {
  const pkg = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8"));
  return resolve(pkg.bin["cito-auth-callout"]);
}
describe("public cito-auth-callout executable", () => {
  it("fails before network access with exit 1 and a diagnostic for missing/malformed config", async () => {
    const directory = await mkdtemp(join(tmpdir(), "cito-cli-"));
    const account = createAccount();
    const user = createUser();
    const operator = createOperator();
    try {
      const base = { ...isolatedEnv(directory), CITO_AUTH_MODE: "oidc",
        CITO_OIDC_ISSUER: "http://127.0.0.1:9", CITO_OIDC_CLIENT_ID: "cito",
        CITO_AUTH_CALLOUT_ISSUER_SEED: decoder.decode(account.getSeed()), CITO_AUTH_CALLOUT_PASSWORD: "test",
      };
      const cases: [NodeJS.ProcessEnv, RegExp][] = [
        [{ CITO_AUTH_MODE: "other" }, /CITO_AUTH_MODE/],
        [{ CITO_USER: "invalid user" }, /CITO_USER/],
        [{ CITO_NATS_URL: "garbage" }, /CITO_NATS_URL/],
        [{ CITO_NATS_URL: "http://example" }, /CITO_NATS_URL/],
        [{ CITO_OIDC_ISSUER: "" }, /CITO_OIDC_ISSUER/],
        [{ CITO_OIDC_ISSUER: "http://example" }, /CITO_OIDC_ISSUER/],
        [{ CITO_OIDC_CLIENT_ID: "" }, /CITO_OIDC_CLIENT_ID/],
        [{ CITO_AUTH_CALLOUT_ISSUER_SEED: "" }, /CITO_AUTH_CALLOUT_ISSUER_SEED/],
        [{ CITO_AUTH_CALLOUT_ISSUER_SEED: "SA-malformed" }, /Account seed/],
        [{ CITO_AUTH_CALLOUT_ISSUER_SEED: decoder.decode(user.getSeed()) }, /Account seed/],
        [{ CITO_AUTH_CALLOUT_ISSUER_SEED: decoder.decode(operator.getSeed()) }, /nats-server rejects Operator/],
        [{ CITO_AUTH_CALLOUT_XKEY_SEED: "" }, /XKey seed|required/],
        [{ CITO_AUTH_CALLOUT_XKEY_SEED: "SX-malformed" }, /XKey seed/],
        [{ CITO_AUTH_CALLOUT_XKEY_SEED: decoder.decode(account.getSeed()) }, /XKey seed/],
        [{ CITO_AUTH_CALLOUT_PASSWORD: "" }, /CITO_AUTH_CALLOUT_PASSWORD/],
      ];
      const bin = await entrypoint();
      for (const [overrides, message] of cases) {
        const result = spawnSync(bin, [], { cwd: directory, env: { ...base, ...overrides }, encoding: "utf8", timeout: 3000 });
        expect(result.error).toBeUndefined();
        expect(result.status).toBe(1);
        expect(result.stdout).toBe("");
        expect(result.stderr).toMatch(message);
        expect(result.stderr).not.toContain(base.CITO_AUTH_CALLOUT_ISSUER_SEED);
      }
    } finally {
      account.clear(); user.clear(); operator.clear();
      await rm(directory, { recursive: true, force: true });
    }
  }, 15_000);
  it("boots through the real CLI, serves OIDC authentication and drains on SIGTERM", async () => {
    const key = await signingKey("ES256", "ec");
    const oidc = await startOidc([key.jwk]);
    const account = createAccount();
    const broker = await startNats({ configFile: resolve("deploy/nats/nats-server.oidc.conf") }, {
      CITO_AUTH_CALLOUT_ISSUER: account.getPublicKey(), CITO_AUTH_CALLOUT_PASSWORD: "test",
    });
    let child: ReturnType<typeof spawn> | undefined;
    let exited: Promise<number | null> | undefined;
    let client: Awaited<ReturnType<typeof connect>> | undefined;
    try {
      child = spawn(await entrypoint(), [], {
        cwd: broker.directory,
        env: { ...isolatedEnv(broker.directory), ...broker.env,
          CITO_NATS_URL: broker.url, CITO_AUTH_MODE: "oidc",
          CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito",
          CITO_AUTH_CALLOUT_ISSUER_SEED: decoder.decode(account.getSeed()), CITO_AUTH_CALLOUT_PASSWORD: "test",
        },
        stdio: ["ignore", "pipe", "pipe"],
      });
      exited = new Promise(resolve => child!.once("close", resolve));
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("CLI did not become ready")), 5000);
        const cleanup = () => clearTimeout(timeout);
        let stderr = "";
        child!.stderr!.on("data", data => {
          stderr += String(data);
          if (stderr.includes("cito-auth-callout: ready")) { cleanup(); resolve(); }
        });
        child!.once("error", error => { cleanup(); reject(error); });
        void exited!.then(code => { cleanup(); reject(new Error("CLI exited " + code + ": " + stderr)); });
      });
      client = await connect({ servers: broker.url, token: await key.sign(oidc.claims()), timeout: 2000 });
      await client.flush();
      child.kill("SIGTERM");
      expect(await exited).toBe(0);
    } finally {
      await client?.close();
      if (child?.exitCode === null && child.signalCode === null) { child.kill("SIGKILL"); await exited; }
      await broker.stop(); await oidc.stop(); account.clear();
    }
  }, 10_000);
  it("runs the no-auth deployment config without any authorization credentials", async () => {
    const broker = await startNats({ configFile: resolve("deploy/nats/nats-server.conf") });
    let client;
    try {
      client = await connect({ servers: broker.url });
      const info = await (await client.jetstreamManager()).getAccountInfo();
      expect(info.streams).toBe(0);
    } finally { await client?.close(); await broker.stop(); }
  });
  it("enforces zero-script git packaging contract and executes native TypeScript entrypoints without dist/", async () => {
    const pkg = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8"));
    expect(pkg.files).toEqual(["bin", "dist/src", "src", "README.md"]);
    for (const forbidden of ["prepare", "build", "prepack", "preinstall", "install", "postinstall"]) {
      expect(pkg.scripts?.[forbidden]).toBeUndefined();
    }
    expect(Object.keys(pkg.scripts ?? {})).toEqual(["test", "typecheck", "compile", "prepublishOnly"]);
    expect(pkg.scripts?.compile).toBe("rm -rf dist && tsc && if [ \"$(uname -s)-$(uname -m)\" = \"Darwin-arm64\" ]; then swiftc -O -target arm64-apple-macos13.0 src/macos/cito-ui.swift -o bin/cito-ui-darwin-arm64 && strip -x bin/cito-ui-darwin-arm64 && codesign -s - --force bin/cito-ui-darwin-arm64; fi");
    expect(pkg.scripts?.prepublishOnly).toBe("npm run typecheck && npm run compile");
    expect(pkg.dependencies?.tsx).toBeUndefined();

    const directory = await mkdtemp(join(tmpdir(), "cito-pkg-"));
    try {
      const sandbox = join(directory, "sandbox");
      for (const item of ["package.json", "bin", "src"]) {
        await cp(resolve(item), join(sandbox, item), { recursive: true });
      }
      await symlink(resolve("node_modules"), join(sandbox, "node_modules"), "dir");
      expect(existsSync(join(sandbox, "dist"))).toBe(false);

      const mcpSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-mcp"), "listen", "--unknown-flag"], {
        cwd: directory,
        env: isolatedEnv(directory),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(mcpSandbox.error).toBeUndefined();
      expect(mcpSandbox.status).toBe(1);
      expect(mcpSandbox.stderr).toContain("Unknown flag: --unknown-flag");

      const authSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-auth-callout")], {
        cwd: directory,
        env: { ...isolatedEnv(directory), CITO_AUTH_MODE: "other" },
        encoding: "utf8",
        timeout: 5000,
      });
      expect(authSandbox.error).toBeUndefined();
      expect(authSandbox.status).toBe(1);
      expect(authSandbox.stderr).toMatch(/cito-auth-callout:.*CITO_AUTH_MODE/);

      const hookSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-claude-hook")], {
        cwd: directory,
        env: isolatedEnv(directory),
        input: JSON.stringify({ hook_event_name: "Stop", cwd: directory }),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(hookSandbox.error).toBeUndefined();
      expect(hookSandbox.status).toBe(0);
      expect(hookSandbox.stdout).toBe("{}\n");

      const repo = join(directory, "repo");
      const prefix = join(directory, "prefix");
      for (const item of ["package.json", "package-lock.json", "tsconfig.json", "README.md", "bin", "src"]) {
        await cp(resolve(item), join(repo, item), { recursive: true });
      }
      for (const args of [
        ["init", "-q"],
        ["config", "user.email", "test@example.com"],
        ["config", "user.name", "test"],
        ["add", "."],
        ["commit", "-q", "-m", "init"],
      ]) {
        const git = spawnSync("git", args, { cwd: repo, encoding: "utf8" });
        expect(git.status).toBe(0);
      }

      const install = spawnSync(
        "npm",
        ["install", "-g", "--prefix", prefix, "--prefer-offline", "--no-audit", "--no-fund", `git+file://${repo}`],
        { cwd: directory, encoding: "utf8", timeout: 30_000 },
      );
      expect(install.error).toBeUndefined();
      expect(install.status).toBe(0);

      const installedPkgRoot = await realpath(join(prefix, "lib/node_modules/@tefx/cito"));
      expect(existsSync(join(installedPkgRoot, "dist"))).toBe(false);
      expect(existsSync(join(installedPkgRoot, "bin/cito-ui-darwin-arm64"))).toBe(true);
      for (const [binName, targetRel] of [
        ["cito", "bin/cito-mcp"],
        ["cito-mcp", "bin/cito-mcp"],
        ["cito-auth-callout", "bin/cito-auth-callout"],
        ["cito-claude-hook", "bin/cito-claude-hook"],
      ] as const) {
        const binPath = join(prefix, "bin", binName);
        expect((await lstat(binPath)).isSymbolicLink()).toBe(true);
        expect(await realpath(binPath)).toBe(join(installedPkgRoot, targetRel));
      }

      const installedCito = spawnSync(join(prefix, "bin/cito"), ["listen", "--unknown-flag"], {
        cwd: directory,
        env: isolatedEnv(directory),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(installedCito.error).toBeUndefined();
      expect(installedCito.status).toBe(1);
      expect(installedCito.stderr).toContain("Unknown flag: --unknown-flag");

      const installedAuthCallout = spawnSync(join(prefix, "bin/cito-auth-callout"), [], {
        cwd: directory,
        env: { ...isolatedEnv(directory), CITO_AUTH_MODE: "other" },
        encoding: "utf8",
        timeout: 5000,
      });
      expect(installedAuthCallout.error).toBeUndefined();
      expect(installedAuthCallout.status).toBe(1);
      expect(installedAuthCallout.stderr).toMatch(/cito-auth-callout:.*CITO_AUTH_MODE/);

      const installedHook = spawnSync(join(prefix, "bin/cito-claude-hook"), [], {
        cwd: directory,
        env: isolatedEnv(directory),
        input: JSON.stringify({ hook_event_name: "Stop", cwd: directory }),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(installedHook.error).toBeUndefined();
      expect(installedHook.status).toBe(0);
      expect(installedHook.stdout).toBe("{}\n");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }, 45_000);
});
