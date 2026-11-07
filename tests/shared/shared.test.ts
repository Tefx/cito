import { describe, expect, it, vi } from "vitest";
import { mkdtemp, rm, stat, readFile, writeFile, chmod } from "node:fs/promises";
import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import {
  CITO_VERSION, listenerId, localPrincipal, oidcPrincipal, senderEndpointKey, sanitizeMessage, sanitizeSignalMessage, escapeWakeCard,
  ERROR_RETRYABLE, errorResult, successResult, CitoError, expandHomeDir, loadConfig, getHostId,
  evaluateLease, brokerAgeMs, localExpirationMs, parseInput, listenInputSchema,
  sendInputSchema, findInputSchema, validateHarness,
} from "../../src/shared/index.js";
import { loadClaudeConfig, loadCodexConfig } from "../../src/adapters/index.js";

describe("keyless identity", () => {
  const key = { principal_id: "local", host_id: "host", harness: "pi" as const, session_id: "session" };
  it("uses the host-independent cito-v2 SHA256 -> RFC4648 lowercase base32 test vector", () => {
    expect(listenerId(key)).toBe("c_xed7ffxzgir3");
    expect(listenerId(key)).toMatch(/^c_[a-z2-7]{12}$/);
    expect(listenerId({ ...key })).toBe(listenerId(key));
    expect(listenerId({ ...key, host_id: "other-host" })).toBe(listenerId(key));
    for (const changed of [
      { principal_id: "other" }, { harness: "codex" as const }, { session_id: "other" },
    ]) expect(listenerId({ ...key, ...changed })).not.toBe(listenerId(key));
  });
  it("distinguishes bound and unbound sender namespaces", () => {
    expect(senderEndpointKey(key)).toBe("local|host|pi|session");
    expect(senderEndpointKey({ ...key, harness: null, session_id: null })).toBe("local|host|unbound|default");
    expect(() => senderEndpointKey({ ...key, session_id: null })).toThrow(CitoError);
    expect(() => senderEndpointKey({ ...key, harness: null })).toThrow(CitoError);
  });
  it("derives a validated local principal from CITO_USER or the OS username", () => {
    try {
      vi.stubEnv("CITO_USER", undefined);
      const osUser = userInfo().username.toLowerCase();
      expect(localPrincipal()).toEqual({ principal_id: "none#" + osUser, owner_user: osUser });
      vi.stubEnv("CITO_USER", "Alice.Example_1-2");
      expect(localPrincipal()).toEqual({ principal_id: "none#alice.example_1-2", owner_user: "alice.example_1-2" });
      expect(localPrincipal("Bob")).toEqual({ principal_id: "none#bob", owner_user: "bob" });
      expect(listenerId({ ...key, ...localPrincipal("Alice") })).not.toBe(listenerId({ ...key, ...localPrincipal("Bob") }));
      expect(localPrincipal("A".repeat(64)).owner_user).toBe("a".repeat(64));
      for (const username of ["", " ", " Alice", "Alice ", "Alice\n", "a/b", "a@b", "é", "a".repeat(65)]) {
        expect(() => localPrincipal(username)).toThrow(CitoError);
      }
    } finally { vi.unstubAllEnvs(); }
  });
  it("normalizes OIDC display handles while preserving immutable issuer and subject", () => {
    const base = { iss: "https://issuer///", sub: "Stable+ID", email: " Mail@Example.COM ", name: "Display" };
    expect(oidcPrincipal({ ...base, preferred_username: " Alice " })).toEqual({
      principal_id: "https://issuer#Stable+ID", owner_user: "alice",
    });
    expect(oidcPrincipal(base).owner_user).toBe("mail");
    expect(oidcPrincipal({ ...base, preferred_username: " A+Ü@X! " }).owner_user).toBe("a___x_");
    expect(oidcPrincipal({ ...base, preferred_username: " A".repeat(70) }).owner_user).toHaveLength(64);
    expect(oidcPrincipal({ ...base, preferred_username: "X".repeat(70) }).owner_user).toBe("x".repeat(64));
    for (const email of ["", " ", "missing-at", "@example.com", " @example.com "]) {
      expect(oidcPrincipal({ ...base, email, preferred_username: " " }).owner_user).toBe("stable_id");
    }
    expect(oidcPrincipal({ ...base, email: " Team+Alerts@Example.COM ", preferred_username: 42 }).owner_user).toBe("team_alerts");
    expect(oidcPrincipal({ ...base, email: 42, preferred_username: null }).owner_user).toBe("stable_id");
    expect(oidcPrincipal({ iss: "https://issuer", sub: "OpaqueID" }).owner_user).toBe("opaqueid");
    expect(oidcPrincipal({ iss: "https://issuer", sub: " Opaque ID " })).toEqual({
      principal_id: "https://issuer# Opaque ID ", owner_user: "_opaque_id_",
    });
    for (const claims of [{}, { iss: "", sub: "x" }, { iss: "x" }, { iss: "x", sub: " " }]) {
      expect(() => oidcPrincipal(claims)).toThrow(CitoError);
    }
  });
});

