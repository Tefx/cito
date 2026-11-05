import type { CitoConfig } from "../shared/index.js";
import type { ListenerRow } from "../shared/state-types.js";
import type { RepoMetadata } from "./git.js";

export interface Caller extends RepoMetadata { harness: string; session_id: string; cwd: string; endpoint_pid?: number }
export interface AvailableSessionSummary {
  harness: string;
  session_id: string;
  title: string;
  cwd: string;
  branch?: string | null;
  endpoint_pid?: number;
  updated_at_ms?: number;
}
export interface CachedSessionQuery { harness?: string; sessionId?: string; endpointPid?: number }
export interface CallerDetectionContext {
  meta: Record<string, unknown>;
  headers?: Record<string, string | string[] | undefined>;
  httpClientPid?: number;
  httpTransport?: boolean;
  requireOpen?: boolean;
  lookupCachedSession?: (query: CachedSessionQuery) => Caller | undefined | Promise<Caller | undefined>;
}
export type CallerInput = CallerDetectionContext | Record<string, unknown>;

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
  readonly canJump?: boolean;
  detectExplicitCaller?(ctx: CallerDetectionContext, config: CitoConfig): Promise<Caller | null>;
  discoverAvailableSessions?(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]>;
  probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome>;
  deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome>;
  updateBadge?(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void>;
}
