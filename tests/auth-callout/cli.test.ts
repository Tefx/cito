import { afterEach, describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, lstat, mkdir, readFile, realpath, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { NativeHarness, nativeFixture, object, projectRoot } from "../support/native-harness.js";
import { signingKey, startOidc } from "./oidc-fixture.js";

const fixtures: NativeHarness[] = [];
afterEach(async () => { const results=await Promise.allSettled(fixtures.splice(0).reverse().map(f=>f.close()));const failed=results.find(r=>r.status==="rejected");if(failed?.status==="rejected")throw failed.reason; });
async function fixture() { const f = await nativeFixture(); fixtures.push(f); return f; }

describe("native cito hub CLI and zero-runtime-dependency npm distribution", () => {
  it("fails before network access with exit 1 and a diagnostic for missing/malformed config", async () => {
    const f = await fixture();
    const secretToken = "super-secret-token-value-123";
    const base = { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: "http://127.0.0.1:9", CITO_OIDC_CLIENT_ID: "cito", CITO_HUB_TOKEN: secretToken };
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
    for (const [args, overrides, message] of cases) {
      const result = await f.command(args, { env: { ...base, ...overrides }, timeout: 3_000, network:"deny" });
      expect(result.code).toBe(1);
      expect(result.stdout).toBe("");
      expect(result.stderr).toMatch(message);
      expect(result.stderr).not.toContain(secretToken);
    }
  }, 15_000);

  it("boots through the real native cito hub CLI, serves OIDC authentication and exits cleanly on SIGTERM", async () => {
    const key = await signingKey("ES256", "ec"), oidc = await startOidc([key.jwk]);
    const f = await fixture();
    try {
      const hub = await f.hub({ auth: "oidc", issuer: oidc.issuer, clientId: "cito" });
      const response = object(await hub.rpc("stream_info", { stream: "KV_cito_listeners" }, { authorization: `Bearer ${await key.sign(oidc.claims())}` }));
      expect(response.config).toMatchObject({ name: "KV_cito_listeners" });
      await hub.process.stop();
      expect((await hub.process.result).code).toBe(0);
    } finally { await oidc.stop(); }
  }, 10_000);

  it("runs the native no-auth HubServer without any authorization credentials", async () => {
    const f = await fixture(), hub = await f.hub();
    const info = object(await hub.rpc("account_info"));
    expect(info.streams).toBe(0);
  });

  it("packs and installs precompiled native entrypoints without dist, source, a Node fallback, or runtime dependencies", async () => {
    const f = await fixture();
    const pkg = object(JSON.parse(await readFile(join(projectRoot, "package.json"), "utf8")));
    expect(pkg.files).toEqual(["bin", "plugins", "README.md", "LICENSE"]);
    for (const forbidden of ["prepare", "build", "prepack", "preinstall", "install", "postinstall"]) {
      expect(object(pkg.scripts)[forbidden]).toBeUndefined();
    }
    expect(Object.keys(object(pkg.scripts))).toEqual(["test", "typecheck", "compile", "prepublishOnly"]);
    expect(object(pkg.scripts).compile).toBe("node bin/build.mjs");
    expect(object(pkg.scripts).prepublishOnly).toBe("npm run typecheck && npm run compile && npm test");
    expect(pkg.dependencies).toEqual({});
    for (const dependency of ["@modelcontextprotocol/sdk", "jose", "zod"]) expect(object(pkg.devDependencies)[dependency]).toBeTypeOf("string");
    expect(pkg.bin).toEqual({ cito: "bin/cito", "cito-mcp": "bin/cito-mcp" });
    for (const key of ["main", "types", "exports", "pi"]) expect(pkg[key]).toBeUndefined();

    const sandbox = join(f.root, "sandbox");
    for (const item of ["package.json", "bin", "plugins", "README.md", "LICENSE"]) await cp(join(projectRoot, item), join(sandbox, item), { recursive: true });
    expect(existsSync(join(sandbox, "dist"))).toBe(false);
    expect(existsSync(join(sandbox, "src"))).toBe(false);
    const custom = join(f.stateDir, "plugins", "cursor");
    await mkdir(custom, { recursive: true });
    await writeFile(join(custom, "plugin.json"), JSON.stringify({ id: "cursor", displayName: "Cursor", kind: "agent-harness", cliFlag: true, command: ["./driver"] }));
    await writeFile(join(custom, "driver"), '#!/bin/sh\nread request\nprintf \'%s\\n\' \'{"ok":false,"error_code":"BINDING_REQUIRED","message":"Cursor subprocess plugin executed"}\'\n', { mode: 0o700 });

    const mcp = await f.command(["listen", "--unknown-flag"], { executable: join(sandbox, "bin/cito-mcp") });
    expect(mcp.code).toBe(1); expect(mcp.stderr).toContain("Unknown flag: --unknown-flag");
    const hub = await f.command(["hub"], { executable: join(sandbox, "bin/cito"), env: { CITO_AUTH_MODE: "other" } });
    expect(hub.code).toBe(1); expect(hub.stderr).toMatch(/cito hub:.*CITO_AUTH_MODE/);
    const hook = await f.command([], { executable: join(sandbox, "plugins/claude/bin/cito-claude-hook"), input: JSON.stringify({ hook_event_name: "Stop", cwd: f.root }) });
    expect.soft(hook).toMatchObject({ code: 0, stdout: "{}\n", stderr: "" });
    const cursor = await f.command(["listen", "--cursor"], { executable: join(sandbox, "bin/cito") });
    expect(cursor.code).toBe(1); expect(cursor.stderr).toContain("Cursor subprocess plugin executed");

    // npm pack/install exercise npm's files/bin contract, with all state and cache inside this cleanroom.
    const npm = (args: string[], cwd = sandbox) => {
      const result = spawnSync("npm", args, { cwd, env: { ...f.env, npm_config_cache: join(f.root, "npm-cache") }, encoding: "utf8", timeout: 30_000 });
      expect(result.error).toBeUndefined();
      expect(result.status, result.stderr).toBe(0);
      return result;
    };
    const packed = JSON.parse(npm(["pack", "--ignore-scripts", "--json", "--pack-destination", f.root]).stdout) as { filename: string }[];
    const prefix = join(f.root, "prefix");
    npm(["install", "-g", "--prefix", prefix, "--ignore-scripts", "--no-audit", "--no-fund", join(f.root, packed[0]!.filename)]);
    const installed = await realpath(join(prefix, "lib/node_modules/@tefx/cito"));
    expect(existsSync(join(installed, "dist"))).toBe(false);
    expect(existsSync(join(installed, "src"))).toBe(false);
    expect(existsSync(join(installed, "node_modules"))).toBe(false);
    expect(existsSync(join(installed, "bin/cito-ui-darwin-arm64"))).toBe(true);
    expect(existsSync(join(installed, "bin/cito-native-darwin-arm64"))).toBe(true);
    for (const [name, target] of [["cito", "bin/cito"], ["cito-mcp", "bin/cito-mcp"]]) {
      const path = join(prefix, "bin", name!);
      expect((await lstat(path)).isSymbolicLink()).toBe(true);
      expect(await realpath(path)).toBe(join(installed, target!));
    }
    expect(existsSync(join(prefix, "bin/cito-claude-hook"))).toBe(false);
    const linked = join(f.root, "linked-cito"); await symlink(join(prefix, "bin/cito"), linked);
    const installedCito = await f.command(["listen", "--unknown-flag"], { executable: linked });
    expect(installedCito.code).toBe(1); expect(installedCito.stderr).toContain("Unknown flag: --unknown-flag");
    const installedHub = await f.command(["hub"], { executable: join(prefix, "bin/cito"), env: { CITO_AUTH_MODE: "other" } });
    expect(installedHub.code).toBe(1); expect(installedHub.stderr).toMatch(/cito hub:.*CITO_AUTH_MODE/);
    const installedF=await fixture();await cp(custom,join(installedF.stateDir,"plugins/cursor"),{recursive:true});
    const installedCursor=await installedF.command(["listen","--cursor"],{executable:join(prefix,"bin/cito"),env:{CITO_BUILTIN_PLUGINS_DIR:undefined,CITO_UI_BINARY:join(installed,"bin/cito-ui-darwin-arm64")}});
    expect(installedCursor.code).toBe(1);expect(installedCursor.stderr).toContain("Cursor subprocess plugin executed");
    const health=object(await(await fetch(`http://127.0.0.1:${installedF.daemonPort}/health`)).json());expect(health.stateDir).toBe(installedF.stateDir);expect(health.version).toBe("0.18.0");
    const image=await installedF.command(["-p",String(health.pid),"-o","command="],{executable:"/bin/ps"});expect(image.stdout).toContain(join(installed,"bin/cito-native-darwin-arm64"));
    const installedHook = await installedF.command([], { executable: join(installed, "plugins/claude/bin/cito-claude-hook"), input: JSON.stringify({ hook_event_name: "Stop", cwd: installedF.root }) });
    expect.soft(installedHook).toMatchObject({ code: 0, stdout: "{}\n", stderr: "" });
  }, 60_000);
});

it("executes a warmed copied native Claude hook in under 15 ms",async()=>{
 const f=await nativeFixture();try{const hook=join(f.root,"copied-hook");await cp(join(projectRoot,"plugins/claude/bin/cito-claude-hook"),hook);const opts={cwd:f.root,env:f.env,encoding:"utf8" as const,timeout:5000};const warm=spawnSync(hook,[],{...opts,input:"{}"});expect(warm.status).toBe(0);const start=performance.now();const result=spawnSync(hook,[],{...opts,input:JSON.stringify({hook_event_name:"Stop",cwd:f.root})});const elapsed=performance.now()-start;expect(result.status).toBe(0);expect(result.stdout).toBe("{}\n");expect(elapsed).toBeLessThan(15);}finally{await f.close();}
});