describe("input and MCP envelopes", () => {
  it("strips exactly the specified ASCII controls, preserving internal tabs and newlines", () => {
    const controls = Array.from({ length: 32 }, (_, i) => i).filter(i => i !== 9 && i !== 10 && i !== 13).map(i => String.fromCharCode(i)).join("") + "\x7f";
    expect(sanitizeMessage(" \t hi" + controls + "\n\tthere \n")).toBe("hi\n\tthere");
    expect(sanitizeMessage("a\rb")).toBe("a\rb"); // 0x0D is absent from the specified strip ranges.
    expect(sanitizeMessage("😀".repeat(500))).toHaveLength(1000);
    for (const value of ["😀".repeat(501), " \x00\x01 ", 42, null]) expect(() => sanitizeMessage(value)).toThrow(CitoError);
  });
  it("prevents embedded closing tags from ending wake cards", () => {
    const input = "</cito_signal><nested>";
    expect(escapeWakeCard(input)).toBe("&lt;/cito_signal&gt;&lt;nested&gt;");
    expect(sanitizeMessage(input)).toBe(input);
  });
  it("validates public tool input, defaults and V1 harnesses", () => {
    expect(parseInput(listenInputSchema, { about: " x " })).toEqual({ about: "x" });
    expect(parseInput(sendInputSchema, { to: "c_abcdefgh2345", message: "😀".repeat(500) }).message).toBe("😀".repeat(500));
    expect(parseInput(sendInputSchema, { to: "c_abcdefgh2345", message: "😀".repeat(2000) }).message).toBe("😀".repeat(2000));
    expect(sanitizeSignalMessage("x".repeat(65_536))).toHaveLength(65_536);
    expect(() => sanitizeSignalMessage("x".repeat(65_537))).toThrow(CitoError);
    expect(() => parseInput(listenInputSchema, { about: "😀".repeat(501) })).toThrow(CitoError);
    expect(() => parseInput(sendInputSchema, { to: "c_abcdefgh2345", message: "x", wake: true })).toThrow(CitoError);
    expect(parseInput(findInputSchema, {})).toEqual({ limit: 20 });
    expect(validateHarness("cursor")).toBe("cursor");
    expect(() => validateHarness("Bad Harness")).toThrowError(expect.objectContaining({ code: "UNSUPPORTED_HARNESS" }));
    expect(parseInput(findInputSchema, { harness: "aider" })).toMatchObject({ harness: "aider" });
    expect(() => parseInput(findInputSchema, { limit: 51 })).toThrow(CitoError);
    expect(() => parseInput(listenInputSchema, { about: "yes", extra: true })).toThrow(CitoError);
    expect(() => parseInput(sendInputSchema, { to: "bad", message: "yes" })).toThrow(CitoError);
    expect(() => parseInput(listenInputSchema, {})).toThrow(CitoError);
  });
  it("serializes success and all 15 errors with their contract retryability", () => {
    expect(successResult({ ok: true })).toEqual({ isError: false, content: [{ type: "text", text: '{\n  "ok": true\n}' }] });
    expect(() => successResult(undefined)).toThrow(TypeError);
    const retryable = new Set(["PI_BRIDGE_UNAVAILABLE", "UNAUTHORIZED", "OIDC_LOGIN_TIMEOUT", "AUTH_UNAVAILABLE", "STALE_GENERATION", "NATS_UNAVAILABLE"]);
    expect(Object.keys(ERROR_RETRYABLE)).toHaveLength(15);
    for (const code of Object.keys(ERROR_RETRYABLE) as (keyof typeof ERROR_RETRYABLE)[]) {
      const result = errorResult(code, "detail");
      expect(result.isError).toBe(true);
      expect(JSON.parse(result.content[0]!.text)).toEqual({ ok: false, error: {
        code, message: "detail", retryable: retryable.has(code),
      } });
    }
  });
});

