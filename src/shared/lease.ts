const ACTIVE_LEASE_MS = 60_000;
export const RETENTION_MS = 1_800_000;
function timestamp(value: string | Date | number): number {
  const result = typeof value === "number" ? value : value instanceof Date ? value.getTime() : Date.parse(value);
  if (!Number.isFinite(result)) throw new Error("Invalid broker timestamp");
  return result;
}
export function brokerAgeMs(brokerNow: string | Date | number, created: string | Date | number): number {
  return Math.max(0, timestamp(brokerNow) - timestamp(created));
}
export type LeaseState = "ACTIVE" | "DISCONNECTED_RETAINED" | "EXPIRED" | "UNLISTENED";
export function evaluateLease(state: "ACTIVE" | "UNLISTENED" | null, brokerNow: string | Date | number, created?: string | Date | number): LeaseState {
  if (state !== "ACTIVE") return "UNLISTENED";
  if (created === undefined) throw new Error("ACTIVE lease requires a broker creation timestamp");
  const age = brokerAgeMs(brokerNow, created);
  return age < ACTIVE_LEASE_MS ? "ACTIVE" : age <= RETENTION_MS ? "DISCONNECTED_RETAINED" : "EXPIRED";
}
export function localExpirationMs(wallAtRequestStart: number, roundTripMs: number, brokerNow: string | Date | number, messageTimestamp: string | Date | number): number {
  if (!Number.isFinite(wallAtRequestStart) || !Number.isFinite(roundTripMs) || roundTripMs < 0) {
    throw new Error("Invalid expiration clock sample");
  }
  return wallAtRequestStart + Math.max(0, RETENTION_MS - brokerAgeMs(brokerNow, messageTimestamp) - Math.ceil(roundTripMs));
}
