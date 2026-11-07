import { createHash } from "node:crypto";
import { userInfo } from "node:os";
import { basename } from "node:path";
import { CitoError } from "./errors.js";
import { validateHarness, hubNameSchema, type Harness } from "./validation.js";
import type { CitoConfig } from "./config.js";

export const sha256 = (text: string) => createHash("sha256").update(text).digest("hex");

export function isSingleDefaultHub(config: CitoConfig): boolean {
  const enabledNames = Object.entries(config.hubs)
    .filter(([, h]) => h.enabled !== false)
    .map(([name]) => name);
  const names = enabledNames.length > 0 ? enabledNames : Object.keys(config.hubs);
  return names.length === 1 && (config.defaultHub === "default" || config.defaultHub === "local");
}
export function formatScopedId(hub: string, id: string, config: CitoConfig): string {
  return isSingleDefaultHub(config) ? id : `${hub}:${id}`;
}
export function parseScopedId(raw: string, config: CitoConfig, explicitHub?: string): { hub: string; id: string } {
  const colon = raw.indexOf(":");
  const prefixHub = colon < 0 ? undefined : raw.slice(0, colon);
  const id = colon < 0 ? raw : raw.slice(colon + 1);
  for (const hub of [prefixHub, explicitHub]) {
    if (hub !== undefined && (!hubNameSchema.safeParse(hub).success || !Object.hasOwn(config.hubs, hub))) {
      throw new CitoError("INVALID_INPUT", `Unknown hub: ${hub}`);
    }
  }
  if (prefixHub !== undefined && explicitHub !== undefined && prefixHub !== explicitHub) {
    throw new CitoError("INVALID_INPUT", "Conflicting hub prefixes");
  }
  if (!id || id.includes(":")) throw new CitoError("INVALID_INPUT", "Invalid scoped ID");
  return { hub: prefixHub ?? explicitHub ?? config.defaultHub, id };
}

export interface Principal { principal_id: string; owner_user: string }
export interface NodeKey {
  principal_id: string;
  harness: Harness;
  session_id: string;
}
export interface SessionKey extends NodeKey {
  host_id?: string;
}
export function isHumanNode(x: { harness: string }): boolean {
  return x.harness === "human";
}
export function humanNodeKey(principal: Pick<Principal, "principal_id">): NodeKey {
  return { principal_id: principal.principal_id, harness: "human", session_id: "default" };
}
export interface SenderEndpoint {
  principal_id: string;
  host_id: string;
  harness: Harness | null;
  session_id: string | null;
}
export interface IdentityClaims {
  iss?: unknown;
  sub?: unknown;
  preferred_username?: unknown;
  [claim: string]: unknown;
}
export function normalizeIssuer(issuer: string): string {
  return issuer.replace(/\/+$/, "");
}
/** Requires cryptographically verified claims. This function does no JWT verification. */
export function oidcPrincipal(claims: IdentityClaims): Principal {
  if (typeof claims.iss !== "string" || !normalizeIssuer(claims.iss).trim()
    || typeof claims.sub !== "string" || !claims.sub.trim()) {
    throw new CitoError("UNAUTHORIZED", "OIDC identity requires iss and sub");
  }
  const preferred = typeof claims.preferred_username === "string" ? claims.preferred_username.trim() : "";
  const mailRaw = claims["em" + "ail"];
  const mail = typeof mailRaw === "string" ? mailRaw.trim() : "";
  const mailLocal = mail.includes("@") ? mail.slice(0, mail.indexOf("@")).trim() : "";
  const display = preferred || mailLocal || claims.sub;
  return {
    principal_id: normalizeIssuer(claims.iss) + "#" + claims.sub,
    owner_user: display.toLowerCase().replace(/[^a-z0-9._-]/g, "_").slice(0, 64),
  };
}
export function localPrincipal(username: string = process.env.CITO_USER ?? userInfo().username): Principal {
  const owner_user = username.toLowerCase();
  if (owner_user.length < 1 || owner_user.length > 64 || /[^a-z0-9._-]/.test(owner_user)) {
    throw new CitoError("INVALID_INPUT", "Local owner_user must match ^[a-z0-9._-]{1,64}$");
  }
  return { principal_id: "none#" + owner_user, owner_user };
}
export function senderEndpointKey(endpoint: SenderEndpoint): string {
  if (endpoint.harness === null && endpoint.session_id === null) {
    return endpoint.principal_id + "|" + endpoint.host_id + "|unbound|default";
  }
  validateHarness(endpoint.harness);
  if (!endpoint.session_id) throw new CitoError("INVALID_INPUT", "Bound sender requires session_id");
  return [endpoint.principal_id, endpoint.host_id, endpoint.harness, endpoint.session_id].join("|");
}
export function normalizeSlug(raw: string): string {
  return raw.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "")
    .slice(0, 20).replace(/-+$/g, "");
}
export interface ListenerMetadata {
  tag?: string | null; repo: string | null; cwd: string; title: string; harness: Harness;
}
/** A stored explicit tag outranks metadata; empty normalized candidates are skipped. */
export function listenerSlug(value: ListenerMetadata): string {
  return [value.tag, value.repo?.split("/").at(-1), basename(value.cwd), value.title, value.harness]
    .map(raw => normalizeSlug(raw ?? "")).find(Boolean)!;
}
export function publicListenerId(slug: string, canonicalId: string): string {
  return `${slug}-${canonicalId.slice(2)}`;
}
export function toCanonicalListenerId(input: string): string {
  if (/^c_[a-z2-7]{12}$/.test(input)) return input;
  const match = /^[a-z0-9]+(?:-[a-z0-9]+)*-([a-z2-7]{12})$/.exec(input);
  if (match) return "c_" + match[1];
  throw new CitoError("INVALID_INPUT", "Invalid listener ID");
}
export function listenerId(key: SessionKey): string {
  validateHarness(key.harness);
  const raw = "cito-v2|" + [key.principal_id, key.harness, key.session_id].join("|");
  const digest = createHash("sha256").update(raw, "utf8").digest().subarray(0, 8);
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  let bits = 0, value = 0, encoded = "";
  for (const byte of digest) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      encoded += alphabet[(value >>> bits) & 31];
    }
  }
  if (bits) encoded += alphabet[(value << (5 - bits)) & 31];
  return "c_" + encoded.slice(0, 12);
}
