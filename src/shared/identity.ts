import { createHash } from "node:crypto";
import { userInfo } from "node:os";
import { CitoError } from "./errors.js";
import { validateHarness, type Harness } from "./validation.js";

export interface Principal { principal_id: string; owner_user: string }
export interface SessionKey {
  principal_id: string;
  host_id: string;
  harness: Harness;
  session_id: string;
}
export type SenderEndpoint = Omit<SessionKey, "harness" | "session_id"> & {
  harness: Harness | null;
  session_id: string | null;
};
export interface IdentityClaims {
  iss?: unknown;
  sub?: unknown;
  preferred_username?: unknown;
  email?: unknown;
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
  const email = typeof claims.email === "string" ? claims.email.trim() : "";
  const emailLocal = email.includes("@") ? email.slice(0, email.indexOf("@")).trim() : "";
  const display = preferred || emailLocal || claims.sub;
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
export function listenerId(key: SessionKey): string {
  validateHarness(key.harness);
  const raw = "cito-v1|" + [key.principal_id, key.host_id, key.harness, key.session_id].join("|");
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
