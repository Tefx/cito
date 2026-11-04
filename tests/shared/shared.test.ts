import { describe, expect, it } from "vitest";
import { mkdtemp, rm, stat, readFile, writeFile } from "node:fs/promises";
import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import {
  listenerId, localPrincipal, oidcPrincipal, senderEndpointKey, sanitizeMessage, escapeWakeCard,
  ERROR_RETRYABLE, errorResult, successResult, CitoError, loadConfig, getHostId,
  evaluateLease, brokerAgeMs, localExpirationMs, parseInput, listenInputSchema,
  sendInputSchema, findInputSchema, validateHarness,
} from "../../src/shared/index.js";

describe("keyless identity", () => {
  const key = { principal_id: "local", host_id: "host", harness: "pi" as const, session_id: "session" };
  it("uses the SHA256 -> RFC4648 lowercase base32 test vector", () => {
    // Independently generated using Python hashlib + base64.b32encode.
    expect(listenerId(key)).toBe("c_za6ccpdr4its");
    expect(listenerId(key)).toMatch(/^c_[a-z2-7]{12}$/);
    expect(listenerId({ ...key })).toBe(listenerId(key));
    for (const changed of [
      { principal_id: "other" }, { host_id: "other" }, { harness: "codex" as const }, { session_id: "other" },
    ]) expect(listenerId({ ...key, ...changed })).not.toBe(listenerId(key));
  });
  it("distinguishes bound and unbound sender namespaces", () => {
    expect(senderEndpointKey(key)).toBe("local|host|pi|session");
    expect(senderEndpointKey({ ...key, harness: null, session_id: null })).toBe("local|host|unbound|default");
    expect(() => senderEndpointKey({ ...key, session_id: null })).toThrow(CitoError);
    expect(() => senderEndpointKey({ ...key, harness: null })).toThrow(CitoError);
  });
  it("derives local and canonical OIDC identity without a server key", () => {
    expect(localPrincipal()).toEqual({ principal_id: "local", owner_user: userInfo().username || "local" });
    expect(localPrincipal("")).toEqual({ principal_id: "local", owner_user: "local" });
    const base = { iss: "https://issuer///", sub: "stable", email: " Mail@Example.COM ", name: "Display" };
    expect(oidcPrincipal({ ...base, preferred_username: " Alice " })).toEqual({
      principal_id: "https://issuer#stable", owner_user: "alice",
    });
    expect(oidcPrincipal(base).owner_user).toBe("mail@example.com");
    expect(oidcPrincipal({ ...base, email: "", preferred_username: " " }).owner_user).toBe("display");
    expect(oidcPrincipal({ iss: "https://issuer", sub: "OpaqueID" }).owner_user).toBe("opaqueid");
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
    expect(parseInput(sendInputSchema, { to: "c_abcdefgh2345", message: "😀".repeat(500) }).wake).toBe(true);
    expect(parseInput(findInputSchema, {})).toEqual({ limit: 20 });
    expect(() => validateHarness("claude")).toThrowError(expect.objectContaining({ code: "UNSUPPORTED_HARNESS" }));
    expect(() => parseInput(findInputSchema, { harness: "claude" })).toThrowError(expect.objectContaining({ code: "UNSUPPORTED_HARNESS" }));
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
  it("loads defaults and validates OIDC and URL configuration", () => {
    const config = loadConfig({});
    expect(config.natsUrl).toBe("nats://127.0.0.1:4222");
    expect(config.authMode).toBe("none");
    expect(config.codexBin).toBe("codex");
    expect(config.stateDir).toBe(join(process.env.HOME!, ".cito"));
    expect(config.codexHome).toBe(join(process.env.HOME!, ".codex"));
    expect(loadConfig({ CITO_NATS_URL: "wss://nats.example/ws" }).natsUrl).toBe("wss://nats.example/ws");
    expect(loadConfig({ CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: "http://127.0.0.1:9999/", CITO_OIDC_CLIENT_ID: "cito" }).oidcIssuer).toBe("http://127.0.0.1:9999");
    for (const env of [
      { CITO_AUTH_MODE: "bad" }, { CITO_NATS_URL: "https://host" }, { CITO_NATS_URL: "nats://host:999999" },
      { CITO_AUTH_MODE: "oidc" }, { CITO_OIDC_ISSUER: "https://issuer" },
      { CITO_OIDC_ISSUER: "http://remote", CITO_OIDC_CLIENT_ID: "cito" },
      { CITO_OIDC_ISSUER: "https://issuer?bad", CITO_OIDC_CLIENT_ID: "cito" },
      { CITO_HOST_ID: "a|b" }, { CITO_STATE_DIR: "" },
    ]) expect(() => loadConfig(env)).toThrow();
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
      expect(await getHostId({ ...config, hostId: "explicit-host" })).toBe("explicit-host");
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
