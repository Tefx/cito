import { afterEach, expect, test } from "vitest";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadConfig, formatScopedId, parseScopedId, isSingleDefaultHub, harnessIdSchema, listenerIdSchema } from "../../src/shared/index.js";

const roots: string[] = [];
afterEach(async () => { for (const root of roots.splice(0)) await rm(root, { recursive: true, force: true }); });
async function configFile(value: unknown) {
  const root = await mkdtemp("/tmp/cito-config-"); roots.push(root);
  const path = join(root, "config.json"); await writeFile(path, JSON.stringify(value));
  return { root, path, env: { CITO_STATE_DIR: root, CITO_CODEX_HOME: join(root, "codex"), CITO_USER: "test" } };
}
const personal = { natsUrl: "nats://127.0.0.1:4222" }, work = { natsUrl: "wss://work.example/ws", authMode: "oidc", oidcIssuer: "https://id.example/", oidcClientId: "cito", oidcScopes: ["openid", "profile"] };
test("daemon and OIDC redirect ports validate environment and JSON aliases", async () => {
  for (const key of ["CITO_DAEMON_PORT", "CITO_OIDC_REDIRECT_PORT"]) {
    for (const value of ["0", "-1", "65536", "1.5", "NaN", ""]) expect(() => loadConfig({ [key]: value })).toThrow();
  }
  expect(loadConfig({ CITO_DAEMON_PORT: "1234", CITO_OIDC_REDIRECT_PORT: "4321" })).toMatchObject({ daemonPort: 1234, oidcRedirectPort: 4321 });
  const { path, root } = await configFile({ daemonPort: 3333, hubs: {
    a: { natsUrl: "nats://127.0.0.1:4222", redirect_port: 4444 }, b: { natsUrl: "nats://127.0.0.1:4223", oidcRedirectPort: 5555 },
  }, default_hub: "a" });
  const config = loadConfig({ CITO_CONFIG_PATH: path, CITO_STATE_DIR: root });
  expect(config.daemonPort).toBe(3333); expect(config.hubs.a!.oidcRedirectPort).toBe(4444); expect(config.hubs.b!.oidcRedirectPort).toBe(5555);
});

test("channel config defaults off, accepts both JSON aliases, validates booleans and gives env precedence", async () => {
  expect(loadConfig({}).enableClaudeChannel).toBe(false);
  expect(loadConfig({ CITO_CLAUDE_CHANNEL: "1" }).enableClaudeChannel).toBe(true);
  for (const key of ["enable_claude_channel", "enableClaudeChannel"]) {
    const f = await configFile({ hubs: { personal }, [key]: true });
    expect(loadConfig(f.env).enableClaudeChannel).toBe(true);
    expect(loadConfig({ ...f.env, CITO_CLAUDE_CHANNEL: "0" }).enableClaudeChannel).toBe(false);
    await writeFile(f.path, JSON.stringify({ hubs: { personal }, [key]: false }));
    expect(loadConfig({ ...f.env, CITO_CLAUDE_CHANNEL: "1" }).enableClaudeChannel).toBe(true);
    await writeFile(f.path, JSON.stringify({ hubs: { personal }, [key]: "true" }));
    expect(() => loadConfig(f.env)).toThrow();
  }
});
test("config file loads simultaneous hubs, defaults, scopes and mirrors the selected default", async () => {
  const f = await configFile({ default_hub: "personal", hubs: { personal, work } });
  const config = loadConfig(f.env);
  expect(config.defaultHub).toBe("personal"); expect(config.hubs.personal).toMatchObject({ name: "personal", authMode: "none", user: "test" });
  const selected = loadConfig({ ...f.env, CITO_HUB: " work " });
  expect(selected).toMatchObject({ defaultHub: "work", natsUrl: work.natsUrl, authMode: "oidc", oidcIssuer: "https://id.example", oidcClientId: "cito", oidcScopes: ["openid", "profile"] });
  expect(loadConfig({ ...f.env, CITO_NATS_URL: "nats://localhost:4333" }).hubs.personal!.natsUrl).toBe("nats://localhost:4333");
  expect(loadConfig({ ...f.env, CITO_CONFIG_PATH: f.path, CITO_NATS_URL: "nats://localhost:4333" }).natsUrl).toBe(personal.natsUrl);
});
test("single named hub selects itself; multiple hubs require a valid default; explicit bad paths fail fast", async () => {
  const f = await configFile({ hubs: { personal } });
  expect(loadConfig(f.env).defaultHub).toBe("personal");
  for (const value of [{ hubs: {} }, { hubs: { Bad: personal } }, { hubs: { personal, work } }, { default_hub: "absent", hubs: { personal } },
    { hubs: { work: { ...work, oidcClientId: undefined } } }, { hubs: { personal: { natsUrl: "https://bad" } } }]) {
    await writeFile(f.path, JSON.stringify(value)); expect(() => loadConfig(f.env)).toThrow();
  }
  await writeFile(f.path, "invalid json"); expect(() => loadConfig(f.env)).toThrow();
  expect(() => loadConfig({ ...f.env, CITO_CONFIG_PATH: join(f.root, "missing") })).toThrow();
});
test("absent config synthesizes single default hub from environment with symmetric unprefixed output", async () => {
  const f = await configFile({}); await rm(f.path);
  const config = loadConfig({ ...f.env, CITO_NATS_URL: "nats://localhost:4444", CITO_AUTH_MODE: "none" });
  expect(config.hubs.default).toMatchObject({ natsUrl: config.natsUrl, authMode: "none", user: "test" });
  expect(isSingleDefaultHub(config)).toBe(true);
  expect(formatScopedId("default", "sig_one", config)).toBe("sig_one");
  expect(parseScopedId("default:sig_one", config)).toEqual({ hub: "default", id: "sig_one" });
  expect(parseScopedId("sig_one", config)).toEqual({ hub: "default", id: "sig_one" });
});
test("named and multi-hub IDs roundtrip and reject unknown or conflicting scopes", async () => {
  const f = await configFile({ default_hub: "personal", hubs: { personal, work } }), config = loadConfig(f.env);
  for (const hub of ["personal", "work"]) {
    const id = formatScopedId(hub, "c_abcdefgh2345", config);
    expect(id).toBe(hub + ":c_abcdefgh2345"); expect(listenerIdSchema.safeParse(id).success).toBe(true);
    expect(parseScopedId(id, config, hub)).toEqual({ hub, id: "c_abcdefgh2345" });
  }
  for (const [raw, hub] of [["work:sig_one", "personal"], ["other:sig_one", undefined], ["sig_one", "other"], ["work:", undefined]]) expect(() => parseScopedId(raw!, config, hub)).toThrow();
  const single = await configFile({ hubs: { personal } });
  expect(formatScopedId("personal", "id", loadConfig(single.env))).toBe("personal:id");
});
test("harness grammar is open and bounded", () => {
  for (const id of ["pi", "codex", "cursor", "aider", "custom_ide-v2", "a".repeat(32)]) expect(harnessIdSchema.safeParse(id).success).toBe(true);
  for (const id of ["", "9ide", "Bad", "a:b", "../pi", "a".repeat(33)]) expect(harnessIdSchema.safeParse(id).success).toBe(false);
});
