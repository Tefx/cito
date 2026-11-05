import { hostname, homedir, userInfo } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { mkdir, open, readFile, chmod } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { z } from "zod";
import { normalizeIssuer } from "./identity.js";
import { CitoError } from "./errors.js";
import { hubNameSchema } from "./validation.js";

export interface HubConfig {
  name: string;
  natsUrl: string;
  authMode: "none" | "oidc";
  user: string;
  oidcIssuer?: string;
  oidcClientId?: string;
  oidcScopes?: string[];
  oidcRedirectPort?: number;
}
export interface CitoConfig extends Omit<HubConfig, "name"> {
  stateDir: string;
  daemonPort: number;
  /** Deterministic desktop liveness injection for embedders and tests. */
  codexDesktopRunning?: (signal: AbortSignal) => Promise<boolean>;
  codexHome: string;
  codexBin: string;
  openBin: string;
  hostName: string;
  defaultHub: string;
  hubs: Record<string, HubConfig>;
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
function validateHub(value: HubConfig): HubConfig {
  const { natsUrl, authMode, user, oidcClientId } = value;
  let url: URL;
  try { url = new URL(natsUrl); } catch { throw new Error("CITO_NATS_URL must be a valid NATS or WebSocket URL"); }
  if (!["nats:", "ws:", "wss:"].includes(url.protocol) || !url.hostname
    || url.username || url.password || url.hash || url.search
    || (url.protocol === "nats:" && url.pathname && url.pathname !== "/")) {
    throw new Error("CITO_NATS_URL must use nats://, ws:// or wss:// without credentials");
  }
  if (authMode !== "none" && authMode !== "oidc") throw new Error("CITO_AUTH_MODE must be none or oidc");
  const oidcIssuer = value.oidcIssuer === undefined ? undefined
    : normalizeIssuer(validateHttpUrl(value.oidcIssuer, "CITO_OIDC_ISSUER"));
  if (authMode === "oidc" && !oidcIssuer) throw new Error("CITO_OIDC_ISSUER is required in oidc mode");
  if ((authMode === "oidc" || oidcIssuer !== undefined) && !oidcClientId?.trim()) {
    throw new Error("CITO_OIDC_CLIENT_ID is required with an OIDC issuer");
  }
  if (oidcClientId !== undefined && (oidcClientId !== oidcClientId.trim() || !oidcClientId)) {
    throw new Error("CITO_OIDC_CLIENT_ID must be a non-empty trimmed string");
  }
  if (user.length < 1 || user.length > 64 || /[^a-zA-Z0-9._-]/.test(user)) {
    throw new Error("CITO_USER must contain 1–64 letters, numbers, dots, underscores or hyphens");
  }
  if (value.oidcRedirectPort !== undefined) port(value.oidcRedirectPort, "oidcRedirectPort");
  return { ...value, oidcIssuer };
}
function port(value: number, name: string): number {
  if (!Number.isInteger(value) || value < 1 || value > 65535) throw new Error(name + " must be an integer in 1..65535");
  return value;
}
export function loadConfig(env: NodeJS.ProcessEnv = process.env): CitoConfig {
  for (const key of ["CITO_STATE_DIR", "CITO_CODEX_HOME", "CITO_CODEX_BIN", "CITO_OPEN_BIN", "CITO_CONFIG_PATH"] as const) {
    if (env[key] !== undefined && !env[key]?.trim()) throw new Error(key + " must not be empty");
  }
  const stateDir = directory(env.CITO_STATE_DIR ?? "~/.cito");
  const oidcRedirectPort = env.CITO_OIDC_REDIRECT_PORT === undefined ? undefined : port(Number(env.CITO_OIDC_REDIRECT_PORT), "CITO_OIDC_REDIRECT_PORT");
  const path = env.CITO_CONFIG_PATH ?? join(stateDir, "config.json");
  const entrySchema = z.object({ natsUrl: z.string(), authMode: z.enum(["none", "oidc"]).default("none"),
    user: z.string().default(env.CITO_USER ?? userInfo().username), oidcIssuer: z.string().optional(),
    oidcClientId: z.string().optional(), oidcScopes: z.array(z.string().min(1)).optional(),
    oidcRedirectPort: z.number().int().min(1).max(65535).optional(), redirect_port: z.number().int().min(1).max(65535).optional() });
  let json: { hubs: Record<string, z.infer<typeof entrySchema>>; default_hub?: string; daemonPort?: number } | undefined;
  if ((env === process.env || env.CITO_STATE_DIR !== undefined || env.CITO_CONFIG_PATH !== undefined)
    && (env.CITO_CONFIG_PATH !== undefined || existsSync(path))) {
    try {
      json = z.object({ hubs: z.record(hubNameSchema, entrySchema).refine(v => Object.keys(v).length > 0),
        default_hub: hubNameSchema.optional(), daemonPort: z.number().int().min(1).max(65535).optional() }).parse(JSON.parse(readFileSync(path, "utf8")));
    } catch (cause) { throw new CitoError("INVALID_INPUT", "Cannot load hub configuration: " + path, { cause }); }
  }
  const hubs: Record<string, HubConfig> = Object.create(null);
  if (json) for (const [name, value] of Object.entries(json.hubs)) hubs[name] = validateHub({ name, ...value, oidcRedirectPort: value.oidcRedirectPort ?? value.redirect_port });
  else hubs.default = validateHub({ name: "default", natsUrl: env.CITO_NATS_URL ?? "nats://127.0.0.1:4222",
    authMode: (env.CITO_AUTH_MODE ?? "none") as HubConfig["authMode"], user: env.CITO_USER ?? userInfo().username,
    oidcIssuer: env.CITO_OIDC_ISSUER, oidcClientId: env.CITO_OIDC_CLIENT_ID,
    oidcScopes: env.CITO_OIDC_SCOPES?.split(/\s+/).filter(Boolean),
    oidcRedirectPort });
  const defaultHub = env.CITO_HUB?.trim() || json?.default_hub || (Object.keys(hubs).length === 1 ? Object.keys(hubs)[0] : undefined);
  if (!defaultHub || !Object.hasOwn(hubs, defaultHub)) {
    throw new CitoError("INVALID_INPUT", "default_hub must be specified and exist in configured hubs");
  }
  if (env.CITO_NATS_URL !== undefined && env.CITO_CONFIG_PATH === undefined) {
    hubs[defaultHub] = validateHub({ ...hubs[defaultHub]!, natsUrl: env.CITO_NATS_URL });
  }
  const { name: _name, ...mirror } = hubs[defaultHub]!;
  return {
    ...mirror, hubs, defaultHub, stateDir,
    daemonPort: port(env.CITO_DAEMON_PORT === undefined ? json?.daemonPort ?? 4290 : Number(env.CITO_DAEMON_PORT), "CITO_DAEMON_PORT"),
    codexHome: directory(env.CITO_CODEX_HOME ?? "~/.codex"),
    codexBin: env.CITO_CODEX_BIN ?? "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
    openBin: env.CITO_OPEN_BIN ?? "/usr/bin/open",
    hostName: (env.CITO_HOST_NAME ?? hostname()).replace(/[^\x20-\x7e]/g, "").slice(0, 128),
  };
}

/** Publish a complete host-id atomically with an exclusive hard-link creation. */
export async function getHostId(config: Pick<CitoConfig, "stateDir">): Promise<string> {
  await mkdir(config.stateDir, { recursive: true, mode: 0o700 });
  await chmod(config.stateDir, 0o700);
  const path = join(config.stateDir, "host-id");
  try {
    const id = validateStoredId(await readFile(path, "utf8"));
    await chmod(path, 0o600);
    return id;
  }
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
