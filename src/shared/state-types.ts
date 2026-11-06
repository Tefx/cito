export interface ListenerRow {
  hub: string; listener_id: string; public_id: string; tag: string | null;
  principal_id: string; owner_user: string; host_id: string; host_name: string;
  harness: string; session_id: string; endpoint_pid: number | null; cwd: string; repo: string | null;
  branch: string | null; title: string; about: string; scope: "self" | "workspace";
  status: "ACTIVE" | "REBINDING" | "UNLISTENED" | "FENCED_INACTIVE";
  binding_generation: number; owner_token: string; persisted_seq: number;
  consecutive_probe_failures: number; replay_gap_detected: number; replay_gap_range: string | null;
  dispatcher_token: string | null; lease_until_ms: number;
}
export interface SignalRow {
  hub: string; signal_id: string; listener_id: string; seq: number; binding_generation: number;
  from_user: string; from_host_name: string; from_harness: string | null; from_listener_id: string | null;
  from_title: string; from_about: string | null; message: string; held_reason: string | null; in_reply_to: string | null;
  created_at: string; expires_at_ms: number;
  state: "RECEIVED" | "DISPATCHING" | "DISPATCHED" | "STORED_PASSIVE" | "EXPIRED_UNSENT";
  batch_id: string | null; batch_expires_at_ms: number | null; dispatcher_token: string | null;
  queue_written: number; acknowledged_at_ms: number | null; batch_card: string | null;
}
export interface SentRow {
  hub: string; dedup_id: string; request_hash: string; signal_id: string; seq: number; to_listener_id: string;
  status: string; held_reason: string | null; expires_at_iso: string; expires_at_ms: number;
}
