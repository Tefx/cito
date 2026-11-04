import { describe, expect, it } from "vitest";
import { createAccount, createOperator, createUser } from "nkeys.js";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
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
});
