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
export interface ChannelAttachment {
  channel_id: string;
  kind: string;
  host_id: string;
  host_name: string;
  delivery_mode: "notify_fanout" | "wake_exclusive";
  priority: number;
  state: "ACTIVE" | "UNLISTENED";
  updated_at_ms: number;
}
export function evaluateLease(state: "ACTIVE" | "UNLISTENED" | null, brokerNow: string | Date | number, created?: string | Date | number): LeaseState {
  if (state !== "ACTIVE") return "UNLISTENED";
  if (created === undefined) throw new Error("ACTIVE lease requires a broker creation timestamp");
  const age = brokerAgeMs(brokerNow, created);
  return age < ACTIVE_LEASE_MS ? "ACTIVE" : age <= RETENTION_MS ? "DISCONNECTED_RETAINED" : "EXPIRED";
}
export function evaluateNodeLease(
  reg: { state: "ACTIVE" | "UNLISTENED" | null; channels?: Record<string, ChannelAttachment> },
  brokerNow: string | Date | number,
  created?: string | Date | number,
): LeaseState {
  if (reg.state !== "ACTIVE") return "UNLISTENED";
  const channelList = reg.channels ? Object.values(reg.channels) : [];
  if (channelList.length > 0) {
    let best: LeaseState = "UNLISTENED";
    const rank: Record<LeaseState, number> = { ACTIVE: 3, DISCONNECTED_RETAINED: 2, EXPIRED: 1, UNLISTENED: 0 };
    for (const ch of channelList) {
      const chLease = evaluateLease(ch.state, brokerNow, ch.updated_at_ms);
      if (rank[chLease] > rank[best]) best = chLease;
    }
    return best;
  }
  return evaluateLease(reg.state, brokerNow, created);
}
export function localExpirationMs(wallAtRequestStart: number, roundTripMs: number, brokerNow: string | Date | number, messageTimestamp: string | Date | number): number {
  if (!Number.isFinite(wallAtRequestStart) || !Number.isFinite(roundTripMs) || roundTripMs < 0) {
    throw new Error("Invalid expiration clock sample");
  }
  return wallAtRequestStart + Math.max(0, RETENTION_MS - brokerAgeMs(brokerNow, messageTimestamp) - Math.ceil(roundTripMs));
}
