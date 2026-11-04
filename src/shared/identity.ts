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
  name?: unknown;
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
  const display = [claims.preferred_username, claims.email, claims.name, claims.sub]
    .find((value): value is string => typeof value === "string" && value.trim().length > 0)!;
  return {
    principal_id: normalizeIssuer(claims.iss) + "#" + claims.sub,
    owner_user: display.trim().toLowerCase(),
  };
}
export function localPrincipal(username?: string): Principal {
  if (username === undefined) {
    try { username = userInfo().username; } catch { username = "local"; }
  }
  return { principal_id: "local", owner_user: username || "local" };
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