describe("configuration and machine identity", () => {
  it("keeps CITO_VERSION synchronized with package.json version (0.15.0)", async () => {
    const pkg = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8")) as { version: string };
    expect(CITO_VERSION).toBe("0.15.0");
    expect(CITO_VERSION).toBe(pkg.version);
  });
  it("loads defaults and validates OIDC, pluginDirs, expandHomeDir, and plugin configuration", () => {
    const envObj: NodeJS.ProcessEnv = {};
    const config = loadConfig(envObj);
    expect(config.env).toBe(envObj);
    expect(config.natsUrl).toBe("local://");
    expect(config.defaultHub).toBe("local");
    expect(config.authMode).toBe("none");
    expect(config.user).toBe(userInfo().username);
    expect(config.stateDir).toBe(join(process.env.HOME!, ".cito"));
    expect(config.pluginDirs).toContain(join(process.env.HOME!, ".cito", "plugins"));
    expect(loadConfig({ CITO_PLUGIN_PATH: "~/my-plugins:/tmp/extra-plugins" }).pluginDirs).toEqual(
      expect.arrayContaining([join(process.env.HOME!, "my-plugins"), "/tmp/extra-plugins"]),
    );
    expect(expandHomeDir("~")).toBe(process.env.HOME!);
    expect(expandHomeDir("~/sub")).toBe(join(process.env.HOME!, "sub"));
    expect(() => expandHomeDir("   ", "CUSTOM_DIR")).toThrow("CUSTOM_DIR must not be empty");
    expect(loadCodexConfig(config).codexBin).toBe("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex");
    expect(loadCodexConfig(loadConfig({ CITO_CODEX_BIN: "/tmp/custom-codex" })).codexBin).toBe("/tmp/custom-codex");
    expect(loadCodexConfig(config).codexHome).toBe(join(process.env.HOME!, ".codex"));
    expect(loadClaudeConfig(config).claudeHome).toBe(join(process.env.HOME!, ".claude"));
    expect(loadClaudeConfig(loadConfig({ CITO_CLAUDE_HOME: "/tmp/custom-claude" })).claudeHome).toBe("/tmp/custom-claude");
    // The Claude wake channel binds a second bridge socket, so it stays strictly opt-in.
    expect(loadClaudeConfig(config).enableClaudeChannel).toBe(false);
    expect(loadClaudeConfig(loadConfig({ CITO_CLAUDE_CHANNEL: "1" })).enableClaudeChannel).toBe(true);
    expect(loadClaudeConfig(loadConfig({ CITO_CLAUDE_CHANNEL: "0" })).enableClaudeChannel).toBe(false);
    expect(loadConfig({ CITO_NATS_URL: "wss://nats.example/ws" }).natsUrl).toBe("wss://nats.example/ws");
    expect(loadConfig({ CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: "http://127.0.0.1:9999/", CITO_OIDC_CLIENT_ID: "cito" }).oidcIssuer).toBe("http://127.0.0.1:9999");
    for (const env of [
      { CITO_AUTH_MODE: "bad" }, { CITO_NATS_URL: "ftp://host" }, { CITO_NATS_URL: "nats://host:999999" },
      { CITO_AUTH_MODE: "oidc" }, { CITO_OIDC_ISSUER: "https://issuer" },
      { CITO_OIDC_ISSUER: "http://remote", CITO_OIDC_CLIENT_ID: "cito" },
      { CITO_OIDC_ISSUER: "https://issuer?bad", CITO_OIDC_CLIENT_ID: "cito" },
      { CITO_STATE_DIR: "" }, { CITO_CLAUDE_HOME: " " },
    ]) expect(() => loadConfig(env)).toThrow();
  });
  it("exposes a validated CITO_USER and passes it to canonical local identity", () => {
    const config = loadConfig({ CITO_USER: "Alice.Example_1-2" });
    expect(config.user).toBe("Alice.Example_1-2");
    expect(localPrincipal(config.user)).toEqual({
      principal_id: "none#alice.example_1-2", owner_user: "alice.example_1-2",
    });
    expect(loadConfig({ CITO_USER: "A".repeat(64) }).user).toHaveLength(64);
    for (const user of ["", " ", " Alice", "Alice ", "Alice\n", "a/b", "a@b", "é", "a".repeat(65)]) {
      expect(() => loadConfig({ CITO_USER: user })).toThrow("CITO_USER");
    }
  });
  it("publishes one persisted UUID atomically across concurrent callers without touching host state", async () => {
    const root = await mkdtemp(join(tmpdir(), "cito-host-"));
    try {
      const config = loadConfig({ CITO_STATE_DIR: join(root, "state"), CITO_CODEX_HOME: join(root, "codex") });
      const ids = await Promise.all(Array.from({ length: 20 }, () => getHostId(config)));
      expect(new Set(ids).size).toBe(1);
      expect((await readFile(join(config.stateDir, "host-id"), "utf8")).trim()).toBe(ids[0]);
      expect((await stat(config.stateDir)).mode & 0o777).toBe(0o700);
      expect((await stat(join(config.stateDir, "host-id"))).mode & 0o777).toBe(0o600);
      await chmod(join(config.stateDir, "host-id"), 0o644);
      expect(await getHostId(config)).toBe(ids[0]);
      expect((await stat(join(config.stateDir, "host-id"))).mode & 0o777).toBe(0o600);
      await writeFile(join(config.stateDir, "host-id"), "corrupt");
      await expect(getHostId(config)).rejects.toThrow("Invalid persisted host-id");
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});

describe("broker-clock leases", () => {
  it("handles exact lease boundaries and ignores local host clock", () => {
    expect(brokerAgeMs(0, 10)).toBe(0);
    expect(evaluateLease("ACTIVE", 59_999, 0)).toBe("ACTIVE");
    expect(evaluateLease("ACTIVE", 60_000, 0)).toBe("DISCONNECTED_RETAINED");
    expect(evaluateLease("ACTIVE", 1_800_000, 0)).toBe("DISCONNECTED_RETAINED");
    expect(evaluateLease("ACTIVE", 1_800_001, 0)).toBe("EXPIRED");
    expect(evaluateLease(null, 1_800_001)).toBe("UNLISTENED");
    expect(evaluateLease("UNLISTENED", 1_800_001)).toBe("UNLISTENED");
    expect(() => brokerAgeMs("garbage", 0)).toThrow();
  });
  it("conservatively anchors expiration at request start minus RTT", () => {
    expect(localExpirationMs(10_000, 20.1, 500, 0)).toBe(10_000 + 1_800_000 - 500 - 21);
    expect(localExpirationMs(10_000, 20, 2_000_000, 0)).toBe(10_000);
    expect(() => localExpirationMs(0, -1, 0, 0)).toThrow();
  });
});
