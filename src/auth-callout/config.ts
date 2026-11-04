import { fromSeed, type KeyPair } from "nkeys.js";
import { loadConfig, type CitoConfig } from "../shared/config.js";

export interface AuthCalloutConfig extends CitoConfig {
  issuer: KeyPair;
  xkey?: KeyPair;
  serviceUser: string;
  servicePassword: string;
}
function seed(value: string | undefined, variable: string, prefix: "A" | "X"): KeyPair {
  if (!value) throw new Error(variable + " is required");
  try {
    const pair = fromSeed(new TextEncoder().encode(value));
    if (pair.getPublicKey()[0] !== prefix) {
      pair.clear();
      throw new Error("wrong key kind");
    }
    return pair;
  } catch {
    throw new Error(variable + (prefix === "A"
      ? " must be an Account seed (SA...); nats-server rejects Operator (SO...) callout issuers"
      : " must be a Curve25519 XKey seed (SX...)"));
  }
}
export function loadAuthCalloutConfig(env: NodeJS.ProcessEnv = process.env): AuthCalloutConfig {
  const base = loadConfig(env);
  const issuer = seed(env.CITO_AUTH_CALLOUT_ISSUER_SEED, "CITO_AUTH_CALLOUT_ISSUER_SEED", "A");
  try {
    const serviceUser = env.CITO_AUTH_CALLOUT_USER ?? "callout-service";
    if (serviceUser !== "callout-service") throw new Error("CITO_AUTH_CALLOUT_USER must be the exempted callout-service user");
    const servicePassword = env.CITO_AUTH_CALLOUT_PASSWORD;
    if (base.authMode === "oidc" && !servicePassword?.trim()) {
      throw new Error("CITO_AUTH_CALLOUT_PASSWORD is required in oidc mode");
    }
    const xkey = env.CITO_AUTH_CALLOUT_XKEY_SEED === undefined ? undefined
      : seed(env.CITO_AUTH_CALLOUT_XKEY_SEED, "CITO_AUTH_CALLOUT_XKEY_SEED", "X");
    return { ...base, issuer, xkey, serviceUser, servicePassword: servicePassword ?? "" };
  } catch (error) { issuer.clear(); throw error; }
}
