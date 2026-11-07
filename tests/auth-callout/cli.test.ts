import { describe, expect, it } from "vitest";
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, lstat, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { connectNats } from "../../src/shared/nats.js";
import { startNats } from "../shared/nats-server.js";
import { signingKey, startOidc } from "./oidc-fixture.js";

function isolatedEnv(directory: string): NodeJS.ProcessEnv {
  const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("CITO_")));
  return { ...env, CITO_STATE_DIR: join(directory, "state"), CITO_CODEX_HOME: join(directory, "codex") };
}
async function entrypoint(): Promise<string> {
  const pkg = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8"));
  return resolve(pkg.bin.cito);
}
describe("cito hub CLI and zero-script packaging contract", () => {
  it("fails before network access with exit 1 and a diagnostic for missing/malformed config", async () => {
    const directory = await mkdtemp(join(tmpdir(), "cito-cli-"));
    try {
      const secretToken = "super-secret-token-value-123";
      const base = {
        ...isolatedEnv(directory),
        CITO_AUTH_MODE: "oidc",
        CITO_OIDC_ISSUER: "http://127.0.0.1:9",
        CITO_OIDC_CLIENT_ID: "cito",
        CITO_HUB_TOKEN: secretToken,
      };
      const cases: [string[], NodeJS.ProcessEnv, RegExp][] = [
        [["hub"], { CITO_AUTH_MODE: "other" }, /CITO_AUTH_MODE/],
        [["hub"], { CITO_HUB_PORT: "invalid" }, /CITO_HUB_PORT/],
        [["hub"], { CITO_HUB_PORT: "70000" }, /CITO_HUB_PORT/],
        [["hub"], { CITO_HUB_BIND: "   " }, /CITO_HUB_BIND/],
        [["hub"], { CITO_OIDC_ISSUER: "" }, /CITO_OIDC_ISSUER/],
        [["hub"], { CITO_OIDC_ISSUER: "http://example" }, /CITO_OIDC_ISSUER/],
        [["hub"], { CITO_OIDC_CLIENT_ID: "" }, /CITO_OIDC_CLIENT_ID/],
        [["hub"], { CITO_AUTH_MODE: "token", CITO_HUB_TOKEN: "" }, /CITO_HUB_TOKEN/],
        [["hub"], { CITO_AUTH_MODE: "token", CITO_HUB_TOKEN: "   " }, /CITO_HUB_TOKEN/],
        [["hub", "--unknown-flag"], {}, /Unknown flag: --unknown-flag/],
        [["hub", "positional"], {}, /Unexpected argument: positional/],
      ];
      const bin = await entrypoint();
      for (const [args, overrides, message] of cases) {
        const result = spawnSync(bin, args, {
          cwd: directory,
          env: { ...base, ...overrides },
          encoding: "utf8",
          timeout: 3000,
        });
        expect(result.error).toBeUndefined();
        expect(result.status).toBe(1);
        expect(result.stdout).toBe("");
        expect(result.stderr).toMatch(message);
        expect(result.stderr).not.toContain(secretToken);
      }
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }, 15_000);

  it("boots through the real cito hub CLI, serves OIDC authentication and exits cleanly on SIGTERM", async () => {
    const key = await signingKey("ES256", "ec");
    const oidc = await startOidc([key.jwk]);
    const directory = await mkdtemp(join(tmpdir(), "cito-hub-cli-"));
    let child: ReturnType<typeof spawn> | undefined;
    let exited: Promise<number | null> | undefined;
    let client: Awaited<ReturnType<typeof connectNats>> | undefined;
    try {
      child = spawn(await entrypoint(), ["hub", "--port", "0", "--bind", "127.0.0.1", "--auth", "oidc"], {
        cwd: directory,
        env: {
          ...isolatedEnv(directory),
          CITO_OIDC_ISSUER: oidc.issuer,
          CITO_OIDC_CLIENT_ID: "cito",
          CITO_HUB_DATA_DIR: join(directory, "data"),
        },
        stdio: ["ignore", "pipe", "pipe"],
      });
      exited = new Promise(resolveExit => child!.once("close", resolveExit));
      const hubUrl = await new Promise<string>((resolveReady, rejectReady) => {
        const timeout = setTimeout(() => rejectReady(new Error("CLI did not become ready")), 5000);
        const cleanup = () => clearTimeout(timeout);
        let stderr = "";
        child!.stderr!.on("data", data => {
          stderr += String(data);
          const match = /cito hub: ready on (http:\/\/127\.0\.0\.1:\d+) \(auth=oidc\)/.exec(stderr);
          if (match?.[1] && stderr.includes("cito setup --hub team")) {
            cleanup();
            resolveReady(match[1]);
          }
        });
        child!.once("error", error => { cleanup(); rejectReady(error); });
        void exited!.then(code => { cleanup(); rejectReady(new Error("CLI exited " + code + ": " + stderr)); });
      });
      client = await connectNats({ servers: hubUrl, token: await key.sign(oidc.claims()), timeout: 2000 });
      await client.flush();
      child.kill("SIGTERM");
      expect(await exited).toBe(0);
    } finally {
      await client?.close();
      if (child && child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
        await exited;
      }
      await oidc.stop();
      await rm(directory, { recursive: true, force: true });
    }
  }, 10_000);

  it("runs the no-auth HubServer without any authorization credentials", async () => {
    const broker = await startNats();
    let client: Awaited<ReturnType<typeof connectNats>> | undefined;
    try {
      client = await connectNats({ servers: broker.url });
      const info = await (await client.jetstreamManager()).getAccountInfo();
      expect(info.streams).toBe(0);
    } finally {
      await client?.close();
      await broker.stop();
    }
  });

  it("enforces zero-script git packaging contract and executes native TypeScript entrypoints without dist/", async () => {
    const pkg = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8"));
    expect(pkg.files).toEqual(["bin", "dist/src", "dist/plugins", "src", "plugins", "README.md"]);
    for (const forbidden of ["prepare", "build", "prepack", "preinstall", "install", "postinstall"]) {
      expect(pkg.scripts?.[forbidden]).toBeUndefined();
    }
    expect(Object.keys(pkg.scripts ?? {})).toEqual(["test", "typecheck", "compile", "prepublishOnly"]);
    expect(pkg.scripts?.compile).toBe("rm -rf dist && tsc && if [ \"$(uname -s)-$(uname -m)\" = \"Darwin-arm64\" ]; then swiftc -O -target arm64-apple-macos13.0 src/macos/cito-ui.swift -o bin/cito-ui-darwin-arm64 && strip -x bin/cito-ui-darwin-arm64 && codesign -s - --force bin/cito-ui-darwin-arm64; fi");
    expect(pkg.scripts?.prepublishOnly).toBe("npm run typecheck && npm run compile");
    expect(pkg.dependencies?.tsx).toBeUndefined();
    expect(Object.keys(pkg.dependencies ?? {}).sort()).toEqual(["@modelcontextprotocol/sdk", "jose", "zod"]);
    expect(Object.keys(pkg.bin ?? {})).toEqual(["cito", "cito-mcp", "cito-claude-hook"]);

    const directory = await mkdtemp(join(tmpdir(), "cito-pkg-"));
    try {
      const sandbox = join(directory, "sandbox");
      for (const item of ["package.json", "bin", "src", "plugins"]) {
        await cp(resolve(item), join(sandbox, item), { recursive: true });
      }
      await symlink(resolve("node_modules"), join(sandbox, "node_modules"), "dir");
      expect(existsSync(join(sandbox, "dist"))).toBe(false);

      const sandboxEnv = isolatedEnv(directory);
      const cursorPluginDir = join(sandboxEnv.CITO_STATE_DIR!, "plugins/cursor");
      await mkdir(cursorPluginDir, { recursive: true });
      await writeFile(
        join(cursorPluginDir, "plugin.json"),
        JSON.stringify({ id: "cursor", displayName: "Cursor", sfSymbol: "cursorarrow.rays", tint: "purple", cliFlag: true }),
      );
      await writeFile(
        join(cursorPluginDir, "index.ts"),
        `import { CitoError, SocketBridgeAdapter, type PluginManifest } from "@tefx/cito/plugin-sdk";
export default (manifest: PluginManifest) => new SocketBridgeAdapter(manifest.id, {
  manifest,
  cli: {
    resolveMeta: () => { throw new CitoError("BINDING_REQUIRED", "Cursor custom plugin executed via @tefx/cito/plugin-sdk"); },
  },
});\n`,
      );

      const mcpSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-mcp"), "listen", "--unknown-flag"], {
        cwd: directory,
        env: isolatedEnv(directory),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(mcpSandbox.error).toBeUndefined();
      expect(mcpSandbox.status).toBe(1);
      expect(mcpSandbox.stderr).toContain("Unknown flag: --unknown-flag");

      const hubSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-mcp"), "hub"], {
        cwd: directory,
        env: { ...isolatedEnv(directory), CITO_AUTH_MODE: "other" },
        encoding: "utf8",
        timeout: 5000,
      });
      expect(hubSandbox.error).toBeUndefined();
      expect(hubSandbox.status).toBe(1);
      expect(hubSandbox.stderr).toMatch(/cito hub:.*CITO_AUTH_MODE/);

      const hookSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-claude-hook")], {
        cwd: directory,
        env: sandboxEnv,
        input: JSON.stringify({ hook_event_name: "Stop", cwd: directory }),
        encoding: "utf8",
        timeout: 5000,
      });
      expect(hookSandbox.error).toBeUndefined();
      expect(hookSandbox.status).toBe(0);
      expect(hookSandbox.stdout).toBe("{}\n");
      expect(hookSandbox.stderr).toBe("");

      const cursorSandbox = spawnSync(process.execPath, [join(sandbox, "bin/cito-mcp"), "listen", "--cursor"], {
        cwd: directory,
        env: sandboxEnv,
        encoding: "utf8",
        timeout: 5000,
      });
      expect(cursorSandbox.error).toBeUndefined();
      expect(cursorSandbox.status).toBe(1);
      expect(cursorSandbox.stderr).toContain("Cursor custom plugin executed via @tefx/cito/plugin-sdk");

      if (existsSync(resolve("dist/src/mcp/cli.js"))) {
        const cursorDist = spawnSync(process.execPath, [resolve("bin/cito-mcp"), "listen", "--harness", "cursor"], {
          cwd: directory,
          env: sandboxEnv,
          encoding: "utf8",
          timeout: 5000,
        });
        expect(cursorDist.error).toBeUndefined();
        expect(cursorDist.status).toBe(1);
        expect(cursorDist.stderr).toContain("Cursor custom plugin executed via @tefx/cito/plugin-sdk");
      }

      const repo = join(directory, "repo");
      const prefix = join(directory, "prefix");
      for (const item of ["package.json", "package-lock.json", "tsconfig.json", "README.md", "bin", "src", "plugins"]) {
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

      const installedHub = spawnSync(join(prefix, "bin/cito"), ["hub"], {
        cwd: directory,
        env: { ...isolatedEnv(directory), CITO_AUTH_MODE: "other" },
        encoding: "utf8",
        timeout: 5000,
      });
      expect(installedHub.error).toBeUndefined();
      expect(installedHub.status).toBe(1);
      expect(installedHub.stderr).toMatch(/cito hub:.*CITO_AUTH_MODE/);

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
      expect(installedHook.stderr).toBe("");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }, 45_000);
});
