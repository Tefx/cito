import { hostname, homedir, userInfo } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { mkdir, open, readFile, chmod } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { normalizeIssuer } from "./identity.js";

export interface CitoConfig {
  natsUrl: string;
  authMode: "none" | "oidc";
  user: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  stateDir: string;
  codexHome: string;
  codexBin: string;
  hostId?: string;
  hostName: string;
}
function directory(value: string): string {
  const expanded = value === "~" ? homedir() : value.startsWith("~/") ? join(homedir(), value.slice(2)) : value;
  return isAbsolute(expanded) ? expanded : resolve(expanded);
}
export function validateHttpUrl(value: string, name: string): string {
  let url: URL;
  try { url = new URL(value); } catch { throw new Error(name + " must be an HTTPS URL"); }
  if ((url.protocol !== "https:" && !(url.protocol === "http:" && url.hostname === "127.0.0.1"))
    || url.username || url.password || url.search || url.hash) {
    throw new Error(name + " must use HTTPS (http://127.0.0.1 is allowed for tests), without credentials, query or fragment");
  }
  return value;
}
export function loadConfig(env: NodeJS.ProcessEnv = process.env): CitoConfig {
  const natsUrl = env.CITO_NATS_URL ?? "nats://127.0.0.1:4222";
  let url: URL;
  try { url = new URL(natsUrl); } catch { throw new Error("CITO_NATS_URL must be a valid NATS or WebSocket URL"); }
  if (!["nats:", "ws:", "wss:"].includes(url.protocol) || !url.hostname
    || url.username || url.password || url.hash || url.search
    || (url.protocol === "nats:" && url.pathname && url.pathname !== "/")) {
    throw new Error("CITO_NATS_URL must use nats://, ws:// or wss:// without credentials");
  }
  const authMode = env.CITO_AUTH_MODE ?? "none";
  if (authMode !== "none" && authMode !== "oidc") throw new Error("CITO_AUTH_MODE must be none or oidc");
  const oidcIssuer = env.CITO_OIDC_ISSUER === undefined ? undefined
    : normalizeIssuer(validateHttpUrl(env.CITO_OIDC_ISSUER, "CITO_OIDC_ISSUER"));
  const oidcClientId = env.CITO_OIDC_CLIENT_ID;
  if (authMode === "oidc" && !oidcIssuer) throw new Error("CITO_OIDC_ISSUER is required in oidc mode");
  if ((authMode === "oidc" || oidcIssuer !== undefined) && !oidcClientId?.trim()) {
    throw new Error("CITO_OIDC_CLIENT_ID is required with an OIDC issuer");
  }
  if (oidcClientId !== undefined && (oidcClientId !== oidcClientId.trim() || !oidcClientId)) {
    throw new Error("CITO_OIDC_CLIENT_ID must be a non-empty trimmed string");
  }
  const user = env.CITO_USER ?? userInfo().username;
  if (user.length < 1 || user.length > 64 || /[^a-zA-Z0-9._-]/.test(user)) {
    throw new Error("CITO_USER must contain 1–64 letters, numbers, dots, underscores or hyphens");
  }
  const hostId = env.CITO_HOST_ID;
  if (hostId !== undefined && !/^[a-zA-Z0-9._-]{1,128}$/.test(hostId)) {
    throw new Error("CITO_HOST_ID must contain 1–128 letters, numbers, dots, underscores or hyphens");
  }
  for (const key of ["CITO_STATE_DIR", "CITO_CODEX_HOME", "CITO_CODEX_BIN"] as const) {
    if (env[key] !== undefined && !env[key]?.trim()) throw new Error(key + " must not be empty");
  }
  return {
    natsUrl, authMode, user, oidcIssuer, oidcClientId,
    stateDir: directory(env.CITO_STATE_DIR ?? "~/.cito"),
    codexHome: directory(env.CITO_CODEX_HOME ?? "~/.codex"),
    codexBin: env.CITO_CODEX_BIN ?? "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
    hostId,
    hostName: (env.CITO_HOST_NAME ?? hostname()).replace(/[^\x20-\x7e]/g, "").slice(0, 128),
  };
}

/** Publish a complete host-id atomically with an exclusive hard-link creation. */
export async function getHostId(config: Pick<CitoConfig, "hostId" | "stateDir">): Promise<string> {
  if (config.hostId) return config.hostId;
  await mkdir(config.stateDir, { recursive: true, mode: 0o700 });
  await chmod(config.stateDir, 0o700);
  const path = join(config.stateDir, "host-id");
  try { return validateStoredId(await readFile(path, "utf8")); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  const temp = join(config.stateDir, ".host-id-" + randomUUID());
  const { link, unlink } = await import("node:fs/promises");
  const id = randomUUID();
  try {
    const file = await open(temp, "wx", 0o600);
    try {
      await file.writeFile(id + "\n");
      await file.sync();
    } finally { await file.close(); }
    try { await link(temp, path); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
  } finally { await unlink(temp).catch(error => {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }); }
  return validateStoredId(await readFile(path, "utf8"));
}
function validateStoredId(text: string): string {
  const id = text.trim();
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)) {
    throw new Error("Invalid persisted host-id; refusing to change machine identity");
  }
  return id;
}
