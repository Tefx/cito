import type { CitoConfig } from "../shared/index.js";
import type { ListenerRow } from "../shared/state-types.js";

export type ProbeOutcome =
  | { status: "alive" | "switched" | "closed_definitive" }
  | { status: "transient_error"; error: unknown };
export type DeliverOutcome = "delivered" | "delivered_unconfirmed" | "driver_unavailable" | "session_switched";
export interface DeliverContext {
  batchId: string;
  card: string;
  queueWritten: boolean;
  markQueueWritten: () => void;
  runEffect: (submit: () => Promise<void>, opts: {
    attempts: number; pause: number[]; onSigtermTimeout?: () => void;
  }) => Promise<boolean>;
  config: CitoConfig;
  signal: AbortSignal;
}
export interface HarnessAdapter {
  readonly id: string;
  probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome>;
  deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome>;
  updateBadge?(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void>;
}
